//! The display media plane: MJPEG capture of this machine's screen, so an
//! active display route actually streams pixels — the piKVM transport
//! (every frame a standalone JPEG; losing one costs one frame and there's
//! no codec state to desync).
//!
//! Mirrors [`crate::audio::AudioBridge`]'s shape: each sourcing route runs
//! a dedicated thread that captures the **primary monitor** with `xcap`
//! (DXGI on Windows, CoreGraphics on macOS, X11/Wayland-portal on Linux),
//! downscales to a sane streaming size, JPEG-encodes, and hands the frame
//! to a callback the mesh forwards on the media channel.
//!
//! v1 simplifications, called out honestly (matching the audio bridge's):
//! it captures the *primary* monitor (per-monitor selection is a follow-up
//! — the synthetic `screen` capability is "the machine's screen"), at a
//! fixed target cadence with drop-on-backpressure rather than a rate
//! negotiation.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
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
/// Capture cadence to aim for. The effective rate adapts downward when
/// encode time or channel backpressure (frames dropped by the bounded
/// forwarder) eats the budget.
const TARGET_FPS: u32 = 12;

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
    let budget = Duration::from_secs(1) / TARGET_FPS;
    let mut seq = 0u64;
    while !stop.load(Ordering::SeqCst) {
        let started = Instant::now();
        match capture_frame(&monitor, route_id, seq) {
            Ok(frame) => {
                seq += 1;
                let _ = on_frame(frame);
            }
            Err(e) => {
                // A transient grab failure (screen lock, monitor sleep)
                // shouldn't end the stream; log and try again next tick.
                tracing::debug!("screen grab failed for {route_id}: {e}");
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

fn capture_frame(monitor: &xcap::Monitor, route_id: &str, seq: u64) -> Result<VideoFrame, String> {
    let image = monitor.capture_image().map_err(|e| e.to_string())?;
    let (sw, sh) = (image.width(), image.height());
    let (dw, dh) = fit_within(sw, sh, MAX_EDGE);
    let rgba = if (dw, dh) == (sw, sh) {
        image.into_raw()
    } else {
        scale_rgba(image.as_raw(), sw, sh, dw, dh)
    };
    let jpeg = encode_jpeg(&rgba, dw, dh)?;
    Ok(VideoFrame::new(route_id, seq, dw, dh, sw, sh, jpeg))
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
/// dominates visually, so the cheapest resampler wins — this runs a dozen
/// times a second on a desktop frame.
fn scale_rgba(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let mut out = vec![0u8; dw as usize * dh as usize * 4];
    for y in 0..dh {
        let sy = (y as u64 * sh as u64 / dh as u64) as u32;
        for x in 0..dw {
            let sx = (x as u64 * sw as u64 / dw as u64) as u32;
            let s = ((sy * sw + sx) * 4) as usize;
            let d = ((y * dw + x) * 4) as usize;
            out[d..d + 4].copy_from_slice(&src[s..s + 4]);
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
}
