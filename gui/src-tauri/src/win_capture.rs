//! In-house DXGI Output Duplication — the Windows screen-capture session.
//!
//! Why not xcap's recorder, which we use everywhere else? Its DXGI
//! implementation has two defects that together cost most of the
//! framerate in the field: walking to the target monitor it calls
//! `DuplicateOutput` on **every output it passes** (DXGI returns
//! `E_INVALIDARG` when an output is already duplicated, killing the whole
//! creation even though the *target* is free), and its capture thread
//! never exits — `stop()` only sleeps a waker, so the thread's clone of
//! the duplication handle leaks for the process lifetime. Net effect: the
//! first session in a process works, and every later one — any monitor —
//! fails with `0x80070057` and drops to per-frame GDI screenshots.
//!
//! This module duplicates exactly the requested output, swizzles BGRA →
//! RGBA while copying out of the staging texture (fused into the copy we
//! must do anyway, so downstream stays the one RGBA pipeline), exits its
//! thread on stop, and re-acquires after `DXGI_ERROR_ACCESS_LOST` (mode
//! change, fullscreen transition, UAC desktop) instead of dying.
//!
//! Frames are damage-driven: `AcquireNextFrame` returns only when the
//! desktop actually changed, so an idle screen costs polling, not copies.

#![cfg(windows)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::core::Interface;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGIAdapter, IDXGIDevice, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
    DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
};

/// One captured desktop frame, already RGBA.
pub struct RawFrame {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// A running duplication session. Dropping it stops the thread — really
/// stops it: the thread exits and the duplication handle is released, so
/// the output is immediately re-duplicable.
pub struct Session {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Begin duplicating the monitor with this id (xcap's monitor id — the
/// raw `HMONITOR` value, which is also what `DXGI_OUTPUT_DESC` carries).
/// Fails fast on the caller's thread when the output can't be duplicated
/// right now (someone else holds it, RDP session), so the caller can fall
/// back without waiting on a channel.
pub fn start(monitor_id: u32) -> Result<(Session, mpsc::Receiver<RawFrame>), String> {
    let dup = Duplication::new(monitor_id)?;
    // A shallow channel: the consumer drains to the freshest frame each
    // tick; anything it hasn't taken by the time two more arrive is stale.
    let (tx, rx) = mpsc::sync_channel::<RawFrame>(2);
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let thread = std::thread::spawn(move || pump(dup, monitor_id, &stop_thread, &tx));
    Ok((
        Session {
            stop,
            thread: Some(thread),
        },
        rx,
    ))
}

fn pump(mut dup: Duplication, monitor_id: u32, stop: &AtomicBool, tx: &mpsc::SyncSender<RawFrame>) {
    while !stop.load(Ordering::SeqCst) {
        match dup.next_frame(100) {
            Ok(Some(frame)) => {
                // try_send: a full channel means the consumer is behind;
                // dropping this frame just means the next one is fresher.
                let _ = tx.try_send(frame);
            }
            Ok(None) => {} // timeout (idle screen) or a mouse-only update
            Err(NextError::AccessLost) => {
                // Mode change / fullscreen toggle / secure desktop: the
                // duplication is dead but the monitor isn't. Re-acquire
                // for up to ~10 s before giving up on the session.
                tracing::debug!("duplication of monitor {monitor_id:#x} lost — re-acquiring");
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    if stop.load(Ordering::SeqCst) {
                        return;
                    }
                    match Duplication::new(monitor_id) {
                        Ok(d) => {
                            dup = d;
                            break;
                        }
                        Err(e) if Instant::now() < deadline => {
                            tracing::debug!("re-acquire not ready ({e}); retrying");
                            std::thread::sleep(Duration::from_millis(250));
                        }
                        Err(e) => {
                            tracing::warn!(
                                "duplication of monitor {monitor_id:#x} not recoverable: {e}"
                            );
                            return;
                        }
                    }
                }
            }
            Err(NextError::Fatal(e)) => {
                tracing::warn!("duplication of monitor {monitor_id:#x} ended: {e}");
                return;
            }
        }
    }
}

enum NextError {
    /// The duplication handle died (mode change etc.) — recreate it.
    AccessLost,
    /// Anything else — end the session; the route falls back.
    Fatal(String),
}

struct Duplication {
    _device: ID3D11Device,
    context: ID3D11DeviceContext,
    dup: IDXGIOutputDuplication,
    /// CPU-readable copy target, reused across frames of the same size.
    staging: Option<(ID3D11Texture2D, u32, u32)>,
}

impl Duplication {
    fn new(monitor_id: u32) -> Result<Self, String> {
        unsafe {
            let mut device: Option<ID3D11Device> = None;
            let mut context: Option<ID3D11DeviceContext> = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE(std::ptr::null_mut()),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .map_err(|e| format!("D3D11CreateDevice: {e}"))?;
            let device = device.ok_or("D3D11CreateDevice returned no device")?;
            let context = context.ok_or("D3D11CreateDevice returned no context")?;
            let dxgi: IDXGIDevice = device.cast().map_err(|e| e.to_string())?;
            let adapter: IDXGIAdapter = dxgi.GetAdapter().map_err(|e| e.to_string())?;

            // Find the target output, and duplicate *only* that one — an
            // already-duplicated sibling output must not abort us.
            let mut index = 0u32;
            loop {
                let Ok(output) = adapter.EnumOutputs(index) else {
                    return Err(format!(
                        "monitor {monitor_id:#x} not found on the primary adapter"
                    ));
                };
                index += 1;
                let desc = output.GetDesc().map_err(|e| e.to_string())?;
                if desc.Monitor.0 as usize as u32 != monitor_id {
                    continue;
                }
                let output1: IDXGIOutput1 = output.cast().map_err(|e| e.to_string())?;
                let dup = output1
                    .DuplicateOutput(&device)
                    .map_err(|e| format!("DuplicateOutput: {e}"))?;
                return Ok(Duplication {
                    _device: device,
                    context,
                    dup,
                    staging: None,
                });
            }
        }
    }

