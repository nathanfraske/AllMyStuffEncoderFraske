//! The GPU frame lane (Windows): keep captured pixels on the GPU from
//! duplication to encoder.
//!
//! The interleaved A/B on the pipelined pump proved the CPU lane's limiter
//! is the copies themselves — per frame today: a staging `Map` + BGRA→RGBA
//! swizzle (~15 MB), a copy-out, an RGBA→NV12 convert (~5.5 MB), and the
//! MFT's own system-memory ingest. This module deletes all of them for the
//! hardware-H.264 route: the desktop texture is color-converted
//! BGRA→NV12 **on the GPU** by the D3D11 VideoProcessor (the fixed-function
//! block every capable GPU carries), and the NV12 *texture* is handed to
//! the encoder MFT through an `IMFDXGIDeviceManager` — the encoder reads
//! it in place. CPU touches per frame: none.
//!
//! Ownership: one [`GpuConvert`] per route owns the D3D11 device
//! (multithread-protected — Media Foundation requires it), the video
//! processor, a small NV12 output-texture ring, and the device manager the
//! MFT is opened with. The capture side will create its duplication on
//! this same device (adapter affinity is what makes the whole chain
//! zero-copy), which also decides the policy question the adapter pin
//! raised: a pinned cross-adapter encode keeps the CPU lane.
//!
//! This module lands **proven but unwired**: the `gpu_lane_end_to_end`
//! test drives synthetic BGRA textures through convert → MFT → openh264
//! decode and asserts picture validity, so the novel COM plumbing is
//! validated in isolation before the capture/pump integration slice
//! switches the live path over.

#![cfg(windows)]

use windows::core::Interface;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D,
    ID3D11VideoContext, ID3D11VideoDevice, ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator,
    ID3D11VideoProcessorInputView, ID3D11VideoProcessorOutputView, D3D11_BIND_RENDER_TARGET,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
    D3D11_VIDEO_PROCESSOR_COLOR_SPACE, D3D11_VIDEO_PROCESSOR_CONTENT_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_STREAM, D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
    D3D11_VPIV_DIMENSION_TEXTURE2D, D3D11_VPOV_DIMENSION_TEXTURE2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC,
};
use windows::Win32::Media::MediaFoundation::{IMFDXGIDeviceManager, MFCreateDXGIDeviceManager};

/// How many NV12 output textures cycle in the ring. The encoder holds a
/// texture only while it reads it (the MFT resolves the DXGI buffer during
/// `ProcessInput`); four gives generous slack over its 1–2 frame depth, and
/// the end-to-end decode test is the corruption tripwire for that
/// assumption — the same discipline the CPU-side ring was held to (and
/// dropped under; this one earns its keep by deleting *all* the copies,
/// not just the allocation).
const NV12_RING: usize = 4;

/// The per-route GPU conversion stage: BGRA texture in, NV12 texture out,
/// plus the device manager the encoder MFT opens with.
pub struct GpuConvert {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    processor: ID3D11VideoProcessor,
    enumerator: ID3D11VideoProcessorEnumerator,
    manager: IMFDXGIDeviceManager,
    in_size: (u32, u32),
    out_size: (u32, u32),
    ring: Vec<(ID3D11Texture2D, ID3D11VideoProcessorOutputView)>,
    ring_next: usize,
}

// SAFETY: the device is created multithread-protected (below) and a
// GpuConvert is owned and driven by one route's threads; the COM interfaces
// it holds are only used through &mut self or handed to MF, which manages
// its own synchronization through the device manager.
unsafe impl Send for GpuConvert {}

