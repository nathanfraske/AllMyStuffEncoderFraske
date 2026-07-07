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
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_MODE_ROTATION_ROTATE180, DXGI_MODE_ROTATION_ROTATE270, DXGI_MODE_ROTATION_ROTATE90,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGIAdapter, IDXGIDevice, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
    DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
    DXGI_OUTDUPL_POINTER_SHAPE_INFO,
};

/// One captured desktop frame, already RGBA.
pub struct RawFrame {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Authoritative clockwise rotation of the scan-out, normalized to
    /// {0,90,180,270}. From `DXGI_OUTDUPL_DESC.Rotation`, read once per
    /// duplication. The raw buffer is rotated by THIS to become upright.
    pub rotation_deg: u32,
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

/// The mouse cursor as DXGI hands it over — the real OS pointer shape (arrow,
/// I-beam, hand, resize, a custom app cursor). Desktop Duplication never draws
/// it into the frame (that's a hardware overlay), so on Windows we composite it
/// ourselves; macOS/Linux capture bakes their own cursor in already. Cached
/// because the *shape* only changes when the pointer image does (a new
/// `PointerShapeBufferSize`), while the *position* updates every mouse move.
struct CursorShape {
    /// `DXGI_OUTDUPL_POINTER_SHAPE_TYPE`: 1 monochrome, 2 color (BGRA), 4
    /// masked-color. Stored raw so no extra windows-crate import is needed.
    kind: u32,
    width: u32,
    height: u32,
    pitch: u32,
    buf: Vec<u8>,
}

struct Duplication {
    _device: ID3D11Device,
    context: ID3D11DeviceContext,
    dup: IDXGIOutputDuplication,
    /// CPU-readable copy target, reused across frames of the same size.
    staging: Option<(ID3D11Texture2D, u32, u32)>,
    /// Clockwise degrees from `DXGI_OUTDUPL_DESC.Rotation`, read at
    /// construction; fixed for the duplication's lifetime. A mode change
    /// kills the duplication with ACCESS_LOST and pump rebuilds it, so the
    /// rebuilt one re-reads the (possibly new) orientation for free.
    rotation_deg: u32,
    /// Latest cursor bitmap (see [`CursorShape`]); `None` until the first
    /// shape arrives.
    cursor: Option<CursorShape>,
    /// Cursor top-left on the desktop surface, and whether it's showing —
    /// from `DXGI_OUTDUPL_FRAME_INFO.PointerPosition`, updated on every mouse
    /// move (`LastMouseUpdateTime != 0`).
    ptr_x: i32,
    ptr_y: i32,
    ptr_visible: bool,
    /// The last frame's pixels WITHOUT the cursor drawn in, so a pointer-only
    /// move (no desktop change — the common case while hovering) can be
    /// re-emitted with the cursor at its new spot instead of freezing it.
    last_clean: Option<Vec<u8>>,
    last_dims: (u32, u32),
    /// Rate limit for those cursor-only re-emits so a fast mouse can't spin the
    /// pump faster than the capture cadence.
    last_cursor_emit: Instant,
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
                // The duplicated output's own rotation is the ground truth for
                // how its raw scan-out is oriented — far more reliable than a
                // separate monitor-rotation query, which can report the native
                // (unrotated) geometry. Read it once: it's fixed for the
                // duplication's life, and an orientation change tears the
                // duplication down with ACCESS_LOST (pump re-acquires, re-reads
                // it on the fresh one). GetDesc is infallible and by-value.
                let rotation_deg = match dup.GetDesc().Rotation {
                    DXGI_MODE_ROTATION_ROTATE90 => 90,
                    DXGI_MODE_ROTATION_ROTATE180 => 180,
                    DXGI_MODE_ROTATION_ROTATE270 => 270,
                    _ => 0, // IDENTITY / UNSPECIFIED / anything else: upright.
                };
                return Ok(Duplication {
                    _device: device,
                    context,
                    dup,
                    staging: None,
                    rotation_deg,
                    cursor: None,
                    ptr_x: 0,
                    ptr_y: 0,
                    ptr_visible: false,
                    last_clean: None,
                    last_dims: (0, 0),
                    last_cursor_emit: Instant::now(),
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
            // the next acquire can't proceed. Pointer metadata must be read
            // while the frame is held too, so do it before copy_out.
            if info.PointerShapeBufferSize > 0 {
                // A new pointer bitmap is available — cache it. Best-effort:
                // a failed fetch keeps the previous shape, never breaks capture.
                self.update_cursor_shape(info.PointerShapeBufferSize);
            }
            if info.LastMouseUpdateTime != 0 {
                let p = info.PointerPosition;
                self.ptr_x = p.Position.x;
                self.ptr_y = p.Position.y;
                self.ptr_visible = p.Visible.as_bool();
            }
            let result = self.copy_out(info, resource);
            let _ = self.dup.ReleaseFrame();
            result.map_err(NextError::Fatal)
        }
    }