    /// Wait up to `timeout_ms` for the desktop to change; `Ok(None)` on
    /// timeout or a cursor-only update (no new pixels).
    fn next_frame(&mut self, timeout_ms: u32) -> Result<Option<RawFrame>, NextError> {
        unsafe {
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource: Option<IDXGIResource> = None;
            if let Err(e) = self
                .dup
                .AcquireNextFrame(timeout_ms, &mut info, &mut resource)
            {
                return match e.code() {
                    c if c == DXGI_ERROR_WAIT_TIMEOUT => Ok(None),
                    c if c == DXGI_ERROR_ACCESS_LOST => Err(NextError::AccessLost),
                    _ => Err(NextError::Fatal(e.to_string())),
                };
            }
            // From here the frame is held; release it on every path or
            // the next acquire can't proceed.
            let result = self.copy_out(info, resource);
            let _ = self.dup.ReleaseFrame();
            result.map_err(NextError::Fatal)
        }
    }

    unsafe fn copy_out(
        &mut self,
        info: DXGI_OUTDUPL_FRAME_INFO,
        resource: Option<IDXGIResource>,
    ) -> Result<Option<RawFrame>, String> {
        if info.LastPresentTime == 0 {
            // Cursor moved, pixels didn't — nothing to encode.
            return Ok(None);
        }
        let resource = resource.ok_or("AcquireNextFrame returned no resource")?;
        let texture: ID3D11Texture2D = resource.cast().map_err(|e| e.to_string())?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        texture.GetDesc(&mut desc);
        let (w, h) = (desc.Width, desc.Height);
        if w == 0 || h == 0 {
            return Ok(None);
        }
        let staging = self.staging_for(w, h)?;
        self.context.CopyResource(&staging, &texture);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        self.context
            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .map_err(|e| format!("Map staging: {e}"))?;
        let pitch = mapped.RowPitch as usize;
        let (wu, hu) = (w as usize, h as usize);
        let src = std::slice::from_raw_parts(mapped.pData as *const u8, pitch * hu);
        let mut rgba = vec![0u8; wu * hu * 4];
        for row in 0..hu {
            let s = &src[row * pitch..][..wu * 4];
            let d = &mut rgba[row * wu * 4..][..wu * 4];
            // BGRA → RGBA fused into the copy we must do anyway; alpha is
            // forced opaque (duplication leaves it undefined).
            for (dp, sp) in d.chunks_exact_mut(4).zip(s.chunks_exact(4)) {
                dp[0] = sp[2];
                dp[1] = sp[1];
                dp[2] = sp[0];
                dp[3] = 255;
            }
        }
        self.context.Unmap(&staging, 0);
        Ok(Some(RawFrame {
            rgba,
            width: w,
            height: h,
        }))
    }

    /// The reusable CPU-readable texture, rebuilt when the desktop size
    /// changes (resolution switch mid-session).
    unsafe fn staging_for(&mut self, w: u32, h: u32) -> Result<ID3D11Texture2D, String> {
        if let Some((tex, sw, sh)) = &self.staging {
            if (*sw, *sh) == (w, h) {
                return Ok(tex.clone());
            }
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut tex: Option<ID3D11Texture2D> = None;
        self._device
            .CreateTexture2D(&desc, None, Some(&mut tex))
            .map_err(|e| format!("CreateTexture2D (staging): {e}"))?;
        let tex = tex.ok_or("CreateTexture2D returned no texture")?;
        self.staging = Some((tex.clone(), w, h));
        Ok(tex)
    }
}
