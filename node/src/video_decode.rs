//! The receive half of the H.264 path: a native openh264 decoder per
//! inbound display route, for console windows whose webview can't decode
//! (no WebCodecs — Linux WebKitGTK today) or whose WebCodecs decoder
//! stalled out. It turns access units into ready-to-paint RGBA frames the
//! window blits with `putImageData` — so H.264's bandwidth and 1920-edge
//! sharpness no longer depend on the webview, and the MJPEG fallback is
//! reserved for genuinely old peers instead of half our own platforms.
//!
//! Mirrors [`crate::video::VideoBridge`]'s shape: one dedicated thread per
//! route, owning the decoder state, fed by a bounded channel. H.264 deltas
//! chain, so the queue is never thinned mid-stream — when it overflows
//! (a wedged consumer), everything is dropped at once and decoding resumes
//! at the sender's next IDR (≤2 s away), exactly the recovery the webview
//! decoder uses. Decoded frames are handed on freshest-wins: the consumer
//! holds at most one undrained picture, which is what "minimum latency"
//! means at a pull-based sink.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// One H.264 access unit headed for a route's decoder.
pub struct Au {
    /// Presentation time in µs (from the RTP clock).
    pub ts_us: u64,
    /// Whether the daemon flagged this unit a decode entry (IDR).
    pub key: bool,
    /// Annex-B bytes.
    pub data: Vec<u8>,
}

/// Pending AUs per route before the overflow dump — ~2 s at 30 fps, far
/// more than a healthy decoder (a few ms per frame) ever queues.
const MAX_PENDING: usize = 64;

/// How often each decoder logs its dial-in line (matches the encode side).
const STATS_EVERY: Duration = Duration::from_secs(5);

