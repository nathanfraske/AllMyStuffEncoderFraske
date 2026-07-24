//! Hardware H.264 encode on **Windows via Media Foundation** — the GPU's own
//! H.264 Media Foundation Transform (MFT): `NVIDIA H.264 Encoder MFT` (NVENC,
//! our fleet's gold standard), Intel QuickSync, or AMD VCE, whichever the box
//! actually has. The ladder in `video.rs` enumerates the platform's hardware
//! MFTs (sorted best-first by MF), opens each, and frame-send-tests it; the
//! first that emits an access unit wins, else it steps down to software
//! openh264.
//!
//! Why MF and not FFmpeg here: FFmpeg's vendor encoders need libav* dev libs +
//! pkg-config/vcpkg at build time — a toolchain end users (and our own Windows
//! host) don't carry, which is exactly what broke the build. Media Foundation
//! ships *inside* Windows and we already link the `windows` crate for DXGI
//! capture, so this path adds **no new build dependency** and works on any
//! Windows config with a hardware H.264 encoder.
//!
//! Input is the same contiguous I420 the software path produces
//! (`allmystuff_pixels::scale_rgba_to_i420`); we interleave its chroma to NV12
//! (the format every hardware H.264 MFT accepts) and feed system-memory
//! samples. Output is an Annex-B byte stream — same seam as openh264/FFmpeg:
//! I420 in, Annex-B H.264 out.

use std::ffi::c_void;
use std::sync::Once;
use std::time::{Duration, Instant};

use windows::core::{Interface, GUID, PWSTR};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED};
use windows::Win32::System::Variant::VARIANT;

/// MF version word: `(MF_SDK_VERSION << 16) | MF_API_VERSION` = `0x0002_0070`.
/// Hard-coded because the `windows` crate doesn't export the composed constant.
const MF_VERSION_WORD: u32 = 0x0002_0070;

static MF_STARTUP: Once = Once::new();

/// Start Media Foundation once for the process (refcounted with `MFShutdown`,
/// which we never call — process lifetime). Idempotent.
fn ensure_mf_started() {
    MF_STARTUP.call_once(|| unsafe {
        let _ = MFStartup(MF_VERSION_WORD, MFSTARTUP_NOSOCKET);
    });
}

/// COM-initialize the *calling thread* (MTA), best-effort. Each route's
/// capture/encode thread builds and drives its own encoder, so every thread
/// that touches MF must be initialized; `S_FALSE`/`RPC_E_CHANGED_MODE` mean
/// it already was, which is fine. We don't `CoUninitialize` — the thread lives
/// for the route and tears down with it.
fn ensure_com_thread() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

/// One enumerated hardware H.264 encoder MFT, not yet activated. Holds the
/// activation handle and the MFT's friendly name (for logs — the user sees
/// e.g. "NVIDIA H.264 Encoder MFT" confirming hardware is in play).
pub struct HwEncoder {
    activate: IMFActivate,
    name: String,
}

impl HwEncoder {
    /// The MFT's friendly name, e.g. `"NVIDIA H.264 Encoder MFT"`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Activate and configure this MFT for `width`×`height` at `fps`/`bitrate`.
    /// Returns `Err` if it won't activate or accept our NV12-in/H264-out types
    /// (wrong driver state, unsupported size) — the ladder steps to the next.
    pub fn open(
        &self,
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
    ) -> Result<MediaFoundationH264, String> {
        ensure_com_thread();
        unsafe { self.open_inner(width, height, fps, bitrate) }
    }

    unsafe fn open_inner(
        &self,
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
    ) -> Result<MediaFoundationH264, String> {
        let name = self.name.clone();

        // Activate the MFT into an IMFTransform.
        let transform: IMFTransform = self
            .activate
            .ActivateObject()
            .map_err(mferr(&name, "activate"))?;

        // Async (hardware) MFTs must be unlocked and driven by events; sync MFTs
        // use the plain ProcessInput/ProcessOutput model. Detect which.
        let attrs = transform.GetAttributes().ok();
        let is_async = attrs
            .as_ref()
            .and_then(|a| a.GetUINT32(&MF_TRANSFORM_ASYNC).ok())
            .unwrap_or(0)
            == 1;
        if let Some(a) = &attrs {
            if is_async {
                let _ = a.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1);
            }
            // Low-latency mode: no reordering/lookahead buffering — what a live
            // screen stream needs. Best-effort (older MFTs may ignore it).
            let _ = a.SetUINT32(&MF_LOW_LATENCY, 1);
        }