impl GpuConvert {
    /// Build the conversion stage for `in_w`×`in_h` BGRA frames fitted to
    /// `out_w`×`out_h` NV12 (the video processor scales when they differ —
    /// GPU downscale replaces the CPU `fit_within` path on this lane).
    /// Fails soft: any missing capability sends the ladder back to the CPU
    /// lane.
    pub fn new(in_w: u32, in_h: u32, out_w: u32, out_h: u32) -> Result<Self, String> {
        unsafe {
            let mut device: Option<ID3D11Device> = None;
            let mut context: Option<ID3D11DeviceContext> = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE(std::ptr::null_mut()),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .map_err(|e| format!("D3D11CreateDevice (video): {e}"))?;
            let device = device.ok_or("D3D11CreateDevice returned no device")?;
            let context = context.ok_or("D3D11CreateDevice returned no context")?;
            // Media Foundation shares the device across its own worker
            // threads; without multithread protection that's a data race
            // inside D3D.
            let mt: ID3D11Multithread = context
                .cast()
                .map_err(|e| format!("ID3D11Multithread: {e}"))?;
            let _ = mt.SetMultithreadProtected(true);

            let video_device: ID3D11VideoDevice = device
                .cast()
                .map_err(|e| format!("ID3D11VideoDevice: {e}"))?;
            let video_context: ID3D11VideoContext = context
                .cast()
                .map_err(|e| format!("ID3D11VideoContext: {e}"))?;

            let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputWidth: in_w,
                InputHeight: in_h,
                OutputWidth: out_w,
                OutputHeight: out_h,
                Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
                ..Default::default()
            };
            let enumerator = video_device
                .CreateVideoProcessorEnumerator(&desc)
                .map_err(|e| format!("CreateVideoProcessorEnumerator: {e}"))?;
            let processor = video_device
                .CreateVideoProcessor(&enumerator, 0)
                .map_err(|e| format!("CreateVideoProcessor: {e}"))?;

            // Colour spaces: full-range RGB in, BT.709 limited-range YCbCr
            // out — the HD convention decoders assume for HD streams. (The
            // CPU lane writes BT.601 for everything; 709 here is the more
            // correct choice at these resolutions, and the lanes never mix
            // within one stream.) The bitfield layout is
            // Usage:1 | RGB_Range:1 | YCbCr_Matrix:1 | YCbCr_xvYCC:1 |
            // Nominal_Range:2 — matrix 709 = bit 2, nominal 16..235 = 0b01
            // at bits 4..5.
            let stream_cs = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0 };
            let output_cs = D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
                _bitfield: (1 << 2) | (1 << 4),
            };
            video_context.VideoProcessorSetStreamColorSpace(&processor, 0, &stream_cs);
            video_context.VideoProcessorSetOutputColorSpace(&processor, &output_cs);

            let mut reset_token = 0u32;
            let mut manager: Option<IMFDXGIDeviceManager> = None;
            MFCreateDXGIDeviceManager(&mut reset_token, &mut manager)
                .map_err(|e| format!("MFCreateDXGIDeviceManager: {e}"))?;
            let manager = manager.ok_or("MFCreateDXGIDeviceManager returned nothing")?;
            manager
                .ResetDevice(&device, reset_token)
                .map_err(|e| format!("DXGI device manager ResetDevice: {e}"))?;

            Ok(GpuConvert {
                device,
                context,
                video_device,
                video_context,
                processor,
                enumerator,
                manager,
                in_size: (in_w, in_h),
                out_size: (out_w, out_h),
                ring: Vec::new(),
                ring_next: 0,
            })
        }
    }

    /// The device manager the encoder MFT must be opened with for this lane
    /// (via `MFT_MESSAGE_SET_D3D_MANAGER`).
    pub fn manager(&self) -> IMFDXGIDeviceManager {
        self.manager.clone()
    }

    /// The device the capture side should create its duplication on so the
    /// whole chain shares one adapter and zero copies.
    pub fn device(&self) -> ID3D11Device {
        self.device.clone()
    }

    pub fn context(&self) -> ID3D11DeviceContext {
        self.context.clone()
    }

    /// The fitted output size this stage was built for.
    pub fn out_size(&self) -> (u32, u32) {
        self.out_size
    }

    /// The source size this stage expects (the duplication's frame size).
    pub fn in_size(&self) -> (u32, u32) {
        self.in_size
    }

    /// Convert one BGRA texture (created on this stage's device, `in` size)
    /// to the next NV12 ring texture, scaling to the fitted output size.
    /// The returned texture stays valid until [`NV12_RING`] further calls.
    pub fn convert(&mut self, bgra: &ID3D11Texture2D) -> Result<ID3D11Texture2D, String> {
        unsafe {
            if self.ring.is_empty() {
                self.build_ring()?;
            }
            let idx = self.ring_next % NV12_RING;
            self.ring_next = self.ring_next.wrapping_add(1);

            let in_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                ..Default::default()
            };
            let mut input_view: Option<ID3D11VideoProcessorInputView> = None;
            self.video_device
                .CreateVideoProcessorInputView(
                    bgra,
                    &self.enumerator,
                    &in_desc,
                    Some(&mut input_view),
                )
                .map_err(|e| format!("CreateVideoProcessorInputView: {e}"))?;
            let input_view = input_view.ok_or("no input view")?;

            let (out_tex, out_view) = &self.ring[idx];
            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                OutputIndex: 0,
                InputFrameOrField: 0,
                pInputSurface: std::mem::ManuallyDrop::new(Some(input_view)),
                ..Default::default()
            };
            let streams = [stream];
            let blt = self
                .video_context
                .VideoProcessorBlt(&self.processor, out_view, 0, &streams);
            // Reclaim the stream's ManuallyDrop'd view reference before
            // checking the result, or an error path leaks it.
            for mut s in streams {
                let _ = std::mem::ManuallyDrop::take(&mut s.pInputSurface);
            }
            blt.map_err(|e| format!("VideoProcessorBlt: {e}"))?;
            Ok(out_tex.clone())
        }
    }

    unsafe fn build_ring(&mut self) -> Result<(), String> {
        let (w, h) = self.out_size;
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let video_device = self.video_device.clone();
        for _ in 0..NV12_RING {
            let mut tex: Option<ID3D11Texture2D> = None;
            self.device
                .CreateTexture2D(&desc, None, Some(&mut tex))
                .map_err(|e| format!("CreateTexture2D (NV12): {e}"))?;
            let tex = tex.ok_or("CreateTexture2D returned no texture")?;
            let out_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                ..Default::default()
            };
            let mut view: Option<ID3D11VideoProcessorOutputView> = None;
            video_device
                .CreateVideoProcessorOutputView(&tex, &self.enumerator, &out_desc, Some(&mut view))
                .map_err(|e| format!("CreateVideoProcessorOutputView: {e}"))?;
            let view = view.ok_or("no output view")?;
            self.ring.push((tex, view));
        }
        Ok(())
    }

    /// A BGRA texture on this device initialised from tightly packed BGRA
    /// bytes — the integration slice's seam for the duplication copy, and
    /// what the end-to-end test feeds with synthetic frames.
    pub fn bgra_texture_from(
        &self,
        bgra: &[u8],
        w: u32,
        h: u32,
    ) -> Result<ID3D11Texture2D, String> {
        let need = (w as usize) * (h as usize) * 4;
        if bgra.len() < need {
            return Err(format!("BGRA bytes too short: {} < {need}", bgra.len()));
        }
        unsafe {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: 0,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let init = windows::Win32::Graphics::Direct3D11::D3D11_SUBRESOURCE_DATA {
                pSysMem: bgra.as_ptr() as *const core::ffi::c_void,
                SysMemPitch: w * 4,
                SysMemSlicePitch: 0,
            };
            let mut tex: Option<ID3D11Texture2D> = None;
            self.device
                .CreateTexture2D(&desc, Some(&init), Some(&mut tex))
                .map_err(|e| format!("CreateTexture2D (BGRA): {e}"))?;
            tex.ok_or_else(|| "CreateTexture2D returned no texture".to_string())
        }
    }

    /// Overwrite an existing BGRA texture's pixels — the per-frame update
    /// path for tests and the pre-integration seam.
    pub fn update_bgra(&self, tex: &ID3D11Texture2D, bgra: &[u8], w: u32, h: u32) {
        unsafe {
            self.context.UpdateSubresource(
                tex,
                0,
                None,
                bgra.as_ptr() as *const core::ffi::c_void,
                w * 4,
                (w * 4) * h,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole GPU lane in isolation: synthetic BGRA frames → texture →
    /// VideoProcessor NV12 → hardware MFT (opened on the shared device
    /// manager, fed textures — zero CPU pixel work) → Annex-B → openh264
    /// decode. Dimensions and luma orientation are asserted on the decoded
    /// picture, so a colour-space, stride, or ring-reuse mistake fails here
    /// before the live pipeline ever switches over. Skips (passing) without
    /// capable hardware.
    #[test]
    fn gpu_lane_end_to_end() {
        let (w, h) = (640u32, 480u32);
        let mut gpu = match GpuConvert::new(w, h, w, h) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("SKIP: GPU convert unavailable: {e}");
                return;
            }
        };
        let hw = crate::mediafoundation::hardware_h264_mfts();
        let Some(first) = hw.first() else {
            eprintln!("SKIP: no hardware H.264 MFT");
            return;
        };
        let mut enc = match first.open_with_manager(w, h, 30, 4_000_000, Some(&gpu.manager())) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("SKIP: MFT open with device manager failed: {e}");
                return;
            }
        };

        // A left-dark/right-bright frame whose bright edge marches down —
        // every frame differs, and decoded luma orientation is checkable.
        let mut bgra = vec![0u8; (w * h * 4) as usize];
        let tex = match gpu.bgra_texture_from(&bgra, w, h) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("SKIP: BGRA texture: {e}");
                return;
            }
        };

        let mut units: Vec<(Vec<u8>, bool)> = Vec::new();
        let mut fed = 0u32;
        for i in 0..60u32 {
            for row in 0..h as usize {
                let bright = (row as u32).is_multiple_of(2) == i.is_multiple_of(2);
                let v = if bright { 220u8 } else { 40u8 };
                let line = &mut bgra[row * (w as usize) * 4..][..(w as usize) * 4];
                for px in line.chunks_exact_mut(4) {
                    px[0] = v;
                    px[1] = v;
                    px[2] = v;
                    px[3] = 255;
                }
            }
            gpu.update_bgra(&tex, &bgra, w, h);
            let nv12 = match gpu.convert(&tex) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("SKIP: VideoProcessor convert failed: {e}");
                    return;
                }
            };
            let out = enc.encode_texture(&nv12, i == 0).expect("texture encode");
            if out.consumed {
                fed += 1;
            }
            units.extend(out.units);
        }
        for _ in 0..3 {
            let nv12 = gpu.convert(&tex).expect("drain convert");
            let out = enc.encode_texture(&nv12, false).expect("drain");
            if out.consumed {
                fed += 1;
            }
            units.extend(out.units);
        }
        assert!(
            units.len() as u32 >= fed.saturating_sub(2),
            "lossless: {} units for {fed} frames",
            units.len()
        );
        assert!(units.iter().any(|(_, k)| *k), "a keyframe came out");

        let mut dec = openh264::decoder::Decoder::with_api_config(
            openh264::OpenH264API::from_source(),
            openh264::decoder::DecoderConfig::new(),
        )
        .expect("decoder");
        let mut decoded = 0u32;
        let mut last_dims = (0usize, 0usize);
        for (d, _) in &units {
            use openh264::formats::YUVSource as _;
            let pic = dec.decode(d).expect("clean decode — GPU lane bitstream");
            if let Some(yuv) = pic {
                decoded += 1;
                last_dims = yuv.dimensions();
                // Luma sanity: the stripes must decode as genuinely dark and
                // bright rows — a colour-space/matrix mistake flattens them.
                let y = yuv.y();
                let (mut lo, mut hi) = (255u8, 0u8);
                for &v in y.iter().take((w * 4) as usize) {
                    lo = lo.min(v);
                    hi = hi.max(v);
                }
                assert!(hi > 150, "bright rows survived conversion (hi {hi})");
            }
        }
        assert_eq!(last_dims, (w as usize, h as usize), "decoded dimensions");
        assert!(
            decoded >= fed.saturating_sub(3),
            "decoded {decoded} of {fed}"
        );
    }
}
