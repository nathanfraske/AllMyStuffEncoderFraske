//! AMD AMF encode — the Radeon twin of [`crate::nvenc`], in progress.
//!
//! Reality first: AMD boxes are NOT software-only today — the MF rung
//! picks up AMD's own hardware MFT on the same zero-copy GPU lane
//! (VideoProcessor and the DXGI device manager are vendor-neutral). What
//! this module adds is the direct-SDK rung with the levers MF hides:
//! GDR intra-refresh for the game posture
//! (`AMF_VIDEO_ENCODER_INTRA_REFRESH_NUM_MBS_PER_SLOT`), guaranteed
//! in-place bitrate, usage/quality presets for Balanced/Game/Studio.
//! Ladder order becomes NVENC → AMF (AMD adapters only) → MF → software.
//! AMF exposes no lossless mode (no transquant bypass) — Studio·Lossless
//! stays an NVIDIA-pair feature.
//!
//! Same discipline as the NVENC rung: no build dependency — `amfrt64.dll`
//! ships with the Radeon driver and is loaded at runtime; absent driver =
//! absent rung, softly. The FFI is transcribed from AMD's MIT-licensed
//! AMF headers (GPUOpen AMF v1.4.35, the C-ABI sections — FFmpeg's
//! `amfenc.c` consumes the same ABI from C and is the flow reference:
//! `AMFInit` → factory `CreateContext` → `InitDX11` on the lane device →
//! `CreateComponent` (AVC/HEVC) → property bag → `SubmitInput` D3D11
//! surfaces → `QueryOutput` → `AMFBuffer`).
//!
//! Status: the loader and version probe below are complete and testable
//! on any box (this dev machine proves the clean-skip path). The
//! component vtables (context/encoder/surface/buffer) are the next
//! transcription unit — headers staged in the session scratchpad;
//! nothing routes through this module until the encode path exists AND a
//! Radeon box has run the e2e test that currently skips everywhere else.

#![cfg(windows)]
#![allow(dead_code)]

use std::ffi::c_void;

use windows::core::PCSTR;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};

/// `AMF_RESULT` — 0 = AMF_OK.
type AmfResult = i32;
const AMF_OK: AmfResult = 0;

/// The full-version word AMF speaks (`AMF_MAKE_FULL_VERSION`): the
/// headers this FFI was transcribed from are 1.4.35; the runtime accepts
/// any caller version ≤ its own.
const AMF_VERSION: u64 = (1u64 << 48) | (4u64 << 32) | (35u64 << 16);

/// Opaque until their vtables are transcribed.
type AmfFactory = *mut c_void;

/// What the loader learned about this box's AMF runtime.
pub(crate) struct AmfRuntime {
    pub(crate) version: u64,
    factory: AmfFactory,
}

// SAFETY: the factory is a process-global AMF documents as thread-safe
// (every FFmpeg amfenc instance shares it); the raw pointer is only a
// handle, never dereferenced outside vtable calls.
unsafe impl Send for AmfRuntime {}
unsafe impl Sync for AmfRuntime {}

/// Load `amfrt64.dll` and initialize the factory once per process.
/// `Err` = no AMD driver on this box (or one predating AMF 1.4) — the
/// encoder ladder skips the rung exactly like the NVENC rung skips
/// without NVIDIA.
pub(crate) fn runtime() -> Result<&'static AmfRuntime, String> {
    static RT: std::sync::OnceLock<Result<&'static AmfRuntime, String>> =
        std::sync::OnceLock::new();
    RT.get_or_init(|| unsafe {
        let module = LoadLibraryA(PCSTR(c"amfrt64.dll".as_ptr() as *const u8))
            .map_err(|e| format!("amfrt64.dll not loadable (no AMD driver): {e}"))?;
        let query = GetProcAddress(module, PCSTR(c"AMFQueryVersion".as_ptr() as *const u8))
            .ok_or("AMFQueryVersion missing")?;
        let query: unsafe extern "C" fn(*mut u64) -> AmfResult = std::mem::transmute(query);
        let mut version = 0u64;
        let status = query(&mut version);
        if status != AMF_OK {
            return Err(format!("AMFQueryVersion: {status}"));
        }
        tracing::info!(
            "AMF runtime {}.{}.{} present",
            (version >> 48) & 0xffff,
            (version >> 32) & 0xffff,
            (version >> 16) & 0xffff,
        );
        let init = GetProcAddress(module, PCSTR(c"AMFInit".as_ptr() as *const u8))
            .ok_or("AMFInit missing")?;
        let init: unsafe extern "C" fn(u64, *mut AmfFactory) -> AmfResult =
            std::mem::transmute(init);
        let mut factory: AmfFactory = std::ptr::null_mut();
        let status = init(AMF_VERSION, &mut factory);
        if status != AMF_OK || factory.is_null() {
            return Err(format!("AMFInit: {status}"));
        }
        Ok(&*Box::leak(Box::new(AmfRuntime { version, factory })))
    })
    .clone()
}

/// Whether the encoder ladder should even try this rung: AMF loads on
/// the box AND the lane's adapter is AMD (vendor 0x1002) — a GeForce
/// box with a Radeon iGPU shouldn't pay an AMF init on the wrong device,
/// and a pure-NVIDIA box shouldn't touch the DLL at all.
pub(crate) fn worth_trying(adapter_vendor_id: u32) -> bool {
    adapter_vendor_id == 0x1002 && runtime().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rung's absence contract, provable on any box: on non-AMD
    /// hardware the loader reports a clean, specific error (no panic, no
    /// partial init) and `worth_trying` is false for every vendor id —
    /// the exact behavior the encoder ladder depends on to skip past.
    /// On a Radeon box this same test instead proves the runtime loads
    /// and reports a version.
    #[test]
    fn amf_loader_fails_soft_or_loads() {
        match runtime() {
            Ok(rt) => {
                assert!(rt.version > 0, "a real version word");
                println!(
                    "AMF present: {}.{}.{}",
                    (rt.version >> 48) & 0xffff,
                    (rt.version >> 32) & 0xffff,
                    (rt.version >> 16) & 0xffff
                );
                assert!(worth_trying(0x1002), "AMD adapter + runtime = try");
            }
            Err(e) => {
                println!("AMF absent (expected on non-AMD): {e}");
                assert!(!worth_trying(0x1002), "no runtime = never try");
            }
        }
        assert!(!worth_trying(0x10DE), "NVIDIA adapter never tries AMF");
        assert!(!worth_trying(0x8086), "Intel adapter never tries AMF");
    }
}