        // H.264 encoders require the OUTPUT type set before the input type.
        let out_type = MFCreateMediaType().map_err(mferr(&name, "create out type"))?;
        out_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(mferr(&name, "out major"))?;
        out_type
            .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)
            .map_err(mferr(&name, "out subtype"))?;
        out_type
            .SetUINT32(&MF_MT_AVG_BITRATE, bitrate)
            .map_err(mferr(&name, "out bitrate"))?;
        out_type
            .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(mferr(&name, "out interlace"))?;
        set_attr_size(&out_type, &MF_MT_FRAME_SIZE, width, height)
            .map_err(mferr(&name, "out size"))?;
        set_attr_size(&out_type, &MF_MT_FRAME_RATE, fps.max(1), 1)
            .map_err(mferr(&name, "out rate"))?;
        set_attr_size(&out_type, &MF_MT_PIXEL_ASPECT_RATIO, 1, 1)
            .map_err(mferr(&name, "out par"))?;
        // Main profile — broad decoder support; best-effort.
        let _ = out_type.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Main.0 as u32);
        transform
            .SetOutputType(0, &out_type, 0)
            .map_err(mferr(&name, "set out type"))?;

        // Input: NV12 system memory (every hardware H.264 MFT accepts it).
        let in_type = MFCreateMediaType().map_err(mferr(&name, "create in type"))?;
        in_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(mferr(&name, "in major"))?;
        in_type
            .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)
            .map_err(mferr(&name, "in subtype"))?;
        in_type
            .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(mferr(&name, "in interlace"))?;
        set_attr_size(&in_type, &MF_MT_FRAME_SIZE, width, height)
            .map_err(mferr(&name, "in size"))?;
        set_attr_size(&in_type, &MF_MT_FRAME_RATE, fps.max(1), 1)
            .map_err(mferr(&name, "in rate"))?;
        set_attr_size(&in_type, &MF_MT_PIXEL_ASPECT_RATIO, 1, 1).map_err(mferr(&name, "in par"))?;
        transform
            .SetInputType(0, &in_type, 0)
            .map_err(mferr(&name, "set in type"))?;

        // ICodecAPI knobs — all best-effort: the encoder works on defaults if a
        // particular box doesn't expose one. The rate control is the crucial
        // part: **peak-constrained VBR with a peak (MaxBitRate ≈ 2× average) and
        // a ~1 s VBV (BufferSize)** so a fast-motion / scene-change frame can
        // spend more than the average byte budget instead of having its QP
        // cranked into macroblocking. Bare CBR with mean-rate-only (the old
        // setting) starved exactly those frames — the "blocky on fast motion"
        // symptom. GOP is a backstop (≈4 s); forced IDRs handle the tight
        // recovery cadence. Plus the ability to force a keyframe on demand.
        let codecapi = transform.cast::<ICodecAPI>().ok();
        if let Some(api) = &codecapi {
            let peak = bitrate.saturating_mul(2);
            let _ = api.SetValue(
                &CODECAPI_AVEncCommonRateControlMode,
                &variant_u32(eAVEncCommonRateControlMode_PeakConstrainedVBR.0 as u32),
            );
            let _ = api.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &variant_u32(bitrate));
            let _ = api.SetValue(&CODECAPI_AVEncCommonMaxBitRate, &variant_u32(peak));
            let _ = api.SetValue(&CODECAPI_AVEncCommonBufferSize, &variant_u32(bitrate));
            let _ = api.SetValue(&CODECAPI_AVEncCommonLowLatency, &variant_bool(true));
            let _ = api.SetValue(
                &CODECAPI_AVEncMPVGOPSize,
                &variant_u32(fps.saturating_mul(4).max(1)),
            );
        }

        // Event generator for the async (hardware) drive model.
        let events = if is_async {
            Some(
                transform
                    .cast::<IMFMediaEventGenerator>()
                    .map_err(mferr(&name, "event generator"))?,
            )
        } else {
            None
        };

        transform
            .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
            .map_err(mferr(&name, "begin streaming"))?;
        transform
            .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
            .map_err(mferr(&name, "start of stream"))?;

        // Whether the MFT hands us its own output samples (hardware MFTs do) or
        // we must supply the buffer.
        let info = transform
            .GetOutputStreamInfo(0)
            .map_err(mferr(&name, "out stream info"))?;
        let provides_samples = info.dwFlags
            & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32
                | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0 as u32)
            != 0;
        let out_buf_size = (info.cbSize as usize).max((width as usize * height as usize) + 4096);

        Ok(MediaFoundationH264 {
            transform,
            events,
            codecapi,
            is_async,
            name: self.name.clone(),
            width,
            height,
            fps: fps.max(1),
            provides_samples,
            out_buf_size,
            frame_index: 0,
            nv12: Vec::new(),
        })
    }
}

