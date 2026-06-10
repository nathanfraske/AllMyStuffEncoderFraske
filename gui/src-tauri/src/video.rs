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

/// Which transport a display route's stream encodes for — picked by the
/// mesh from the offer's `video` accepts (see `RouteControl::Offer`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoMode {
    /// Standalone JPEG frames over the media channel — the v1 transport
    /// and the universal fallback.
    Mjpeg,
    /// H.264 access units for the mesh's RTP track lane.
    H264,
}

/// One capture tick's output, headed for the forwarder. (H.264 units
/// carry no key flag — the receiving daemon re-derives IDR-ness from the
/// NAL stream itself when it reassembles units off RTP.)
#[derive(Debug, Clone)]
pub enum VideoPacket {
    Jpeg(VideoFrame),
    H264 {
        /// Annex-B access unit.
        data: Vec<u8>,
        /// Capture-tick pacing for the RTP clock (1/fps).
        duration_us: u64,
    },
}

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
const TARGET_FPS: u32 = 30;
/// An unchanged screen still re-sends one frame this often, so a viewer
/// that lost a frame (or joined a quiet stream) is never stranded on a
/// stale picture. Every tick in between costs one buffer compare.
const STATIC_REFRESH: Duration = Duration::from_secs(2);

/// H.264 ceiling on the longest edge — sharper than the MJPEG cap
/// because inter-frame compression pays for the pixels. 1920 keeps a 4K
/// desktop at a crisp 2:1 and software encode comfortably real-time;
/// also inside openh264's 3840×2160 hard limit. Dimensions are forced
/// even (4:2:0 chroma needs it).
const H264_MAX_EDGE: u32 = 1920;
/// Target bitrate for the track lane. ~6 Mbps carries 1920-edge desktop
/// content well in screen-content mode and stays trivial on a LAN.
const H264_BITRATE_BPS: u32 = 6_000_000;
/// Forced IDR cadence — bounds how long a viewer that joined mid-stream
/// (or lost an unrepaired packet) waits for a clean decode entry.
const H264_IDR_EVERY: Duration = Duration::from_secs(4);

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

    /// Begin streaming the primary screen for `route_id`, encoding for
    /// `mode`. `on_packet` is called with each encoded packet; it returns
    /// `false` when the packet was dropped downstream (backpressure),
    /// which is fine — the next capture simply carries the newer picture.
    pub fn start_capture<F>(&self, route_id: String, mode: VideoMode, on_packet: F)
    where
        F: Fn(VideoPacket) -> bool + Send + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let id = route_id.clone();
        let thread = std::thread::spawn(move || {
            if let Err(e) = run_capture(&stop_thread, &id, mode, on_packet) {
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

fn run_capture<F>(
    stop: &AtomicBool,
    route_id: &str,
    mode: VideoMode,
    on_packet: F,
) -> Result<(), String>
where
    F: Fn(VideoPacket) -> bool + Send + 'static,
{
    let monitor = primary_monitor()?;
    // An encoder that can't init (openh264 build/runtime trouble) must
    // cost quality, not the stream: fall back to MJPEG and say so.
    let mut encoder = match StreamEncoder::new(route_id, mode) {
        Ok(enc) => enc,
        Err(e) => {
            tracing::warn!("encoder for {route_id} unavailable ({e}); falling back to MJPEG");
            StreamEncoder::new(route_id, VideoMode::Mjpeg)?
        }
    };
    if prefer_session_capture() {
        match run_session_capture(stop, route_id, &monitor, &on_packet, &mut encoder) {
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
    run_oneshot_capture(stop, route_id, &monitor, &on_packet, &mut encoder)
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
    on_packet: &F,
    encoder: &mut StreamEncoder,
) -> Result<(), String>
where
    F: Fn(VideoPacket) -> bool + Send + 'static,
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
                let _ = on_packet(out);
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
    on_packet: &F,
    encoder: &mut StreamEncoder,
) -> Result<(), String>
where
    F: Fn(VideoPacket) -> bool + Send + 'static,
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
            Ok(Some(packet)) => {
                failures = 0;
                let _ = on_packet(packet);
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

/// The per-route encode stage, dispatching on the negotiated transport.
/// (The H.264 arm boxes openh264's chunky encoder state.)
enum StreamEncoder {
    Mjpeg(FrameEncoder),
    H264(Box<H264Stream>),
}

impl StreamEncoder {
    fn new(route_id: &str, mode: VideoMode) -> Result<Self, String> {
        match mode {
            VideoMode::Mjpeg => Ok(StreamEncoder::Mjpeg(FrameEncoder::new(route_id))),
            VideoMode::H264 => Ok(StreamEncoder::H264(Box::new(H264Stream::new()?))),
        }
    }

    fn encode(&mut self, rgba: Vec<u8>, sw: u32, sh: u32) -> Result<Option<VideoPacket>, String> {
        match self {
            StreamEncoder::Mjpeg(enc) => Ok(enc.encode(rgba, sw, sh)?.map(VideoPacket::Jpeg)),
            StreamEncoder::H264(enc) => enc.encode(rgba, sw, sh),
        }
    }
}

/// The H.264 encode stage of one route's stream — openh264 in
/// screen-content mode, scaled to [`H264_MAX_EDGE`] (even dimensions for
/// 4:2:0), with the same unchanged-frame gate as MJPEG and a forced IDR
/// every [`H264_IDR_EVERY`] so a viewer always has a decode entry point
/// within seconds. A resolution change (monitor swap) re-initializes the
/// encoder inside openh264; the next unit out is an IDR.
struct H264Stream {
    encoder: openh264::encoder::Encoder,
    prev: Vec<u8>,
    prev_size: (u32, u32),
    last_sent: Option<Instant>,
    last_idr: Option<Instant>,
}

impl H264Stream {
    fn new() -> Result<Self, String> {
        use openh264::encoder::{
            BitRate, Encoder, EncoderConfig, FrameRate, RateControlMode, UsageType,
        };
        let config = EncoderConfig::new()
            .usage_type(UsageType::ScreenContentRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .bitrate(BitRate::from_bps(H264_BITRATE_BPS))
            .max_frame_rate(FrameRate::from_hz(TARGET_FPS as f32));
        let encoder = Encoder::with_api_config(openh264::OpenH264API::from_source(), config)
            .map_err(|e| format!("openh264 init: {e}"))?;
        Ok(H264Stream {
            encoder,
            prev: Vec::new(),
            prev_size: (0, 0),
            last_sent: None,
            last_idr: None,
        })
    }

    fn encode(&mut self, rgba: Vec<u8>, sw: u32, sh: u32) -> Result<Option<VideoPacket>, String> {
        if sw == 0 || sh == 0 {
            return Ok(None);
        }
        let (dw, dh) = fit_within_even(sw, sh, H264_MAX_EDGE);
        // Scale and strip alpha in one pass: the encoder's fast RGB→YUV
        // path wants tightly packed 3-byte pixels, and the unchanged-
        // frame compare gets 25% cheaper for free.
        let scaled = scale_rgba_to_rgb(&rgba, sw, sh, dw, dh);
        let refresh_due = self
            .last_sent
            .is_none_or(|sent| sent.elapsed() >= STATIC_REFRESH);
        if !refresh_due && self.prev_size == (dw, dh) && self.prev == scaled {
            return Ok(None);
        }
        if self
            .last_idr
            .is_none_or(|idr| idr.elapsed() >= H264_IDR_EVERY)
        {
            self.encoder.force_intra_frame();
        }
        let yuv = openh264::formats::YUVBuffer::from_rgb8_source(
            openh264::formats::RgbSliceU8::new(&scaled, (dw as usize, dh as usize)),
        );
        let stream = self
            .encoder
            .encode(&yuv)
            .map_err(|e| format!("h264 encode: {e}"))?;
        let key = matches!(
            stream.frame_type(),
            openh264::encoder::FrameType::IDR | openh264::encoder::FrameType::I
        );
        let data = stream.to_vec();
        self.prev = scaled;
        self.prev_size = (dw, dh);
        self.last_sent = Some(Instant::now());
        if key {
            self.last_idr = Some(Instant::now());
        }
        if data.is_empty() {
            // Rate control may skip a frame outright; nothing to send.
            return Ok(None);
        }
        Ok(Some(VideoPacket::H264 {
            data,
            duration_us: 1_000_000u64 / TARGET_FPS as u64,
        }))
    }
}

/// [`fit_within`], then force both edges even (4:2:0 chroma subsampling
/// needs it; a 1-pixel crop is invisible at these sizes).
fn fit_within_even(w: u32, h: u32, max_edge: u32) -> (u32, u32) {
    let (w, h) = fit_within(w, h, max_edge);
    ((w & !1).max(2), (h & !1).max(2))
}

/// Nearest-neighbour scale straight from RGBA capture to tightly packed
/// RGB (alpha dropped). Same row-sliced structure as [`scale_rgba`]; also
/// serves as the plain strip pass when no scaling is needed.
fn scale_rgba_to_rgb(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let (sw, sh, dw, dh) = (sw as usize, sh as usize, dw as usize, dh as usize);
    let xmap: Vec<usize> = (0..dw).map(|x| (x * sw / dw) * 4).collect();
    let mut out = vec![0u8; dw * dh * 3];
    for (y, drow) in out.chunks_exact_mut(dw * 3).enumerate() {
        let sy = y * sh / dh;
        let srow = &src[sy * sw * 4..][..sw * 4];
        for (dst, &sx) in drow.chunks_exact_mut(3).zip(&xmap) {
            dst.copy_from_slice(&srow[sx..sx + 3]);
        }
    }
    out
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