struct RouteDecode {
    tx: mpsc::SyncSender<Au>,
    /// Set on queue overflow; the thread dumps to the next key unit.
    need_key: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for RouteDecode {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[derive(Default)]
pub struct DecodeBridge {
    routes: Mutex<HashMap<String, RouteDecode>>,
}

impl DecodeBridge {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one access unit to `route_id`'s decoder, starting it on first
    /// use. `on_frame` receives each decoded picture as a ready IPC packet
    /// (see [`raw_ipc_packet`]); `on_glitch` fires when the decoder loses
    /// its place (corrupt unit, dumped backlog) so the caller can ask the
    /// sender to re-key. Both are only captured when this call starts the
    /// thread. A full queue dumps wholesale and re-keys — see module docs.
    pub fn feed<F, G>(&self, route_id: &str, au: Au, on_frame: F, on_glitch: G)
    where
        F: Fn(Vec<u8>) + Send + 'static,
        G: Fn() + Send + 'static,
    {
        let mut routes = self.routes.lock();
        let entry = routes.entry(route_id.to_string()).or_insert_with(|| {
            let (tx, rx) = mpsc::sync_channel::<Au>(MAX_PENDING);
            let need_key = Arc::new(AtomicBool::new(false));
            let stop = Arc::new(AtomicBool::new(false));
            let id = route_id.to_string();
            let (nk, st) = (need_key.clone(), stop.clone());
            let thread =
                std::thread::spawn(move || run_decode(&st, &nk, &id, rx, on_frame, on_glitch));
            tracing::info!("native H.264 decoder started for {route_id}");
            RouteDecode {
                tx,
                need_key,
                stop,
                thread: Some(thread),
            }
        });
        if entry.tx.try_send(au).is_err() {
            // Queue full (or thread gone): deltas past this point are
            // useless without their predecessors — flag a re-key; the
            // thread dumps what's queued when it sees the flag.
            entry.need_key.store(true, Ordering::SeqCst);
        }
    }

    /// Whether `route_id` currently has a live decoder.
    #[cfg(test)]
    pub fn is_running(&self, route_id: &str) -> bool {
        self.routes.lock().contains_key(route_id)
    }

    pub fn stop(&self, route_id: &str) {
        if self.routes.lock().remove(route_id).is_some() {
            // The start line names the decode path in use; the stop is
            // routine teardown (every tab switch in native mode).
            tracing::debug!("native H.264 decoder stopped for {route_id}");
        }
    }
}

fn run_decode<F, G>(
    stop: &AtomicBool,
    need_key: &AtomicBool,
    route_id: &str,
    rx: mpsc::Receiver<Au>,
    on_frame: F,
    on_glitch: G,
) where
    F: Fn(Vec<u8>),
    G: Fn(),
{
    use openh264::decoder::{Decoder, DecoderConfig};
    use openh264::formats::YUVSource as _;

    let mut decoder: Option<Decoder> = None;
    // Decode entry is a key unit; deltas before one can't decode.
    let mut waiting_key = true;
    let mut last_err: Option<Instant> = None;
    let (mut frames, mut spent, mut out_dims, mut since) =
        (0u32, Duration::ZERO, (0usize, 0usize), Instant::now());

    while !stop.load(Ordering::SeqCst) {
        // A bounded wait keeps the stop flag responsive on a quiet stream.
        let au = match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(au) => au,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if need_key.swap(false, Ordering::SeqCst) {
            // The feeder overflowed: drain the stale backlog and wait for
            // the sender's next IDR — same recovery as a decode error.
            while rx.try_recv().is_ok() {}
            waiting_key = true;
            on_glitch();
        }
        if waiting_key && !au.key {
            continue;
        }
        let dec = match &mut decoder {
            Some(d) => d,
            None => match Decoder::with_api_config(
                openh264::OpenH264API::from_source(),
                DecoderConfig::new(),
            ) {
                Ok(d) => decoder.insert(d),
                Err(e) => {
                    // Init trouble is permanent for this stream — say so
                    // once a window, keep draining so the route isn't
                    // backed up behind us.
                    if last_err.is_none_or(|t| t.elapsed() >= STATS_EVERY) {
                        last_err = Some(Instant::now());
                        tracing::warn!("openh264 decoder init for {route_id} failed: {e}");
                    }
                    continue;
                }
            },
        };
        let t0 = Instant::now();
        match dec.decode(&au.data) {
            Ok(picture) => {
                // A key unit that decoded clean re-arms the stream even if
                // this call produced no picture (headers-only AU): the
                // reference frame now lives in the decoder.
                waiting_key = false;
                if let Some(yuv) = picture {
                    let (w, h) = yuv.dimensions();
                    if w == 0 || h == 0 {
                        continue;
                    }
                    let mut packet = raw_ipc_packet(au.ts_us, w as u32, h as u32);
                    yuv.write_rgba8(&mut packet[crate::mesh::VIDEO_IPC_HEADER_LEN..]);
                    spent += t0.elapsed();
                    frames += 1;
                    out_dims = (w, h);
                    on_frame(packet);
                }
            }
            Err(e) => {
                // Corrupt bitstream (a lost unit upstream): drop the
                // decoder, re-enter at the next IDR. Rate-limited — at
                // frame rate this would otherwise be a log flood.
                if last_err.is_none_or(|t| t.elapsed() >= STATS_EVERY) {
                    last_err = Some(Instant::now());
                    tracing::warn!("H.264 decode for {route_id} failed ({e}); awaiting a key unit");
                }
                decoder = None;
                waiting_key = true;
                on_glitch();
            }
        }
        let elapsed = since.elapsed();
        if elapsed >= STATS_EVERY && frames > 0 {
            let line = format!(
                "video decode {route_id}: {:.1} fps · {:.1} ms/frame · {}×{} (native)",
                frames as f64 / elapsed.as_secs_f64(),
                spent.as_secs_f64() * 1000.0 / frames as f64,
                out_dims.0,
                out_dims.1,
            );
            if crate::video::stats_to_info() {
                tracing::info!("{line}");
            } else {
                tracing::debug!("{line}");
            }
            (frames, spent, since) = (0, Duration::ZERO, Instant::now());
        }
    }
}

/// An IPC packet (kind 3 — see the wire-format comment in `mesh.rs`) with
/// the RGBA payload area zeroed, sized for `w`×`h`. The decoder writes its
/// pixels straight into the tail, so the only copy on the decode path is
/// the YUV→RGBA conversion itself.
fn raw_ipc_packet(ts_us: u64, w: u32, h: u32) -> Vec<u8> {
    let len = (w as usize) * (h as usize) * 4;
    let mut out = crate::mesh::video_ipc_header(3, 0, [w, h, 0, 0], ts_us, len);
    out.resize(crate::mesh::VIDEO_IPC_HEADER_LEN + len, 0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a couple of frames with the encoder the send side uses, feed
    /// them through the bridge, and check real RGBA frames come out — the
    /// whole loop the two ends of a route rely on, no hardware involved.
    #[test]
    fn decodes_what_the_encoder_produces() {
        use openh264::encoder::Encoder;
        use openh264::formats::{RgbSliceU8, YUVBuffer};

        let mut enc = Encoder::with_api_config(
            openh264::OpenH264API::from_source(),
            openh264::encoder::EncoderConfig::new(),
        )
        .expect("encoder");
        let bridge = DecodeBridge::new();
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();

        for shade in [40u8, 200u8] {
            let rgb = vec![shade; 64 * 64 * 3];
            let yuv = YUVBuffer::from_rgb8_source(RgbSliceU8::new(&rgb, (64, 64)));
            let stream = enc.encode(&yuv).expect("encode");
            let data = stream.to_vec();
            if data.is_empty() {
                continue;
            }
            let tx = tx.clone();
            bridge.feed(
                "r1",
                Au {
                    ts_us: 0,
                    key: shade == 40, // first unit out of a fresh encoder is the IDR
                    data,
                },
                move |packet| {
                    let _ = tx.send(packet);
                },
                || {},
            );
        }

        let packet = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("a decoded frame");
        assert_eq!(packet[0], 3, "kind 3 = raw RGBA");
        let w = u32::from_le_bytes(packet[4..8].try_into().unwrap());
        let h = u32::from_le_bytes(packet[8..12].try_into().unwrap());
        assert_eq!((w, h), (64, 64));
        assert_eq!(
            packet.len(),
            crate::mesh::VIDEO_IPC_HEADER_LEN + 64 * 64 * 4
        );
        // Alpha is opaque all the way through (the canvas blits it as-is).
        assert_eq!(packet[crate::mesh::VIDEO_IPC_HEADER_LEN + 3], 255);
        assert!(bridge.is_running("r1"));
        bridge.stop("r1");
        assert!(!bridge.is_running("r1"));
    }
}