/// Enumerate the box's **hardware** H.264 encoder MFTs, best-first (MF's own
/// sort/filter). Empty when none exists (no GPU encoder, RDP session, locked
/// down driver) — the ladder then falls to software openh264.
pub fn hardware_h264_mfts() -> Vec<HwEncoder> {
    ensure_mf_started();
    ensure_com_thread();
    unsafe { enum_hw_h264() }
}

unsafe fn enum_hw_h264() -> Vec<HwEncoder> {
    let out_info = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count: u32 = 0;
    // Hardware encoders, output H.264; SORTANDFILTER puts the preferred MFT
    // first and drops disabled ones.
    let flags = MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER;
    if MFTEnumEx(
        MFT_CATEGORY_VIDEO_ENCODER,
        flags,
        None,
        Some(&out_info),
        &mut activates,
        &mut count,
    )
    .is_err()
        || activates.is_null()
        || count == 0
    {
        if !activates.is_null() {
            CoTaskMemFree(Some(activates as *const c_void));
        }
        return Vec::new();
    }

    let mut result = Vec::new();
    let slice = std::slice::from_raw_parts_mut(activates, count as usize);
    for slot in slice.iter_mut() {
        // take() moves ownership out of the array (leaving None), so the COM
        // ref transfers to us — dropped with HwEncoder, never leaked.
        if let Some(act) = slot.take() {
            let name = friendly_name(&act);
            result.push(HwEncoder {
                activate: act,
                name,
            });
        }
    }
    // The array memory itself is CoTaskMem; its element refs are now ours.
    CoTaskMemFree(Some(activates as *const c_void));
    result
}

/// The `MFT_FRIENDLY_NAME_Attribute` string, or a generic label if absent.
unsafe fn friendly_name(act: &IMFActivate) -> String {
    let mut ptr = PWSTR::null();
    let mut len: u32 = 0;
    match act.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut ptr, &mut len) {
        Ok(()) if !ptr.is_null() => {
            let s = ptr.to_string().unwrap_or_default();
            CoTaskMemFree(Some(ptr.0 as *const c_void));
            if s.is_empty() {
                "H.264 hardware MFT".to_string()
            } else {
                s
            }
        }
        _ => "H.264 hardware MFT".to_string(),
    }
}

/// One opened Media Foundation hardware H.264 encoder. Created and driven
/// entirely on its route's capture/encode thread.
pub struct MediaFoundationH264 {
    transform: IMFTransform,
    /// `Some` for async (hardware) MFTs — the event source we pump.
    events: Option<IMFMediaEventGenerator>,
    /// `Some` when the MFT exposes the codec API (force-keyframe, rate control).
    codecapi: Option<ICodecAPI>,
    is_async: bool,
    name: String,
    width: u32,
    height: u32,
    fps: u32,
    /// Whether the MFT allocates its own output samples.
    provides_samples: bool,
    out_buf_size: usize,
    frame_index: i64,
    /// Reused NV12 scratch so a 4K frame doesn't allocate per encode.
    nv12: Vec<u8>,
}

// SAFETY: a MediaFoundationH264 is built on, and only ever used from, the single
// capture/encode thread that owns its route's stream (see `video.rs` —
// `H264Stream` lives on that thread and is never shared). The COM interfaces it
// holds are therefore never touched from two threads at once. The `Send` bound
// exists only because the `H264Codec` trait object is nominally `Send`; the
// value does not actually migrate between threads after construction.
unsafe impl Send for MediaFoundationH264 {}

impl MediaFoundationH264 {
    pub fn label(&self) -> &str {
        &self.name
    }