    /// Fetch and cache the current pointer bitmap (held-frame only). Silent on
    /// failure — the cursor just keeps its last shape.
    unsafe fn update_cursor_shape(&mut self, size: u32) {
        let mut buf = vec![0u8; size as usize];
        let mut required = 0u32;
        let mut info = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
        if self
            .dup
            .GetFramePointerShape(
                size,
                buf.as_mut_ptr() as *mut core::ffi::c_void,
                &mut required,
                &mut info,
            )
            .is_ok()
        {
            self.cursor = Some(CursorShape {
                kind: info.Type,
                width: info.Width,
                height: info.Height,
                pitch: info.Pitch,
                buf,
            });
        }
    }

    /// A pointer-only update (no desktop change): re-emit the last frame with
    /// the cursor moved to its new spot, rate-limited to the capture cadence.
    /// `None` when there's nothing to show or the limiter says wait.
    fn cursor_only_frame(&mut self) -> Option<RawFrame> {
        if !self.ptr_visible || self.cursor.is_none() || self.last_clean.is_none() {
            return None;
        }
        if self.last_cursor_emit.elapsed() < Duration::from_millis(15) {
            return None;
        }
        let (w, h) = self.last_dims;
        let mut rgba = self.last_clean.as_ref().unwrap().clone();
        if let Some(cur) = &self.cursor {
            composite_cursor(&mut rgba, w, h, cur, self.ptr_x, self.ptr_y);
        }
        self.last_cursor_emit = Instant::now();
        Some(RawFrame {
            rgba,
            width: w,
            height: h,
            rotation_deg: self.rotation_deg,
        })
    }

