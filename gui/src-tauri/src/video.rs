//! The display media plane: MJPEG capture of this machine's screen, so an
//! active display route actually streams pixels — the piKVM transport
//! (every frame a standalone JPEG; losing one costs one frame and there's
//! no codec state to desync).
//!
//! Mirrors [`crate::audio::AudioBridge`]'s shape: each sourcing route runs
//! a dedicated thread that captures the **primary monitor**, downscales to
//! a sane streaming size, JPEG-encodes, and hands the frame to a callback
//! the mesh forwards on the media channel.
//!
//! Capture prefers a **persistent session** (`xcap`'s `VideoRecorder`:
//! PipeWire ScreenCast on Wayland, DXGI duplication on Windows,
//! AVFoundation on macOS): the OS negotiates the stream once per route and
//! pushes frames, often only on damage. The alternative — one
//! `capture_image()` screenshot per tick — pays the platform's full
//! one-shot cost every frame (the Wayland portal literally has the
//! compositor write a PNG to disk per call), which is what made v1's
//! framerate so dire. The paced one-shot loop remains as the X11 path
//! (xcap's X11 "recorder" is that same screenshot in an unpaced hot loop)
//! and as the fallback wherever a session can't start (denied portal,
//! headless session) — so the stream degrades to v1 behaviour, never to
//! nothing.
//!
//! Two costs are skipped outright when they buy nothing: a frame whose
//! pixels match the previous one isn't re-encoded or re-sent (an idle
//! desktop costs one buffer compare per tick, with a periodic refresh so
//! late joiners aren't stranded), and when the link can't keep up the
//! bounded forwarder drops captures rather than queueing stale ones.
//!
//! v1 simplifications still standing, called out honestly: it captures the
//! *primary* monitor (per-monitor selection is a follow-up — the synthetic
//! `screen` capability is "the machine's screen"), and on Wayland each
//! route start runs the compositor's share-picker dialog (the portal's
//! one-time consent; restore tokens are a follow-up).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use allmystuff_session::VideoFrame;

/// Streaming ceiling on the longest frame edge. 1280 keeps a 1080p/4K
/// desktop readable while a frame stays ~60-150 KB at [`JPEG_QUALITY`] —
/// comfortably inside a LAN data channel at [`TARGET_FPS`].
const MAX_EDGE: u32 = 1280;
/// Mid-range JPEG quality — piKVM's default neighbourhood; text stays
/// legible, photos stay cheap.
const JPEG_QUALITY: u8 = 60;
/// Capture cadence to aim for — a ceiling, not a promise. Session capture
/// sustains it; the one-shot fallback runs at whatever rate the platform's
/// screenshot path allows (the budget math self-limits, as before).
const TARGET_FPS: u32 = 24;
/// An unchanged screen still re-sends one frame this often, so a viewer
/// that lost a frame (or joined a quiet stream) is never stranded on a
/// stale picture. Every tick in between costs one buffer compare.
const STATIC_REFRESH: Duration = Duration::from_secs(2);

struct RouteVideo {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for RouteVideo {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[derive(Default)]
pub struct VideoBridge {
    routes: Mutex<HashMap<String, RouteVideo>>,
}

impl VideoBridge {
    pub fn new() -> Self {
        Self::default()
    }

    /// Begin streaming the primary screen for `route_id`. `on_frame` is
    /// called with each encoded frame; it returns `false` when the frame
    /// was dropped downstream (backpressure), which is fine — the next
    /// capture simply carries the newer picture.
    pub fn start_capture<F>(&self, route_id: String, on_frame: F)
    where
        F: Fn(VideoFrame) -> bool + Send + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let id = route_id.clone();
        let thread = std::thread::spawn(move || {
            if let Err(e) = run_capture(&stop_thread, &id, on_frame) {
                tracing::warn!("screen capture for {id} stopped: {e}");
            }
        });
        self.routes.lock().insert(
            route_id,
            RouteVideo {
                stop,
                thread: Some(thread),
            },
        );
    }