    /// Encode one contiguous I420 frame (`width*height` Y, then quarter-size U,
    /// then V). Returns the Annex-B access unit + whether it was a keyframe, or
    /// `None` when the encoder produced nothing this call (buffering).
    pub fn encode_i420(
        &mut self,
        i420: &[u8],
        force_idr: bool,
    ) -> Result<Option<(Vec<u8>, bool)>, String> {
        let (w, h) = (self.width as usize, self.height as usize);
        let ysize = w * h;
        let csize = (w / 2) * (h / 2);
        if i420.len() < ysize + 2 * csize {
            return Err(format!(
                "{}: short I420 ({} < {})",
                self.name,
                i420.len(),
                ysize + 2 * csize
            ));
        }
        // I420 (planar Y,U,V) → NV12 (Y plane, then interleaved U,V). Same total
        // size; every hardware H.264 MFT takes NV12.
        self.nv12.resize(ysize + 2 * csize, 0);
        self.nv12[..ysize].copy_from_slice(&i420[..ysize]);
        let u = &i420[ysize..ysize + csize];
        let v = &i420[ysize + csize..ysize + 2 * csize];
        let uv = &mut self.nv12[ysize..];
        for i in 0..csize {
            uv[2 * i] = u[i];
            uv[2 * i + 1] = v[i];
        }

        let duration = 10_000_000i64 / i64::from(self.fps);
        let time = self.frame_index * duration;
        self.frame_index += 1;

        unsafe {
            let sample = make_input_sample(&self.nv12, time, duration)
                .map_err(|e| format!("{}: input sample: {e}", self.name))?;
            if force_idr {
                self.force_keyframe();
            }
            if self.is_async {
                self.pump_async(&sample)
            } else {
                self.pump_sync(&sample)
            }
        }
    }

    /// Request the next encoded frame be an IDR (viewer asked for a clean entry,
    /// or our adaptive cadence is due). Best-effort — if the codec API isn't
    /// exposed, the encoder's GOP still yields periodic keyframes.
    unsafe fn force_keyframe(&self) {
        if let Some(api) = &self.codecapi {
            let _ = api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &variant_u32(1));
        }
    }

    /// Synchronous MFT: feed one input, drain every output it produces.
    unsafe fn pump_sync(&mut self, sample: &IMFSample) -> Result<Option<(Vec<u8>, bool)>, String> {
        self.transform
            .ProcessInput(0, sample, 0)
            .map_err(|e| format!("{}: ProcessInput: {e}", self.name))?;
        let mut data = Vec::new();
        let mut key = false;
        let mut got = false;
        while let Some((d, k)) = self.process_output()? {
            data.extend_from_slice(&d);
            key |= k;
            got = true;
        }
        Ok(if got { Some((data, key)) } else { None })
    }

    /// Async (hardware) MFT: drive the event model. Feed our one frame on the
    /// first `METransformNeedInput`, then collect its `METransformHaveOutput`.
    /// Polls with `NO_WAIT` + a deadline so a quirky driver can never deadlock
    /// the encode thread — output that lags by a frame is picked up next call.
    unsafe fn pump_async(&mut self, sample: &IMFSample) -> Result<Option<(Vec<u8>, bool)>, String> {
        let events = match &self.events {
            Some(e) => e.clone(),
            None => return Err(format!("{}: async MFT without event generator", self.name)),
        };
        let mut fed = false;
        let mut out: Option<(Vec<u8>, bool)> = None;
        let start = Instant::now();
        let deadline = Duration::from_millis(1000);
        loop {
            let event = match events.GetEvent(MF_EVENT_FLAG_NO_WAIT) {
                Ok(ev) => ev,
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => {
                    if out.is_some() {
                        return Ok(out);
                    }
                    if start.elapsed() >= deadline {
                        // Nothing yet — treat as buffering. The frame-send test
                        // retries, and a live stream picks the unit up next tick.
                        return Ok(None);
                    }
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                }
                Err(e) => return Err(format!("{}: GetEvent: {e}", self.name)),
            };
            let met = event
                .GetType()
                .map_err(|e| format!("{}: event type: {e}", self.name))?;
            if met == METransformNeedInput.0 as u32 {
                if !fed {
                    self.transform
                        .ProcessInput(0, sample, 0)
                        .map_err(|e| format!("{}: ProcessInput: {e}", self.name))?;
                    fed = true;
                }
                // A second NeedInput means the MFT wants the *next* frame; we
                // have only this one. Keep waiting for this frame's output.
            } else if met == METransformHaveOutput.0 as u32 {
                if let Some(pkt) = self.process_output()? {
                    out = Some(pkt);
                    if fed {
                        return Ok(out);
                    }
                }
            }
        }
    }

    /// One `ProcessOutput` call. `Ok(None)` means the MFT wants more input (or a
    /// benign stream-format change); `Ok(Some)` is one access unit + key flag.
    unsafe fn process_output(&mut self) -> Result<Option<(Vec<u8>, bool)>, String> {
        // Supply an output sample only when the MFT doesn't provide its own.
        let provided = if self.provides_samples {
            None
        } else {
            Some(
                alloc_output_sample(self.out_buf_size)
                    .map_err(|e| format!("{}: alloc output: {e}", self.name))?,
            )
        };
        let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: std::mem::ManuallyDrop::new(provided),
            dwStatus: 0,
            pEvents: std::mem::ManuallyDrop::new(None),
        }];
        let mut status = 0u32;
        let r = self.transform.ProcessOutput(0, &mut buffers, &mut status);
        // Reclaim the (possibly MFT-allocated) sample regardless of outcome.
        let sample = std::mem::ManuallyDrop::take(&mut buffers[0].pSample);
        std::mem::ManuallyDrop::drop(&mut buffers[0].pEvents);
        match r {
            Ok(()) => match sample {
                Some(s) => Ok(Some(read_sample(&s)?)),
                None => Ok(None),
            },
            Err(e)
                if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT
                    || e.code() == MF_E_TRANSFORM_STREAM_CHANGE =>
            {
                Ok(None)
            }
            Err(e) => Err(format!("{}: ProcessOutput: {e}", self.name)),
        }
    }
}