    unsafe fn copy_out(
        &mut self,
        info: DXGI_OUTDUPL_FRAME_INFO,
        resource: Option<IDXGIResource>,
    ) -> Result<Option<RawFrame>, String> {
        if info.LastPresentTime == 0 {
            // No desktop change. If only the pointer moved, re-emit the last
            // frame with the cursor at its new spot so it doesn't freeze on a
            // static screen; a bare timeout yields nothing.
            return Ok(self.cursor_only_frame());
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
        // Desktop Duplication delivers the desktop WITHOUT the cursor (it's a
        // hardware overlay), so draw the real OS pointer shape in ourselves —
        // matching what macOS/Linux capture already bakes in. Keep the
        // cursor-free pixels first so a later pointer-only move can re-emit
        // (see cursor_only_frame). Compositing runs on the pre-rotation buffer,
        // so the cursor rotates with the frame downstream.
        if self.ptr_visible && self.cursor.is_some() {
            self.last_clean = Some(rgba.clone());
            self.last_dims = (w, h);
            if let Some(cur) = &self.cursor {
                composite_cursor(&mut rgba, w, h, cur, self.ptr_x, self.ptr_y);
            }
            self.last_cursor_emit = Instant::now();
        } else {
            self.last_clean = None;
        }
        Ok(Some(RawFrame {
            rgba,
            width: w,
            height: h,
            rotation_deg: self.rotation_deg,
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

/// Draw the DXGI cursor bitmap into an RGBA desktop frame with its top-left at
/// `(px, py)`, clipped to the frame. Covers the three
/// `DXGI_OUTDUPL_POINTER_SHAPE_TYPE`s (color / masked-color / monochrome); an
/// unknown type is skipped. Fully bounds-checked, so a malformed shape can only
/// draw less, never out of bounds.
fn composite_cursor(dst: &mut [u8], dw: u32, dh: u32, cur: &CursorShape, px: i32, py: i32) {
    let dw = dw as i32;
    let dh = dh as i32;
    let pitch = cur.pitch as usize;
    match cur.kind {
        // Color: 32bpp BGRA, straight alpha — the usual modern cursor.
        2 => {
            let (cw, ch) = (cur.width as i32, cur.height as i32);
            for cy in 0..ch {
                let dy = py + cy;
                if dy < 0 || dy >= dh {
                    continue;
                }
                for cx in 0..cw {
                    let dx = px + cx;
                    if dx < 0 || dx >= dw {
                        continue;
                    }
                    let s = cy as usize * pitch + cx as usize * 4;
                    if s + 3 >= cur.buf.len() {
                        continue;
                    }
                    let a = cur.buf[s + 3] as u32;
                    if a == 0 {
                        continue;
                    }
                    let (b, g, r) = (
                        cur.buf[s] as u32,
                        cur.buf[s + 1] as u32,
                        cur.buf[s + 2] as u32,
                    );
                    let d = (dy as usize * dw as usize + dx as usize) * 4;
                    dst[d] = ((r * a + dst[d] as u32 * (255 - a)) / 255) as u8;
                    dst[d + 1] = ((g * a + dst[d + 1] as u32 * (255 - a)) / 255) as u8;
                    dst[d + 2] = ((b * a + dst[d + 2] as u32 * (255 - a)) / 255) as u8;
                }
            }
        }
        // Masked color: 32bpp BGRA where the alpha byte is a 1-bit mask —
        // 0 = paint the RGB opaque, 0xFF = XOR the RGB onto the screen.
        4 => {
            let (cw, ch) = (cur.width as i32, cur.height as i32);
            for cy in 0..ch {
                let dy = py + cy;
                if dy < 0 || dy >= dh {
                    continue;
                }
                for cx in 0..cw {
                    let dx = px + cx;
                    if dx < 0 || dx >= dw {
                        continue;
                    }
                    let s = cy as usize * pitch + cx as usize * 4;
                    if s + 3 >= cur.buf.len() {
                        continue;
                    }
                    let (b, g, r, a) = (cur.buf[s], cur.buf[s + 1], cur.buf[s + 2], cur.buf[s + 3]);
                    let d = (dy as usize * dw as usize + dx as usize) * 4;
                    if a == 0 {
                        dst[d] = r;
                        dst[d + 1] = g;
                        dst[d + 2] = b;
                    } else {
                        dst[d] ^= r;
                        dst[d + 1] ^= g;
                        dst[d + 2] ^= b;
                    }
                }
            }
        }
        // Monochrome: two stacked 1bpp masks (AND over XOR); the real height is
        // half `Height`. AND=0 -> opaque (XOR selects black/white); AND=1,XOR=1
        // -> invert the screen; AND=1,XOR=0 -> transparent.
        1 => {
            let cw = cur.width as i32;
            let ch = (cur.height / 2) as i32;
            for cy in 0..ch {
                let dy = py + cy;
                if dy < 0 || dy >= dh {
                    continue;
                }
                for cx in 0..cw {
                    let dx = px + cx;
                    if dx < 0 || dx >= dw {
                        continue;
                    }
                    let byte = cx as usize / 8;
                    let bit = 7 - (cx as usize % 8);
                    let and_at = cy as usize * pitch + byte;
                    let xor_at = (cy + ch) as usize * pitch + byte;
                    if and_at >= cur.buf.len() || xor_at >= cur.buf.len() {
                        continue;
                    }
                    let and_bit = (cur.buf[and_at] >> bit) & 1;
                    let xor_bit = (cur.buf[xor_at] >> bit) & 1;
                    let d = (dy as usize * dw as usize + dx as usize) * 4;
                    if and_bit == 0 {
                        let v = if xor_bit == 1 { 255 } else { 0 };
                        dst[d] = v;
                        dst[d + 1] = v;
                        dst[d + 2] = v;
                    } else if xor_bit == 1 {
                        dst[d] = 255 - dst[d];
                        dst[d + 1] = 255 - dst[d + 1];
                        dst[d + 2] = 255 - dst[d + 2];
                    }
                }
            }
        }
        _ => {}
    }
}
