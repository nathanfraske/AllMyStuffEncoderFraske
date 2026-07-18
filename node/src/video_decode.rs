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

/// One access unit headed for a route's decoder (H.264 or HEVC — the
/// stream declares itself; see [`sniff_codec`]).
pub struct Au {
    /// Presentation time in µs (from the RTP clock).
    pub ts_us: u64,
    /// Whether the daemon flagged this unit a decode entry (IDR).
    pub key: bool,
    /// Annex-B bytes.
    pub data: Vec<u8>,
}

/// Which codec an Annex-B access unit opens with, judged from its first
/// NAL header byte — and only for *parameter-set-led* units (the decode
/// entries both our encoders emit with repeated VPS/SPS/PPS), where the
/// byte values are unambiguous: HEVC VPS/SPS/PPS read as H.264 types
/// 0/2/4, which no H.264 key unit leads with, and H.264 SPS/PPS/IDR read
/// as HEVC types 19/51/50/etc. outside the parameter-set range. Delta
/// units return `None` — the stream's codec is a property carried from
/// key to key, not re-judged per frame.
#[derive(Clone, Copy, PartialEq)]
enum AuCodec {
    H264,
    Hevc,
}

fn sniff_codec(data: &[u8]) -> Option<AuCodec> {
    let mut i = 0usize;
    let b = loop {
        if i + 3 >= data.len() {
            return None;
        }
        if data[i] == 0 && data[i + 1] == 0 {
            if data[i + 2] == 1 {
                break data[i + 3];
            }
            if data[i + 2] == 0 && i + 4 < data.len() && data[i + 3] == 1 {
                break data[i + 4];
            }
        }
        i += 1;
    };
    // Exact bytes, not masked types: HEVC VPS/SPS/PPS at layer 0 are
    // precisely 0x40/0x42/0x44. A masked `(b>>1)&0x3F == 32` test would
    // also catch 0x41 — an H.264 P slice with nal_ref_idc 2, the byte
    // most delta AUs lead with — and flip a healthy H.264 stream's
    // decoder on every frame. (Caught in review; the byte-exact match is
    // collision-free because H.264 types 0/2/4 never lead an AU.)
    match b {
        0x40 | 0x42 | 0x44 => Some(AuCodec::Hevc), // VPS · SPS · PPS
        _ => match b & 0x1F {
            5 | 7 | 8 => Some(AuCodec::H264), // IDR · SPS · PPS
            _ => None,
        },
    }
}