impl Drop for MediaFoundationH264 {
    fn drop(&mut self) {
        unsafe {
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
        }
    }
}

/// Copy an output `IMFSample`'s bytes out and read its keyframe flag.
unsafe fn read_sample(sample: &IMFSample) -> Result<(Vec<u8>, bool), String> {
    let buffer = sample
        .ConvertToContiguousBuffer()
        .map_err(|e| format!("contiguous buffer: {e}"))?;
    let mut ptr: *mut u8 = std::ptr::null_mut();
    let mut current: u32 = 0;
    buffer
        .Lock(&mut ptr, None, Some(&mut current))
        .map_err(|e| format!("buffer lock: {e}"))?;
    let data = if ptr.is_null() || current == 0 {
        Vec::new()
    } else {
        std::slice::from_raw_parts(ptr, current as usize).to_vec()
    };
    let _ = buffer.Unlock();
    // MFSampleExtension_CleanPoint == 1 marks a keyframe (IDR).
    let key = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) == 1;
    Ok((data, key))
}

/// A fresh `IMFSample` carrying NV12 `data`, timestamped for the RTP clock.
unsafe fn make_input_sample(
    data: &[u8],
    time_100ns: i64,
    duration_100ns: i64,
) -> Result<IMFSample, windows::core::Error> {
    let sample = MFCreateSample()?;
    let buffer = MFCreateMemoryBuffer(data.len() as u32)?;
    let mut ptr: *mut u8 = std::ptr::null_mut();
    buffer.Lock(&mut ptr, None, None)?;
    if !ptr.is_null() {
        std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
    }
    buffer.Unlock()?;
    buffer.SetCurrentLength(data.len() as u32)?;
    sample.AddBuffer(&buffer)?;
    sample.SetSampleTime(time_100ns)?;
    sample.SetSampleDuration(duration_100ns)?;
    Ok(sample)
}

/// An empty output sample with a `size`-byte buffer, for MFTs that don't
/// allocate their own.
unsafe fn alloc_output_sample(size: usize) -> Result<IMFSample, windows::core::Error> {
    let sample = MFCreateSample()?;
    let buffer = MFCreateMemoryBuffer(size as u32)?;
    sample.AddBuffer(&buffer)?;
    Ok(sample)
}

