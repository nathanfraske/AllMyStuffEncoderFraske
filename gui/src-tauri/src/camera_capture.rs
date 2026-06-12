//! Camera capture for video routes — the per-frame source behind a
//! `MediaKind::Video` stream, the way [`crate::win_capture`] /
//! [`crate::wayland_capture`] / xcap are behind a display one. One API
//! over the three platform stacks via `nokhwa`: V4L2 on Linux,
//! AVFoundation on macOS, Media Foundation on Windows.
//!
//! Shape matches the other capture sessions: [`open`] starts a dedicated
//! reader thread that pulls frames from the OS, decodes them to packed
//! RGBA (UVC cameras mostly hand over MJPEG or YUYV — nokhwa's decoder
//! covers both), and feeds a bounded channel the encoder pump drains
//! freshest-first. Dropping the [`CameraSession`] stops the reader and
//! releases the device, so a torn-down route frees the camera for the
//! next app.
//!
//! Which camera: a video route's source capability embeds the inventory's
//! device id (`cam:video0` on Linux — the `/dev/videoN` node — `cam:<n>`
//! by enumeration order elsewhere). Both forms end in the ordinal the OS
//! enumeration uses, so [`device_ordinal`] recovers it; a named camera
//! that's gone (unplugged since the scan) degrades to the first one
//! present with a note — a stream beats an error, the `select_monitor`
//! rule.
//!
//! Format: ask for the camera's smoothest frame rate and the largest
//! picture it carries (`AbsoluteHighestFrameRate`) — for a webcam that's
//! the 1080p30 / 720p60 sweet spot, never the 4K-at-5-fps trap — and let
//! the route's encoder do any fitting, exactly as it does for monitors.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use nokhwa::pixel_format::RgbAFormat;
use nokhwa::utils::{ApiBackend, CameraIndex, RequestedFormat, RequestedFormatType};

/// One captured picture, packed RGBA — what the encoder pump wants.
/// (Same shape as the screen sessions' frames.)
pub struct RawFrame {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// The live capture: an open OS camera feeding the frame channel.
/// Dropping it stops the reader thread and releases the device.
pub struct CameraSession {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for CameraSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Open the camera `device` names (an inventory id like `cam:video0`)
/// and start pulling frames. `fps` paces the reader loop — a ceiling,
/// not a promise; the camera's own rate is what actually arrives.
///
/// The device is constructed, streamed and dropped entirely on the
/// reader thread (an open-result handshake makes failures synchronous
/// here) — one thread owns the OS handle for its whole life, so no
/// platform's thread-affinity rules are ever in play.
pub fn open(device: &str, fps: u32) -> Result<(CameraSession, Receiver<RawFrame>), String> {
    ensure_permission()?;
    let index = resolve_index(device)?;
    // Two frames of slack: the pump drains freshest-first, so anything
    // deeper only adds latency.
    let (tx, rx) = std::sync::mpsc::sync_channel::<RawFrame>(2);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let thread = std::thread::spawn(move || {
        // Smoothest rate first, biggest picture at it — the webcam sweet
        // spot. A camera that lists nothing nokhwa can decode errors here.
        let format =
            RequestedFormat::new::<RgbAFormat>(RequestedFormatType::AbsoluteHighestFrameRate);
        let opened = nokhwa::Camera::new(index.clone(), format)
            .map_err(|e| format!("camera {} won't open: {e}", index.as_string()))
            .and_then(|mut cam| {
                cam.open_stream()
                    .map_err(|e| format!("camera {} stream: {e}", index.as_string()))?;
                Ok(cam)
            });
        let mut camera = match opened {
            Ok(cam) => {
                let _ = ready_tx.send(Ok(()));
                cam
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e));
                return;
            }
        };
        let fmt = camera.camera_format();
        tracing::info!(
            "camera {} open: {}×{} @ {} fps ({})",
            index.as_string(),
            fmt.resolution().width(),
            fmt.resolution().height(),
            fmt.frame_rate(),
            fmt.format(),
        );
        run_reader(&mut camera, &stop_thread, &tx, fps);
        // Release the device explicitly so the next app (or the next
        // route) can open it the moment the route ends.
        let _ = camera.stop_stream();
    });
    let session = CameraSession {
        stop,
        thread: Some(thread),
    };
    match ready_rx.recv() {
        Ok(Ok(())) => Ok((session, rx)),
        // Joining the already-finished thread via Drop keeps the error
        // path tidy.
        Ok(Err(e)) => Err(e),
        Err(_) => Err("camera reader thread died opening the device".to_string()),
    }
}