/// Pending AUs per route before the overflow dump. Kept short (~200 ms at
/// 60 fps) so a decoder that stalls dumps to the next keyframe fast instead of
/// playing seconds of stale, latency-inducing backlog — a healthy decoder (a
/// few ms per frame) never queues anywhere near this.
const MAX_PENDING: usize = 12;

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

    // The decode thread is the viewer's media plane — same priority/EcoQoS
    // treatment as the host's capture and encode threads, so a loaded
    // viewer box doesn't stutter the picture it's watching.
    crate::os_perf::boost_media_thread();

    // The route's decoder, whichever codec the stream declared at its
    // last key unit. H.264 = software openh264 (every platform); HEVC =
    // NVDEC (Windows + NVIDIA — the posture negotiation only offers HEVC
    // where this rung exists, so the unavailable arm is a guard, not a
    // path).
    enum Active {
        H264(Decoder),
        #[cfg(all(windows, feature = "host"))]
        Hevc(crate::nvdec::NvdecHevc),
    }
    let mut decoder: Option<Active> = None;
    let mut stream_codec = AuCodec::H264;
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
        // A parameter-set-led unit is a decode entry in both codecs and
        // carries the stream's codec declaration — trusted over `au.key`,
        // whose daemon-side derivation is H.264-shaped and blind to HEVC.
        let sniffed = sniff_codec(&au.data);
        let is_key = au.key || sniffed.is_some();
        if waiting_key && !is_key {
            continue;
        }
        if let Some(c) = sniffed {
            if stream_codec != c {
                // Codec morph mid-route (posture change): the old decoder
                // has nothing valid to say about the new stream.
                stream_codec = c;
                decoder = None;
            }
        }
        let dec = match &mut decoder {
            Some(d) => d,
            None => {
                let built = match stream_codec {
                    AuCodec::H264 => Decoder::with_api_config(
                        openh264::OpenH264API::from_source(),
                        DecoderConfig::new(),
                    )
                    .map(Active::H264)
                    .map_err(|e| format!("openh264: {e}")),
                    #[cfg(all(windows, feature = "host"))]
                    AuCodec::Hevc => crate::nvdec::NvdecHevc::open().map(Active::Hevc),
                    #[cfg(not(all(windows, feature = "host")))]
                    AuCodec::Hevc => Err("no HEVC decoder on this platform".to_string()),
                };
                match built {
                    Ok(d) => decoder.insert(d),
                    Err(e) => {
                        // Init trouble is permanent for this stream — say
                        // so once a window, keep draining so the route
                        // isn't backed up behind us.
                        if last_err.is_none_or(|t| t.elapsed() >= STATS_EVERY) {
                            last_err = Some(Instant::now());
                            tracing::warn!("decoder init for {route_id} failed: {e}");
                        }
                        continue;
                    }
                }
            }
        };
        let t0 = Instant::now();
        let mut broke: Option<String> = None;
        match dec {
            Active::H264(dec) => match dec.decode(&au.data) {
                Ok(picture) => {
                    // A key unit that decoded clean re-arms the stream
                    // even if this call produced no picture (headers-only
                    // AU): the reference frame now lives in the decoder.
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
                Err(e) => broke = Some(format!("H.264: {e}")),
            },
            #[cfg(all(windows, feature = "host"))]
            Active::Hevc(dec) => match dec.decode(&au.data, au.ts_us) {
                Ok(pics) => {
                    waiting_key = false;
                    for f in pics {
                        let (w, h) = (f.width as usize, f.height as usize);
                        if w == 0 || h == 0 {
                            continue;
                        }
                        let mut packet = raw_ipc_packet(f.ts_us, f.width, f.height);
                        crate::nvdec::nv12_to_rgba(
                            &f.nv12,
                            w,
                            h,
                            &mut packet[crate::mesh::VIDEO_IPC_HEADER_LEN..],
                        );
                        spent += t0.elapsed();
                        frames += 1;
                        out_dims = (w, h);
                        on_frame(packet);
                    }
                }
                Err(e) => broke = Some(format!("HEVC: {e}")),
            },
        }
        if let Some(e) = broke {
            // Corrupt bitstream (a lost unit upstream): drop the decoder,
            // re-enter at the next IDR. Rate-limited — at frame rate this
            // would otherwise be a log flood.
            if last_err.is_none_or(|t| t.elapsed() >= STATS_EVERY) {
                last_err = Some(Instant::now());
                tracing::warn!("video decode for {route_id} failed ({e}); awaiting a key unit");
            }
            decoder = None;
            waiting_key = true;
            on_glitch();
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

    /// HEVC through the whole bridge on the real hardware rungs: NVENC
    /// lossless AUs fed with `key: false` on purpose — the daemon's key
    /// flag is H.264-shaped and must never be load-bearing for HEVC; the
    /// sniff carries the entry. 640×360 codes with CTB padding (384
    /// rows), so the display crop is exercised too. Skips (passing)
    /// without the NVIDIA rungs.
    #[cfg(all(windows, feature = "host"))]
    #[test]
    fn hevc_stream_decodes_through_bridge() {
        let (w, h) = (640u32, 360u32);
        let (wu, hu) = (w as usize, h as usize);
        let mut gpu = match crate::gpu_pipeline::GpuConvert::new(w, h, w, h) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("SKIP: GPU convert unavailable: {e}");
                return;
            }
        };
        let mut enc =
            match crate::nvenc::NvencH264::open_lossless_hevc_on_device(&gpu.device(), w, h, 60) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("SKIP: NVENC HEVC unavailable: {e}");
                    return;
                }
            };
        // Availability probe, held open through the test: paying cuInit
        // here keeps the bridge thread's lazy open fast, the same warm
        // state a live viewer reaches after its first session.
        let _warm = match crate::nvdec::NvdecHevc::open() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("SKIP: NVDEC unavailable: {e}");
                return;
            }
        };
        let bridge = DecodeBridge::new();
        let got = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let mut bgra = vec![0u8; wu * hu * 4];
        let tex = gpu.bgra_texture_from(&bgra, w, h).expect("tex");
        for i in 0..30u64 {
            for (j, v) in bgra.iter_mut().enumerate() {
                *v = ((j as u64).wrapping_add(i * 11) % 251) as u8;
            }
            gpu.update_bgra(&tex, &bgra, w, h);
            let (slot, nv12) = gpu.convert(&tex).expect("convert").expect("slot");
            // Periodic IDRs, like the live stream's adaptive cadence: if
            // the bridge's bounded queue ever dumps (slow first open),
            // the stream carries its own re-entry points.
            let out = enc
                .encode_texture(&nv12, i.is_multiple_of(10))
                .expect("encode");
            gpu.release(slot);
            for (d, _) in out.units {
                let sink = got.clone();
                bridge.feed(
                    "route-hevc",
                    Au {
                        ts_us: i * 16_667,
                        key: false,
                        data: d,
                    },
                    move |p| sink.lock().push(p),
                    || {},
                );
            }
            // Stay under the bounded queue — the decoder runs ~1 ms/frame.
            std::thread::sleep(Duration::from_millis(4));
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while got.lock().len() < 30 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        bridge.stop("route-hevc");
        let packets = got.lock();
        // ≥18: allows one dumped queue window at start-up (≤12 units)
        // healed by the next periodic key — the bridge's designed
        // recovery — while still proving a sustained decoded stream.
        assert!(packets.len() >= 18, "decoded packets: {}", packets.len());
        let expect = crate::mesh::VIDEO_IPC_HEADER_LEN + wu * hu * 4;
        assert!(packets.iter().all(|p| p.len() == expect), "packet shape");
        let pw = u32::from_le_bytes(packets[0][4..8].try_into().unwrap());
        let ph = u32::from_le_bytes(packets[0][8..12].try_into().unwrap());
        assert_eq!((pw, ph), (w, h), "display-cropped dimensions");
        let body = &packets[5][crate::mesh::VIDEO_IPC_HEADER_LEN..];
        assert!(body.iter().any(|&b| b > 8), "pixels arrived");
    }
}