/// Pack two `u32`s into the hi/lo halves of an attribute `UINT64` — the layout
/// `MFSetAttributeSize`/`MFSetAttributeRatio` use for frame size, frame rate,
/// and pixel aspect ratio.
unsafe fn set_attr_size(
    media_type: &IMFMediaType,
    key: &GUID,
    hi: u32,
    lo: u32,
) -> Result<(), windows::core::Error> {
    media_type.SetUINT64(key, ((hi as u64) << 32) | (lo as u64))
}

/// Stage-tagged error formatter for the `open` path — borrows the MFT name so a
/// failure reads e.g. `"NVIDIA H.264 Encoder MFT: set out type: <hresult>"`.
fn mferr<'a>(name: &'a str, stage: &'a str) -> impl FnOnce(windows::core::Error) -> String + 'a {
    move |e| format!("{name}: {stage}: {e}")
}

/// A `VT_UI4` VARIANT for an `ICodecAPI` numeric property.
fn variant_u32(v: u32) -> VARIANT {
    VARIANT::from(v)
}

/// A `VT_BOOL` VARIANT for an `ICodecAPI` boolean property.
fn variant_bool(v: bool) -> VARIANT {
    VARIANT::from(v)
}

/// A/B/C comparison bench (added in a throwaway worktree of the upstream
/// fork point — never committed). Mirrors the fork's bench parameters and
/// content patterns exactly, so the numbers interleave against the fork's
/// own benches on the same box:
///  - pixel-loop rows match `win_capture`/`pixels` bench shapes (4K,
///    same synthetic content, 30 iters)
///  - the MF encode row matches `bench_mf_encode_call_latency` (1440p,
///    60 fps, 30 Mbps, 150 frames, the same luma shift per frame)
/// Run: `cargo test --release -- --ignored bench_abc --nocapture --test-threads=1`
#[cfg(test)]
mod abc_bench {
    use super::*;

    fn ms(d: Duration, n: u32) -> f64 {
        d.as_secs_f64() * 1000.0 / f64::from(n)
    }