/// The reader loop: one OS frame per tick, decoded to RGBA, try-sent (a
/// full channel drops this picture — the next is fresher). V4L2's
/// `frame()` blocks until the sensor delivers, pacing the loop naturally;
/// the explicit pacing below is for backends that poll a buffer instead
/// (AVFoundation), where a hot loop would re-decode the same picture.
fn run_reader(
    camera: &mut nokhwa::Camera,
    stop: &AtomicBool,
    tx: &SyncSender<RawFrame>,
    fps: u32,
) {
    let budget = Duration::from_secs(1) / fps.max(1);
    let mut failures = 0u64;
    while !stop.load(Ordering::SeqCst) {
        let started = Instant::now();
        match camera.frame().and_then(|f| f.decode_image::<RgbAFormat>()) {
            Ok(decoded) => {
                failures = 0;
                let (width, height) = (decoded.width(), decoded.height());
                let frame = RawFrame {
                    rgba: decoded.into_raw(),
                    width,
                    height,
                };
                match tx.try_send(frame) {
                    Ok(()) | Err(TrySendError::Full(_)) => {}
                    // The route ended and the pump is gone; stop reading.
                    Err(TrySendError::Disconnected(_)) => return,
                }
            }
            Err(e) => {
                // A transient hiccup shouldn't end the stream, but a
                // camera that yanks its cable mid-route fails every grab
                // from here on — give up after a patient run of them so
                // the pump's Disconnected arm reports it to the viewer.
                failures += 1;
                if failures == 1 {
                    tracing::warn!("camera frame failed: {e}");
                } else {
                    tracing::debug!("camera frame failed ({failures}x): {e}");
                }
                if failures >= 50 {
                    tracing::warn!("camera stream gave up after {failures} failed grabs");
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        }
        if let Some(rest) = budget.checked_sub(started.elapsed()) {
            std::thread::sleep(rest);
        }
    }
}

/// macOS: AVFoundation insists the camera permission is settled before
/// any capture call. First use raises the OS prompt (the bundle's
/// `NSCameraUsageDescription`) and waits here for the human; a denial
/// comes back as the error the viewer reads. Everywhere else this is
/// free — Linux/Windows gate at device-open instead.
fn ensure_permission() -> Result<(), String> {
    if !nokhwa::nokhwa_check() {
        let (tx, rx) = std::sync::mpsc::channel();
        nokhwa::nokhwa_initialize(move |granted| {
            let _ = tx.send(granted);
        });
        match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(true) => {}
            Ok(false) => return Err("camera access denied in System Settings".to_string()),
            Err(_) => return Err("camera permission prompt unanswered".to_string()),
        }
    }
    Ok(())
}

/// The camera the route names, by OS enumeration: the ordinal embedded in
/// the inventory id when a camera with it exists, the first camera
/// otherwise (unplugged since the scan — degrade with a note, the
/// `select_monitor` rule). Errors only when there's no camera at all.
fn resolve_index(device: &str) -> Result<CameraIndex, String> {
    let cameras = nokhwa::query(ApiBackend::Auto).map_err(|e| format!("camera query: {e}"))?;
    if cameras.is_empty() {
        return Err("no camera on this machine".to_string());
    }
    if let Some(wanted) = device_ordinal(device) {
        for c in &cameras {
            if c.index().as_index().is_ok_and(|i| i == wanted) {
                return Ok(c.index().clone());
            }
        }
        tracing::warn!("camera {device} not found (unplugged?); capturing the first one instead");
    }
    Ok(cameras[0].index().clone())
}

/// The OS enumeration ordinal embedded in an inventory camera id — the
/// trailing digit run: `cam:video0` → 0 (Linux device nodes), `cam:3` →
/// 3 (macOS / Windows enumeration order). `None` when the id carries no
/// number; the caller falls back to the first camera.
fn device_ordinal(device: &str) -> Option<u32> {
    let tail: String = device
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    tail.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_ordinal_reads_the_trailing_number() {
        // Linux inventory ids name the /dev node; the digits are the
        // V4L2 index nokhwa opens by.
        assert_eq!(device_ordinal("cam:video0"), Some(0));
        assert_eq!(device_ordinal("cam:video12"), Some(12));
        // macOS / Windows ids are bare enumeration ordinals.
        assert_eq!(device_ordinal("cam:0"), Some(0));
        assert_eq!(device_ordinal("cam:3"), Some(3));
        // No number → no claim; the caller takes the first camera.
        assert_eq!(device_ordinal("cam:builtin"), None);
        assert_eq!(device_ordinal(""), None);
    }
}