    pub fn stop(&self, route_id: &str) {
        self.routes.lock().remove(route_id);
    }
}

fn run_capture<F>(stop: &AtomicBool, route_id: &str, on_frame: F) -> Result<(), String>
where
    F: Fn(VideoFrame) -> bool + Send + 'static,
{
    let monitor = primary_monitor()?;
    let mut encoder = FrameEncoder::new(route_id);
    if prefer_session_capture() {
        match run_session_capture(stop, route_id, &monitor, &on_frame, &mut encoder) {
            Ok(()) => return Ok(()),
            Err(e) => {
                if stop.load(Ordering::SeqCst) {
                    return Ok(());
                }
                tracing::warn!(
                    "capture session for {route_id} unavailable ({e}); \
                     falling back to per-frame screenshots"
                );
            }
        }
    }
    run_oneshot_capture(stop, route_id, &monitor, &on_frame, &mut encoder)
}

/// Whether to try a persistent capture session first. On Linux only
/// Wayland has a real one (PipeWire ScreenCast); under X11 xcap's recorder
/// is the same per-frame screenshot in an unpaced hot loop, so our paced
/// one-shot loop is strictly better there. Everywhere else the session
/// backend (DXGI duplication / AVFoundation) is the right default.
fn prefer_session_capture() -> bool {
    #[cfg(target_os = "linux")]
    {
        wayland_session()
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

/// Mirrors xcap's (private) `wayland_detect`, so our path choice matches
/// the one xcap will take internally.
#[cfg(target_os = "linux")]
fn wayland_session() -> bool {
    let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    session.eq_ignore_ascii_case("wayland") || display.to_lowercase().contains("wayland")
}

/// Stream from a persistent OS capture session. Set-up (portal consent,
/// duplication handles) happens once; frames arrive on `frames` as the OS
/// produces them — damage-driven backends send nothing while the screen is
/// still. Each tick encodes the *freshest* pending frame; a backlog is
/// skipped, never transcoded late.
fn run_session_capture<F>(
    stop: &AtomicBool,
    route_id: &str,
    monitor: &xcap::Monitor,
    on_frame: &F,
    encoder: &mut FrameEncoder,
) -> Result<(), String>
where
    F: Fn(VideoFrame) -> bool + Send + 'static,
{
    let (recorder, frames) = monitor.video_recorder().map_err(|e| e.to_string())?;
    recorder.start().map_err(|e| e.to_string())?;
    tracing::info!("screen capture session started for {route_id}");
    let budget = Duration::from_secs(1) / TARGET_FPS;
    let result = loop {
        if stop.load(Ordering::SeqCst) {
            break Ok(());
        }
        // A bounded wait keeps the stop flag responsive on idle screens.
        let mut frame = match frames.recv_timeout(Duration::from_millis(250)) {
            Ok(f) => f,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                break Err("capture session ended".to_string());
            }
        };
        let started = Instant::now();
        while let Ok(newer) = frames.try_recv() {
            frame = newer;
        }
        match encoder.encode(frame.raw, frame.width, frame.height) {
            Ok(Some(out)) => {
                let _ = on_frame(out);
            }
            Ok(None) => {}
            Err(e) => break Err(e),
        }
        if let Some(rest) = budget.checked_sub(started.elapsed()) {
            std::thread::sleep(rest);
        }
    };
    let _ = recorder.stop();
    result
}

/// One screenshot per tick — the X11 path and the universal fallback.
/// Every grab pays the platform's full one-shot cost, so the effective
/// rate is whatever that path allows; the encoder's unchanged-frame gate
/// at least makes idle screens cheap to *send*.
fn run_oneshot_capture<F>(
    stop: &AtomicBool,
    route_id: &str,
    monitor: &xcap::Monitor,
    on_frame: &F,
    encoder: &mut FrameEncoder,
) -> Result<(), String>
where
    F: Fn(VideoFrame) -> bool + Send + 'static,
{
    let budget = Duration::from_secs(1) / TARGET_FPS;
    let mut failures = 0u64;
    while !stop.load(Ordering::SeqCst) {
        let started = Instant::now();
        let outcome = monitor
            .capture_image()
            .map_err(|e| e.to_string())
            .and_then(|image| {
                let (sw, sh) = (image.width(), image.height());
                encoder.encode(image.into_raw(), sw, sh)
            });
        match outcome {
            Ok(Some(frame)) => {
                failures = 0;
                let _ = on_frame(frame);
            }
            Ok(None) => failures = 0,
            Err(e) => {
                // A transient grab failure (screen lock, monitor sleep)
                // shouldn't end the stream — but a *persistent* one (a
                // denied screen-recording permission, a Wayland portal
                // that never granted) must be loud, not a debug whisper:
                // it reads as "connected but no pixels" at the far end.
                failures += 1;
                if failures == 1 || failures.is_multiple_of(100) {
                    tracing::warn!("screen grab failing for {route_id} ({failures}x): {e}");
                } else {
                    tracing::debug!("screen grab failed for {route_id}: {e}");
                }
            }
        }
        if let Some(rest) = budget.checked_sub(started.elapsed()) {
            std::thread::sleep(rest);
        }
    }
    Ok(())
}

fn primary_monitor() -> Result<xcap::Monitor, String> {
    let monitors = xcap::Monitor::all().map_err(|e| e.to_string())?;
    let mut first = None;
    for m in monitors {
        if m.is_primary().unwrap_or(false) {
            return Ok(m);
        }
        first.get_or_insert(m);
    }
    first.ok_or_else(|| "no monitor to capture".to_string())
}

/// The downscale + JPEG stage of one route's stream, with the
/// unchanged-frame gate: pixels identical to the last send (compared
/// post-downscale, where the buffer is small) return `None` instead of
/// burning an encode on a picture the viewer already has — refreshed every
/// [`STATIC_REFRESH`] regardless. Owns the stream's `seq`, which counts
/// *sent* frames (the receiver logs its first frame as `seq == 0`).
struct FrameEncoder {
    route_id: String,
    seq: u64,
    prev: Vec<u8>,
    prev_size: (u32, u32),
    last_sent: Option<Instant>,
}

impl FrameEncoder {
    fn new(route_id: &str) -> Self {
        FrameEncoder {
            route_id: route_id.to_string(),
            seq: 0,
            prev: Vec::new(),
            prev_size: (0, 0),
            last_sent: None,
        }
    }

    fn encode(&mut self, rgba: Vec<u8>, sw: u32, sh: u32) -> Result<Option<VideoFrame>, String> {
        let (dw, dh) = fit_within(sw, sh, MAX_EDGE);
        let scaled = if (dw, dh) == (sw, sh) {
            rgba
        } else {
            scale_rgba(&rgba, sw, sh, dw, dh)
        };
        let refresh_due = self
            .last_sent
            .is_none_or(|sent| sent.elapsed() >= STATIC_REFRESH);
        if !refresh_due && self.prev_size == (dw, dh) && self.prev == scaled {
            return Ok(None);
        }
        let jpeg = encode_jpeg(&scaled, dw, dh)?;
        self.prev = scaled;
        self.prev_size = (dw, dh);
        self.last_sent = Some(Instant::now());
        let frame = VideoFrame::new(&self.route_id, self.seq, dw, dh, sw, sh, jpeg);
        self.seq += 1;
        Ok(Some(frame))
    }
}

fn encode_jpeg(rgba: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(64 * 1024);
    let encoder = jpeg_encoder::Encoder::new(&mut out, JPEG_QUALITY);
    encoder
        .encode(
            rgba,
            w.try_into().map_err(|_| "frame too wide")?,
            h.try_into().map_err(|_| "frame too tall")?,
            jpeg_encoder::ColorType::Rgba,
        )
        .map_err(|e| e.to_string())?;
    Ok(out)
}

/// Scale `(w, h)` down (never up) so the longest edge fits `max_edge`,
/// preserving aspect.
fn fit_within(w: u32, h: u32, max_edge: u32) -> (u32, u32) {
    let edge = w.max(h);
    if edge <= max_edge || edge == 0 {
        return (w, h);
    }
    let num = max_edge as u64;
    let den = edge as u64;
    (
        ((w as u64 * num / den) as u32).max(1),
        ((h as u64 * num / den) as u32).max(1),
    )
}

/// Nearest-neighbour RGBA downscale. At streaming sizes the JPEG pass
/// dominates visually, so the cheapest resampler wins — but it runs on
/// every frame, so the source column for each output column is computed
/// once and the inner loop is pure row-sliced copies, not per-pixel
/// index arithmetic.
fn scale_rgba(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let (sw, sh, dw, dh) = (sw as usize, sh as usize, dw as usize, dh as usize);
    let xmap: Vec<usize> = (0..dw).map(|x| (x * sw / dw) * 4).collect();
    let mut out = vec![0u8; dw * dh * 4];
    for (y, drow) in out.chunks_exact_mut(dw * 4).enumerate() {
        let sy = y * sh / dh;
        let srow = &src[sy * sw * 4..][..sw * 4];
        for (dst, &sx) in drow.chunks_exact_mut(4).zip(&xmap) {
            dst.copy_from_slice(&srow[sx..sx + 4]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_within_caps_the_long_edge_and_keeps_aspect() {
        assert_eq!(fit_within(3840, 2160, 1280), (1280, 720));
        assert_eq!(fit_within(1080, 1920, 1280), (720, 1280));
        // Already small → untouched (never upscaled).
        assert_eq!(fit_within(800, 600, 1280), (800, 600));
        assert_eq!(fit_within(0, 0, 1280), (0, 0));
    }

    #[test]
    fn scale_rgba_samples_the_right_pixels() {
        // 2x1 image: red then blue. Downscale to 1x1 keeps the left pixel.
        let src = [255, 0, 0, 255, 0, 0, 255, 255];
        assert_eq!(scale_rgba(&src, 2, 1, 1, 1), vec![255, 0, 0, 255]);
        // Upscaling 1x1 to 2x2 repeats it (the fn never errors on growth
        // even though fit_within never asks for it).
        let one = [9, 8, 7, 255];
        assert_eq!(scale_rgba(&one, 1, 1, 2, 2), one.repeat(4));
    }

    #[test]
    fn jpeg_encoder_produces_a_jpeg() {
        let rgba = vec![128u8; 8 * 8 * 4];
        let jpeg = encode_jpeg(&rgba, 8, 8).expect("encode");
        // SOI marker.
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
    }

    #[test]
    fn encoder_skips_unchanged_frames_and_keeps_seq_for_sent_ones() {
        let mut enc = FrameEncoder::new("r");
        let a = vec![10u8; 8 * 8 * 4];
        let first = enc.encode(a.clone(), 8, 8).unwrap().expect("first sends");
        assert_eq!(first.seq, 0);
        // Same pixels again, inside the refresh window → skipped.
        assert!(enc.encode(a.clone(), 8, 8).unwrap().is_none());
        // Changed pixels → sent, with the next seq (skips don't burn one).
        let b = vec![200u8; 8 * 8 * 4];
        let second = enc.encode(b, 8, 8).unwrap().expect("change sends");
        assert_eq!(second.seq, 1);
    }

    #[test]
    fn encoder_resends_after_the_refresh_interval() {
        let mut enc = FrameEncoder::new("r");
        let a = vec![10u8; 8 * 8 * 4];
        enc.encode(a.clone(), 8, 8).unwrap().expect("first sends");
        enc.last_sent = Some(Instant::now() - STATIC_REFRESH);
        let refreshed = enc.encode(a, 8, 8).unwrap();
        assert_eq!(refreshed.expect("refresh resends").seq, 1);
    }
}