    #[test]
    #[ignore = "bench — run with --ignored --nocapture"]
    fn bench_abc_upstream_decomposition() {
        // ---- capture-side per-damage-frame costs (4K), exactly the
        // shipped upstream shapes: fresh zeroed alloc (copy_out), the
        // BGRA->RGBA swizzle compiled in THIS crate (-Os), and the
        // full-frame clone (`last_clean = Some(rgba.clone())`). The warm
        // copy row is the fork's replacement shape (reused pages),
        // measured here so both shapes come from one binary.
        let (cw, chh) = (3840usize, 2160usize);
        let pitch = cw * 4;
        let src: Vec<u8> = (0..pitch * chh).map(|i| (i % 253) as u8).collect();
        let n = 30u32;
        let t0 = Instant::now();
        for _ in 0..n {
            std::hint::black_box(vec![0u8; cw * chh * 4]);
        }
        let zalloc = ms(t0.elapsed(), n);
        let mut rgba = vec![0u8; cw * chh * 4];
        let t0 = Instant::now();
        for _ in 0..n {
            for row in 0..chh {
                let s = &src[row * pitch..][..cw * 4];
                let d = &mut rgba[row * cw * 4..][..cw * 4];
                for (dp, sp) in d.chunks_exact_mut(4).zip(s.chunks_exact(4)) {
                    dp[0] = sp[2];
                    dp[1] = sp[1];
                    dp[2] = sp[0];
                    dp[3] = 255;
                }
            }
            std::hint::black_box(&rgba);
        }
        let swizzle = ms(t0.elapsed(), n);
        let t0 = Instant::now();
        for _ in 0..n {
            std::hint::black_box(rgba.clone());
        }
        let cold_clone = ms(t0.elapsed(), n);
        let mut warm = rgba.clone();
        let t0 = Instant::now();
        for _ in 0..n {
            warm.clear();
            warm.extend_from_slice(&rgba);
            std::hint::black_box(&warm);
        }
        let warm_copy = ms(t0.elapsed(), n);
        println!(
            "bench ABC upstream capture 4K: zalloc {zalloc:7.3} ms | swizzle(-Os) {swizzle:7.3} ms | cold clone {cold_clone:7.3} ms | warm copy {warm_copy:7.3} ms"
        );

        // ---- convert (pixels crate, O3 here too): the upstream path is
        // scale_rgba_to_i420 with a fresh allocation per call. Same
        // content and iters as the fork's pixels benches.
        let synth: Vec<u8> = (0..cw * chh * 4).map(|i| (i * 31 % 251) as u8).collect();
        let native = {
            let t0 = Instant::now();
            for _ in 0..n {
                std::hint::black_box(allmystuff_pixels::scale_rgba_to_i420(
                    &synth, 3840, 2160, 3840, 2160,
                ));
            }
            ms(t0.elapsed(), n)
        };
        let scaled = {
            let t0 = Instant::now();
            for _ in 0..n {
                std::hint::black_box(allmystuff_pixels::scale_rgba_to_i420(
                    &synth, 3840, 2160, 1920, 1080,
                ));
            }
            ms(t0.elapsed(), n)
        };
        println!(
            "bench ABC upstream convert: scale_rgba_to_i420 4K->4K {native:7.3} ms | 4K->1080p {scaled:7.3} ms"
        );

        // ---- the encoder-side I420->NV12 interleave upstream pays inside
        // every encode_i420 call (the fork deleted it) — at 4K (desktop
        // native) and 1440p (the MF row's size, to subtract mentally).
        for (w, h, tag) in [(3840usize, 2160usize, "4K"), (2560usize, 1440usize, "1440p")] {
            let ysize = w * h;
            let csize = (w / 2) * (h / 2);
            let i420: Vec<u8> = (0..ysize + 2 * csize).map(|i| (i % 247) as u8).collect();
            let mut nv12 = vec![0u8; ysize + 2 * csize];
            let iters = 60u32;
            let t0 = Instant::now();
            for _ in 0..iters {
                nv12[..ysize].copy_from_slice(&i420[..ysize]);
                let u = &i420[ysize..ysize + csize];
                let v = &i420[ysize + csize..ysize + 2 * csize];
                let uv = &mut nv12[ysize..];
                for i in 0..csize {
                    uv[2 * i] = u[i];
                    uv[2 * i + 1] = v[i];
                }
                std::hint::black_box(&nv12);
            }
            println!(
                "bench ABC upstream ingest interleave {tag}: {:7.3} ms",
                ms(t0.elapsed(), iters)
            );
        }

        // ---- MF encode call (1440p, 30 Mbps, 150 frames, same luma shift
        // as the fork's bench). NOTE: upstream's encode_i420 includes the
        // interleave above in its timed cost, and its async pump can wait
        // up to 1000 ms for output — max is the stall, not noise.
        let (w, h) = (2560u32, 1440u32);
        let hw = hardware_h264_mfts();
        let Some(first) = hw.first() else {
            println!("bench ABC upstream MF encode: SKIP (no hardware MFT)");
            return;
        };
        let mut enc = match first.open(w, h, 60, 30_000_000) {
            Ok(e) => e,
            Err(e) => {
                println!("bench ABC upstream MF encode: SKIP ({e})");
                return;
            }
        };
        println!("bench ABC upstream MF encoder: {}", enc.label());
        let (wu, hu) = (w as usize, h as usize);
        let mut yuv = vec![128u8; wu * hu + 2 * ((wu / 2) * (hu / 2))];
        let mut lat: Vec<Duration> = Vec::new();
        let (mut fed, mut units, mut bytes) = (0u32, 0u32, 0u64);
        for i in 0..150u32 {
            for (j, v) in yuv[..wu * hu].iter_mut().enumerate() {
                *v = ((j as u32).wrapping_add(i.wrapping_mul(7)) % 255) as u8;
            }
            let t = Instant::now();
            let out = enc.encode_i420(&yuv, i == 0);
            lat.push(t.elapsed());
            fed += 1;
            if let Ok(Some((d, _))) = out {
                if !d.is_empty() {
                    units += 1;
                    bytes += d.len() as u64;
                }
            }
        }
        let mut ms_all: Vec<f64> = lat.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
        ms_all.sort_by(f64::total_cmp);
        let avg: f64 = ms_all.iter().sum::<f64>() / ms_all.len() as f64;
        let p95 = ms_all[(ms_all.len() * 95 / 100).min(ms_all.len() - 1)];
        let max = ms_all[ms_all.len() - 1];
        println!(
            "bench ABC upstream MF encode call 1440p: avg {avg:6.2} ms · p95 {p95:6.2} ms · max {max:6.2} ms"
        );
        println!(
            "bench ABC upstream MF units conservation: {units} units out of {fed} frames fed · {bytes} bytes"
        );
    }
}
