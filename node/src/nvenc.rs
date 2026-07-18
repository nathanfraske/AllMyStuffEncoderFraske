//! Direct NVENC — NVIDIA's Video Codec SDK driven straight, no Media
//! Foundation in between. This is the game-mode encoder rung: the MFT
//! wraps this same hardware but withholds the levers WAN gaming needs —
//! **intra-refresh/GDR** (spread the I-data across N frames instead of
//! sending an IDR wall, which shrinks the exact packet bursts the
//! transport can't yet pace), **reference-picture invalidation** (recover
//! from a reported loss with one P-frame instead of an IDR), and
//! guaranteed in-place reconfigure. The lane stays texture-native: input
//! is the same NV12 ring texture the GPU lane's VideoProcessor produces,
//! registered with the session and read in place.
//!
//! No build-time dependency: the API is a C function table loaded from
//! `nvEncodeAPI64.dll` at runtime (`NvEncodeAPICreateInstance`), so every
//! failure — no NVIDIA driver, old driver, no free session — is soft and
//! the ladder keeps the MF rung. The FFI subset below is hand-transcribed
//! from NVIDIA's MIT-licensed `nvEncodeAPI.h` (ffnvcodec n12.0.16.0 —
//! SDK 12.0, driver 522.25+); struct layouts are pinned by compile-time
//! size asserts. The two codec unions are deliberately **oversized** (and
//! zeroed): our pre-union field offsets match the header exactly, the
//! driver's writes land inside our larger allocation, and everything past
//! what we set reads back as zero on both sides — safe in both directions
//! without transcribing every codec's config.
//!
//! Sync mode only (`enableEncodeAsync = 0`, one frame in flight): encode
//! → lock (blocks until done) → copy out. At our sizes the hardware runs
//! a frame in single-digit milliseconds and the GPU lane's encode thread
//! exists to wait on exactly this; async event plumbing can come later if
//! a measurement asks for it.

#![cfg(windows)]

use std::ffi::c_void;

use windows::core::{Interface, GUID, PCSTR};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};

use crate::video::EncodeOutcome;

// ---------------------------------------------------------------------------
// FFI: the nvEncodeAPI.h subset (SDK 12.0 layouts)
// ---------------------------------------------------------------------------

/// `NVENCAPI_VERSION` for SDK 12.0: major 12, minor 0.
const API_VERSION: u32 = 12;
/// `NVENCAPI_STRUCT_VERSION(n)`.
const fn sv(n: u32) -> u32 {
    API_VERSION | (n << 16) | (0x7 << 28)
}

const NV_ENC_SUCCESS: u32 = 0;
const NV_ENC_ERR_NEED_MORE_INPUT: u32 = 17;

const NV_ENC_DEVICE_TYPE_DIRECTX: u32 = 0;
const NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX: u32 = 0;
const NV_ENC_BUFFER_FORMAT_NV12: u32 = 0x1;
const NV_ENC_BUFFER_USAGE_INPUT_IMAGE: u32 = 0;
const NV_ENC_PIC_STRUCT_FRAME: u32 = 0x01;
const NV_ENC_PIC_TYPE_I: u32 = 0x02;
const NV_ENC_PIC_TYPE_IDR: u32 = 0x03;
const NV_ENC_PIC_FLAG_FORCEIDR: u32 = 0x2;
const NV_ENC_PIC_FLAG_OUTPUT_SPSPPS: u32 = 0x4;
const NV_ENC_PARAMS_RC_VBR: u32 = 0x1;
const NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY: u32 = 3;
const NVENC_INFINITE_GOPLENGTH: u32 = 0xffff_ffff;

const NV_ENC_CODEC_H264_GUID: GUID = GUID::from_values(
    0x6bc8_2762,
    0x4e63,
    0x4ca4,
    [0xaa, 0x85, 0x1e, 0x50, 0xf3, 0x21, 0xf6, 0xbf],
);
const NV_ENC_PRESET_P4_GUID: GUID = GUID::from_values(
    0x90a7_b826,
    0xdf06,
    0x4862,
    [0xb9, 0xd2, 0xcd, 0x6d, 0x73, 0xa0, 0x86, 0x81],
);
const NV_ENC_PRESET_P5_GUID: GUID = GUID::from_values(
    0x21c6_e6b4,
    0x297a,
    0x4cba,
    [0x99, 0x8f, 0xb6, 0xcb, 0xde, 0x72, 0xad, 0xe3],
);
const NV_ENC_TUNING_INFO_HIGH_QUALITY: u32 = 1;
const NV_ENC_H264_PROFILE_MAIN_GUID: GUID = GUID::from_values(
    0x60b5_c1d4,
    0x67fe,
    0x4790,
    [0x94, 0xd5, 0xc4, 0x72, 0x6d, 0x7b, 0x6e, 0x6d],
);

#[allow(dead_code)]
#[repr(C)]
struct SessionExParams {
    version: u32,
    device_type: u32,
    device: *mut c_void,
    reserved: *mut c_void,
    api_version: u32,
    reserved1: [u32; 253],
    reserved2: [*mut c_void; 64],
}
const _: () = assert!(std::mem::size_of::<SessionExParams>() == 1552);

/// `NV_ENC_QP`.
#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy)]
struct NvEncQp {
    qp_inter_p: u32,
    qp_inter_b: u32,
    qp_intra: u32,
}

/// `NV_ENC_RC_PARAMS` — exact.
#[allow(dead_code)]
#[repr(C)]
struct RcParams {
    version: u32,
    rate_control_mode: u32,
    const_qp: NvEncQp,
    average_bit_rate: u32,
    max_bit_rate: u32,
    vbv_buffer_size: u32,
    vbv_initial_delay: u32,
    flags: u32,
    min_qp: NvEncQp,
    max_qp: NvEncQp,
    initial_rc_qp: NvEncQp,
    temporal_layer_idx_mask: u32,
    temporal_layer_qp: [u8; 8],
    target_quality: u8,
    target_quality_lsb: u8,
    lookahead_depth: u16,
    low_delay_key_frame_scale: u8,
    y_dc_qp_index_offset: i8,
    u_dc_qp_index_offset: i8,
    v_dc_qp_index_offset: i8,
    qp_map_mode: u32,
    multi_pass: u32,
    alpha_layer_bitrate_ratio: u32,
    cb_qp_index_offset: i8,
    cr_qp_index_offset: i8,
    reserved2: u16,
    reserved: [u32; 4],
}
const _: () = assert!(std::mem::size_of::<RcParams>() == 128);

/// `NV_ENC_CONFIG_H264_VUI_PARAMETERS` — exact (all-u32 fields).
#[allow(dead_code)]
#[repr(C)]
struct H264VuiParams {
    fields: [u32; 16],
    reserved: [u32; 12],
}
const _: () = assert!(std::mem::size_of::<H264VuiParams>() == 112);

/// `NV_ENC_CONFIG_H264` — exact.
#[allow(dead_code)]
#[repr(C)]
struct ConfigH264 {
    /// The 22 feature bits + 10 reserved, one packed u32 (see
    /// [`h264_flags`] for the bit positions this module sets).
    flags: u32,
    level: u32,
    idr_period: u32,
    separate_colour_plane_flag: u32,
    disable_deblocking_filter_idc: u32,
    num_temporal_layers: u32,
    sps_id: u32,
    pps_id: u32,
    adaptive_transform_mode: u32,
    fmo_mode: u32,
    bdirect_mode: u32,
    entropy_coding_mode: u32,
    stereo_mode: u32,
    intra_refresh_period: u32,
    intra_refresh_cnt: u32,
    max_num_ref_frames: u32,
    slice_mode: u32,
    slice_mode_data: u32,
    h264_vui_parameters: H264VuiParams,
    ltr_num_frames: u32,
    ltr_trust_mode: u32,
    chroma_format_idc: u32,
    max_temporal_layers: u32,
    use_bframes_as_ref: u32,
    num_ref_l0: u32,
    num_ref_l1: u32,
    reserved1: [u32; 267],
    reserved2: [*mut c_void; 64],
}
const _: () = assert!(std::mem::size_of::<ConfigH264>() == 1792);

/// Bit positions inside [`ConfigH264::flags`] (header order).
mod h264_flags {
    pub const OUTPUT_RECOVERY_POINT_SEI: u32 = 1 << 9;
    pub const ENABLE_INTRA_REFRESH: u32 = 1 << 10;
    pub const REPEAT_SPSPPS: u32 = 1 << 12;
}

/// `NV_ENC_CODEC_CONFIG` — deliberately oversized (see module docs). The
/// true union is `max(sizeof members)` (H.264's 1792 is the largest we
/// transcribed); 2048 gives headroom over every SDK 12.0 member.
#[allow(dead_code)]
#[repr(C)]
union CodecConfig {
    h264: std::mem::ManuallyDrop<ConfigH264>,
    raw: [u64; 256],
}
const _: () = assert!(std::mem::size_of::<CodecConfig>() == 2048);

/// `NV_ENC_CONFIG` — pre-union layout exact; union oversized.
#[allow(dead_code)]
#[repr(C)]
struct NvEncConfig {
    version: u32,
    profile_guid: GUID,
    gop_length: u32,
    frame_interval_p: i32,
    mono_chrome_encoding: u32,
    frame_field_mode: u32,
    mv_precision: u32,
    rc_params: RcParams,
    encode_codec_config: CodecConfig,
    reserved: [u32; 278],
    reserved2: [*mut c_void; 64],
}
const _: () = assert!(std::mem::size_of::<NvEncConfig>() == 3840);

/// `NV_ENC_PRESET_CONFIG`.
#[allow(dead_code)]
#[repr(C)]
struct PresetConfig {
    version: u32,
    preset_cfg: NvEncConfig,
    reserved1: [u32; 255],
    reserved2: [*mut c_void; 64],
}

/// `NVENC_EXTERNAL_ME_HINT_COUNTS_PER_BLOCKTYPE`.
#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy)]
struct MeHintCounts {
    bits: u32,
    reserved1: [u32; 3],
}

/// `NV_ENC_INITIALIZE_PARAMS` — exact.
#[allow(dead_code)]
#[repr(C)]
struct InitializeParams {
    version: u32,
    encode_guid: GUID,
    preset_guid: GUID,
    encode_width: u32,
    encode_height: u32,
    dar_width: u32,
    dar_height: u32,
    frame_rate_num: u32,
    frame_rate_den: u32,
    enable_encode_async: u32,
    enable_ptd: u32,
    flags: u32,
    priv_data_size: u32,
    priv_data: *mut c_void,
    encode_config: *mut NvEncConfig,
    max_encode_width: u32,
    max_encode_height: u32,
    max_me_hint_counts_per_block: [MeHintCounts; 2],
    tuning_info: u32,
    buffer_format: u32,
    reserved: [u32; 287],
    reserved2: [*mut c_void; 64],
}
const _: () = assert!(std::mem::size_of::<InitializeParams>() == 1808);

/// `NV_ENC_RECONFIGURE_PARAMS`.
#[allow(dead_code)]
#[repr(C)]
struct ReconfigureParams {
    version: u32,
    re_init_encode_params: InitializeParams,
    flags: u32,
}
const _: () = assert!(std::mem::size_of::<ReconfigureParams>() == 1824);

/// `NV_ENC_CREATE_BITSTREAM_BUFFER`.
#[allow(dead_code)]
#[repr(C)]
struct CreateBitstreamBuffer {
    version: u32,
    size: u32,
    memory_heap: u32,
    reserved: u32,
    bitstream_buffer: *mut c_void,
    bitstream_buffer_ptr: *mut c_void,
    reserved1: [u32; 58],
    reserved2: [*mut c_void; 64],
}
const _: () = assert!(std::mem::size_of::<CreateBitstreamBuffer>() == 776);

/// `NV_ENC_REGISTER_RESOURCE`.
#[allow(dead_code)]
#[repr(C)]
struct RegisterResource {
    version: u32,
    resource_type: u32,
    width: u32,
    height: u32,
    pitch: u32,
    sub_resource_index: u32,
    resource_to_register: *mut c_void,
    registered_resource: *mut c_void,
    buffer_format: u32,
    buffer_usage: u32,
    p_input_fence_point: *mut c_void,
    reserved1: [u32; 247],
    reserved2: [*mut c_void; 61],
}
const _: () = assert!(std::mem::size_of::<RegisterResource>() == 1536);

/// `NV_ENC_MAP_INPUT_RESOURCE`.
#[allow(dead_code)]
#[repr(C)]
struct MapInputResource {
    version: u32,
    sub_resource_index: u32,
    input_resource: *mut c_void,
    registered_resource: *mut c_void,
    mapped_resource: *mut c_void,
    mapped_buffer_fmt: u32,
    reserved1: [u32; 251],
    reserved2: [*mut c_void; 63],
}
const _: () = assert!(std::mem::size_of::<MapInputResource>() == 1544);

/// `NV_ENC_PIC_PARAMS` — pre-union layout exact; the codec union and
/// everything after it are one oversized zeroed tail (the fields there —
/// ME hints, QP maps, alpha — are all optional and null/zero for us; the
/// driver reads zeros at its own offsets inside our larger buffer).
#[allow(dead_code)]
#[repr(C)]
struct PicParams {
    version: u32,
    input_width: u32,
    input_height: u32,
    input_pitch: u32,
    encode_pic_flags: u32,
    frame_idx: u32,
    input_time_stamp: u64,
    input_duration: u64,
    input_buffer: *mut c_void,
    output_bitstream: *mut c_void,
    completion_event: *mut c_void,
    buffer_fmt: u32,
    picture_struct: u32,
    picture_type: u32,
    _pad: u32,
    /// H.264 per-picture params live at the head of the true union; the
    /// one field this module ever sets is `forceIntraRefreshWithFrameCnt`
    /// at offset 16 within it.
    codec_pic_params: [u64; 256],
    tail: [u64; 256],
}
const _: () = assert!(std::mem::size_of::<PicParams>() == 80 + 2048 + 2048);

/// Offset (in u32 units) of `forceIntraRefreshWithFrameCnt` inside
/// `NV_ENC_PIC_PARAMS_H264` (fifth field, all u32s before it).
const H264_PIC_FORCE_INTRA_REFRESH_IDX: usize = 4;

/// `NV_ENC_LOCK_BITSTREAM` — exact.
#[allow(dead_code)]
#[repr(C)]
struct LockBitstream {
    version: u32,
    flags: u32,
    output_bitstream: *mut c_void,
    slice_offsets: *mut u32,
    frame_idx: u32,
    hw_encode_status: u32,
    num_slices: u32,
    bitstream_size_in_bytes: u32,
    output_time_stamp: u64,
    output_duration: u64,
    bitstream_buffer_ptr: *mut c_void,
    picture_type: u32,
    picture_struct: u32,
    frame_avg_qp: u32,
    frame_satd: u32,
    ltr_frame_idx: u32,
    ltr_frame_bitmap: u32,
    temporal_id: u32,
    reserved: [u32; 12],
    intra_mb_count: u32,
    inter_mb_count: u32,
    average_mvx: i32,
    average_mvy: i32,
    alpha_layer_size_in_bytes: u32,
    reserved1: [u32; 218],
    reserved2: [*mut c_void; 64],
}
const _: () = assert!(std::mem::size_of::<LockBitstream>() == 1544);

type Enc = *mut c_void;
type StatusFn0 = unsafe extern "system" fn(Enc) -> u32;
type StatusFn1<T> = unsafe extern "system" fn(Enc, *mut T) -> u32;
type StatusFnPtr = unsafe extern "system" fn(Enc, *mut c_void) -> u32;

/// `NV_ENCODE_API_FUNCTION_LIST` — every slot transcribed in header
/// order (each is one pointer, so unused ones are typed opaque).
#[allow(dead_code)]
#[repr(C)]
struct FunctionList {
    version: u32,
    reserved: u32,
    open_encode_session: *mut c_void,
    get_encode_guid_count: *mut c_void,
    get_encode_profile_guid_count: *mut c_void,
    get_encode_profile_guids: *mut c_void,
    get_encode_guids: *mut c_void,
    get_input_format_count: *mut c_void,
    get_input_formats: *mut c_void,
    get_encode_caps: *mut c_void,
    get_encode_preset_count: *mut c_void,
    get_encode_preset_guids: *mut c_void,
    get_encode_preset_config: *mut c_void,
    initialize_encoder: Option<StatusFn1<InitializeParams>>,
    create_input_buffer: *mut c_void,
    destroy_input_buffer: *mut c_void,
    create_bitstream_buffer: Option<StatusFn1<CreateBitstreamBuffer>>,
    destroy_bitstream_buffer: Option<StatusFnPtr>,
    encode_picture: Option<StatusFn1<PicParams>>,
    lock_bitstream: Option<StatusFn1<LockBitstream>>,
    unlock_bitstream: Option<StatusFnPtr>,
    lock_input_buffer: *mut c_void,
    unlock_input_buffer: *mut c_void,
    get_encode_stats: *mut c_void,
    get_sequence_params: *mut c_void,
    register_async_event: *mut c_void,
    unregister_async_event: *mut c_void,
    map_input_resource: Option<StatusFn1<MapInputResource>>,
    unmap_input_resource: Option<StatusFnPtr>,
    destroy_encoder: Option<StatusFn0>,
    invalidate_ref_frames: Option<unsafe extern "system" fn(Enc, u64) -> u32>,
    open_encode_session_ex:
        Option<unsafe extern "system" fn(*mut SessionExParams, *mut Enc) -> u32>,
    register_resource: Option<StatusFn1<RegisterResource>>,
    unregister_resource: Option<StatusFnPtr>,
    reconfigure_encoder: Option<StatusFn1<ReconfigureParams>>,
    reserved1: *mut c_void,
    create_mv_buffer: *mut c_void,
    destroy_mv_buffer: *mut c_void,
    run_motion_estimation_only: *mut c_void,
    get_last_error_string: Option<unsafe extern "system" fn(Enc) -> *const i8>,
    set_io_cuda_streams: *mut c_void,
    get_encode_preset_config_ex:
        Option<unsafe extern "system" fn(Enc, GUID, GUID, u32, *mut PresetConfig) -> u32>,
    get_sequence_param_ex: *mut c_void,
    reserved2: [*mut c_void; 277],
}
const _: () = assert!(std::mem::size_of::<FunctionList>() == 2552);

// ---------------------------------------------------------------------------
// Loader
// ---------------------------------------------------------------------------

/// The loaded function table. Raw pointers make it `!Sync` by default;
/// after init it is immutable and every call site owns its own encoder
/// handle (NVENC requires per-session external sync, which the one-thread
/// ownership of [`NvencH264`] provides), so sharing the table is sound.
struct ApiTable(FunctionList);
unsafe impl Send for ApiTable {}
unsafe impl Sync for ApiTable {}

/// Load `nvEncodeAPI64.dll` and populate the function table once per
/// process. `Err` = no NVIDIA driver (or one too old for API 12.0) — the
/// ladder keeps the MF rung.
fn api() -> Result<&'static FunctionList, String> {
    static API: std::sync::OnceLock<Result<&'static ApiTable, String>> = std::sync::OnceLock::new();
    API.get_or_init(|| unsafe {
        let module = LoadLibraryA(PCSTR(c"nvEncodeAPI64.dll".as_ptr() as *const u8))
            .map_err(|e| format!("nvEncodeAPI64.dll not loadable: {e}"))?;
        let max_ver = GetProcAddress(
            module,
            PCSTR(c"NvEncodeAPIGetMaxSupportedVersion".as_ptr() as *const u8),
        )
        .ok_or("NvEncodeAPIGetMaxSupportedVersion missing")?;
        let max_ver: unsafe extern "system" fn(*mut u32) -> u32 = std::mem::transmute(max_ver);
        let mut driver_ver = 0u32;
        if max_ver(&mut driver_ver) != NV_ENC_SUCCESS {
            return Err("NvEncodeAPIGetMaxSupportedVersion failed".into());
        }
        // Version word is (major | minor<<24) on both sides; compare as
        // (major, minor).
        let (drv_major, drv_minor) = (driver_ver & 0xffffff, driver_ver >> 24);
        if (drv_major, drv_minor) < (12, 0) {
            return Err(format!(
                "driver NVENC API {drv_major}.{drv_minor} < 12.0 (driver too old)"
            ));
        }
        tracing::info!(
            "NVENC driver API {drv_major}.{drv_minor} available (this build targets 12.0)"
        );
        let create = GetProcAddress(
            module,
            PCSTR(c"NvEncodeAPICreateInstance".as_ptr() as *const u8),
        )
        .ok_or("NvEncodeAPICreateInstance missing")?;
        let create: unsafe extern "system" fn(*mut FunctionList) -> u32 =
            std::mem::transmute(create);
        let mut list: Box<ApiTable> = Box::new(std::mem::zeroed());
        list.0.version = sv(2);
        let status = create(&mut list.0);
        if status != NV_ENC_SUCCESS {
            return Err(format!("NvEncodeAPICreateInstance: status {status}"));
        }
        Ok(&*Box::leak(list))
    })
    .as_ref()
    .map(|t| &t.0)
    .map_err(Clone::clone)
}

// ---------------------------------------------------------------------------
// The encoder
// ---------------------------------------------------------------------------

/// One registered input texture (the GPU lane's NV12 ring slots register
/// lazily, keyed by interface pointer).
struct Registered {
    tex_ptr: *mut c_void,
    handle: *mut c_void,
}

/// A direct NVENC H.264 session, fed D3D11 NV12 textures.
pub struct NvencH264 {
    api: &'static FunctionList,
    encoder: Enc,
    bitstream: *mut c_void,
    registered: Vec<Registered>,
    /// The init/config blocks, kept for in-place reconfigure.
    config: Box<NvEncConfig>,
    init: Box<InitializeParams>,
    width: u32,
    height: u32,
    fps: u32,
    frame_index: u64,
    intra_refresh: bool,
    studio: bool,
    label: String,
}

// SAFETY: built on, and only ever driven from, the route's encode thread
// (same contract as MediaFoundationH264 — the Send bound exists for the
// codec seam, the value never actually migrates mid-stream).
unsafe impl Send for NvencH264 {}

impl NvencH264 {
    /// Open a session on `device` (the GPU lane's D3D11 device — input
    /// textures must live on it). `game` = GDR posture (infinite GOP,
    /// refresh waves instead of IDR walls, single-frame VBV); `studio` =
    /// the LAN fidelity posture (quality-tuned preset, deep VBV — 4:4:4
    /// and lossless slot in here once the hardware-decode viewer lands).
    /// The two are exclusive; `studio` wins if both arrive.
    pub fn open_on_device(
        device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
        game: bool,
        studio: bool,
    ) -> Result<Self, String> {
        let intra_refresh = game && !studio;
        let api = api()?;
        unsafe {
            let open = api.open_encode_session_ex.ok_or("no OpenEncodeSessionEx")?;
            let mut session = SessionExParams {
                version: sv(1),
                device_type: NV_ENC_DEVICE_TYPE_DIRECTX,
                device: device.as_raw(),
                reserved: std::ptr::null_mut(),
                api_version: API_VERSION,
                reserved1: [0; 253],
                reserved2: [std::ptr::null_mut(); 64],
            };
            let mut encoder: Enc = std::ptr::null_mut();
            let status = open(&mut session, &mut encoder);
            if status != NV_ENC_SUCCESS || encoder.is_null() {
                return Err(format!("NvEncOpenEncodeSessionEx: status {status}"));
            }
            // From here every failure must destroy the session or it
            // leaks a hardware slot.
            let mut me = NvencH264 {
                api,
                encoder,
                bitstream: std::ptr::null_mut(),
                registered: Vec::new(),
                config: Box::new(std::mem::zeroed()),
                init: Box::new(std::mem::zeroed()),
                width,
                height,
                fps: fps.max(1),
                frame_index: 0,
                intra_refresh,
                studio,
                label: if studio {
                    "NVENC SDK (H.264, studio)".to_string()
                } else if intra_refresh {
                    "NVENC SDK (H.264, intra-refresh)".to_string()
                } else {
                    "NVENC SDK (H.264)".to_string()
                },
            };
            // An init failure drops `me`, which destroys the session —
            // no leaked hardware slot.
            me.initialize(bitrate)?;
            Ok(me)
        }
    }

    unsafe fn initialize(&mut self, bitrate: u32) -> Result<(), String> {
        // Start from the driver's own preset config (P4, ultra-low-latency
        // tuning — no lookahead, no B-frames, single-frame delay), then
        // re-aim the pieces our stream model owns.
        let preset_ex = self
            .api
            .get_encode_preset_config_ex
            .ok_or("no GetEncodePresetConfigEx")?;
        // Balanced/game run P4 + ultra-low-latency tuning; studio runs
        // P5 + high-quality tuning (a slower, better encode the LAN
        // fidelity budget is meant to feed — still no B-frames, forced
        // below, so latency stays interactive).
        let (preset_guid, tuning) = if self.studio {
            (NV_ENC_PRESET_P5_GUID, NV_ENC_TUNING_INFO_HIGH_QUALITY)
        } else {
            (NV_ENC_PRESET_P4_GUID, NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY)
        };
        let mut preset: Box<PresetConfig> = Box::new(std::mem::zeroed());
        preset.version = sv(4) | (1 << 31);
        preset.preset_cfg.version = sv(8) | (1 << 31);
        let status = preset_ex(
            self.encoder,
            NV_ENC_CODEC_H264_GUID,
            preset_guid,
            tuning,
            &mut *preset,
        );
        if status != NV_ENC_SUCCESS {
            return Err(self.err("GetEncodePresetConfigEx", status));
        }
        // Copy the filled config out (plain data — the box just drops)
        // and shape it.
        std::ptr::copy_nonoverlapping(&preset.preset_cfg, &mut *self.config, 1);
        drop(preset);
        let cfg = &mut *self.config;
        cfg.version = sv(8) | (1 << 31);
        cfg.profile_guid = NV_ENC_H264_PROFILE_MAIN_GUID;
        cfg.frame_interval_p = 1; // IPP…, never B (latency + LTR/invalidatable)
        cfg.rc_params.rate_control_mode = NV_ENC_PARAMS_RC_VBR;
        cfg.rc_params.average_bit_rate = bitrate;
        let (peak, vbv) = crate::video::burst_bounds(bitrate, self.intra_refresh);
        cfg.rc_params.max_bit_rate = peak;
        cfg.rc_params.vbv_buffer_size = vbv;
        cfg.rc_params.vbv_initial_delay = 0;
        if self.studio {
            // Studio: quality-first on a LAN that can carry it — a full
            // second of VBV lets rate control spend where the picture
            // needs it, and the peak stays close to the (already high)
            // mean so the pacer's bursts stay predictable. 4:4:4 chroma
            // and the lossless preset slot in here once the viewer
            // decodes them (the hardware-decode epic).
            cfg.rc_params.vbv_buffer_size = bitrate;
            cfg.rc_params.max_bit_rate = bitrate + bitrate / 5;
        }
        if self.intra_refresh {
            // Game posture: a single-frame VBV — every frame fits one
            // frame interval's bit budget, so no frame can queue behind
            // an oversized predecessor. GDR removes the IDR spikes that
            // made a deep bucket necessary; what remains benefits from
            // constant-latency framing far more than from burst headroom.
            cfg.rc_params.vbv_buffer_size = (bitrate / self.fps).max(50_000);
        }
        let h264 = &mut cfg.encode_codec_config.h264;
        h264.flags |= h264_flags::REPEAT_SPSPPS;
        h264.entropy_coding_mode = 0; // autoselect (CABAC where allowed)
        if crate::video::paced_slices_enabled() {
            // Slice-count mode (sliceMode 3): the send-side pacer's cut
            // points — a keyframe leaves as several independently-
            // decodable slices instead of one wall. Count, not bytes:
            // byte-based slicing (mode 1) is rejected outright by real
            // drivers in the field ("Byte based slice encoding is not
            // supported"), while count mode is universal. 8 slices at
            // ≥1080p ≈ the ~24 KB pacing grain on a worst-case keyframe.
            h264.slice_mode = 3;
            h264.slice_mode_data = if self.width * self.height >= 1920 * 1080 {
                8
            } else {
                4
            };
        }
        if self.intra_refresh {
            // GDR: no automatic IDRs at all; intra data rides a refresh
            // wave every ~0.5 s (the field pass at 2 s left loss artifacts
            // lingering visibly in-game — half a second bounds the heal to
            // a blink while still spreading the intra cost). Recovery-point
            // SEI marks each wave so a decoder knows where clean starts. A
            // forced IDR (viewer join, quiesce rescue) still cuts through
            // via the per-picture flag.
            cfg.gop_length = NVENC_INFINITE_GOPLENGTH;
            h264.idr_period = NVENC_INFINITE_GOPLENGTH;
            h264.flags |= h264_flags::ENABLE_INTRA_REFRESH | h264_flags::OUTPUT_RECOVERY_POINT_SEI;
            let period = (self.fps / 2).max(15);
            h264.intra_refresh_period = period;
            h264.intra_refresh_cnt = (period / 5).max(3);
        } else {
            // Stable-arc shape: same ~4 s GOP backstop as the MF rung; the
            // stream's adaptive IDR cadence forces the real keyframes.
            cfg.gop_length = self.fps.saturating_mul(4).max(1);
            h264.idr_period = cfg.gop_length;
        }

        let init = &mut *self.init;
        init.version = sv(5) | (1 << 31);
        init.encode_guid = NV_ENC_CODEC_H264_GUID;
        init.preset_guid = preset_guid;
        init.encode_width = self.width;
        init.encode_height = self.height;
        init.dar_width = self.width;
        init.dar_height = self.height;
        init.frame_rate_num = self.fps;
        init.frame_rate_den = 1;
        init.enable_encode_async = 0;
        init.enable_ptd = 1;
        init.tuning_info = tuning;
        init.encode_config = &mut *self.config;
        let init_fn = self.api.initialize_encoder.ok_or("no InitializeEncoder")?;
        let mut status = init_fn(self.encoder, &mut *self.init);
        if status != NV_ENC_SUCCESS {
            let h264 = &mut self.config.encode_codec_config.h264;
            if h264.slice_mode != 0 {
                // A driver that rejects our slice config must cost the
                // pacer its cut points, never the whole SDK rung: retry
                // once with default (single-slice) framing.
                tracing::info!(
                    "NVENC rejected slice config (status {status}); retrying single-slice"
                );
                h264.slice_mode = 0;
                h264.slice_mode_data = 0;
                status = init_fn(self.encoder, &mut *self.init);
            }
        }
        if status != NV_ENC_SUCCESS {
            return Err(self.err("InitializeEncoder", status));
        }

        let mut create = CreateBitstreamBuffer {
            version: sv(1),
            size: 0,
            memory_heap: 0,
            reserved: 0,
            bitstream_buffer: std::ptr::null_mut(),
            bitstream_buffer_ptr: std::ptr::null_mut(),
            reserved1: [0; 58],
            reserved2: [std::ptr::null_mut(); 64],
        };
        let status = (self
            .api
            .create_bitstream_buffer
            .ok_or("no CreateBitstreamBuffer")?)(self.encoder, &mut create);
        if status != NV_ENC_SUCCESS {
            return Err(self.err("CreateBitstreamBuffer", status));
        }
        self.bitstream = create.bitstream_buffer;
        Ok(())
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    /// Encode one NV12 texture (on the session's device) synchronously,
    /// returning the produced access unit. `force_idr` = clean entry now
    /// (works in both GOP shapes). Speaks the ladder's
    /// [`EncodeOutcome`] seam (`pub(crate)`: the outcome type is the
    /// video module's internal seam).
    pub(crate) fn encode_texture(
        &mut self,
        nv12: &ID3D11Texture2D,
        force_idr: bool,
    ) -> Result<EncodeOutcome, String> {
        unsafe {
            let handle = self.register(nv12)?;
            let mut map = MapInputResource {
                version: sv(4),
                sub_resource_index: 0,
                input_resource: std::ptr::null_mut(),
                registered_resource: handle,
                mapped_resource: std::ptr::null_mut(),
                mapped_buffer_fmt: 0,
                reserved1: [0; 251],
                reserved2: [std::ptr::null_mut(); 63],
            };
            let status =
                (self.api.map_input_resource.ok_or("no MapInputResource")?)(self.encoder, &mut map);
            if status != NV_ENC_SUCCESS {
                return Err(self.err("MapInputResource", status));
            }

            let duration = 10_000_000 / u64::from(self.fps);
            let mut pic: Box<PicParams> = Box::new(std::mem::zeroed());
            pic.version = sv(6) | (1 << 31);
            pic.input_width = self.width;
            pic.input_height = self.height;
            pic.input_pitch = self.width;
            pic.input_buffer = map.mapped_resource;
            pic.output_bitstream = self.bitstream;
            pic.buffer_fmt = NV_ENC_BUFFER_FORMAT_NV12;
            pic.picture_struct = NV_ENC_PIC_STRUCT_FRAME;
            pic.input_time_stamp = self.frame_index * duration;
            pic.input_duration = duration;
            self.frame_index += 1;
            if force_idr {
                pic.encode_pic_flags = NV_ENC_PIC_FLAG_FORCEIDR | NV_ENC_PIC_FLAG_OUTPUT_SPSPPS;
                if self.intra_refresh {
                    // An IDR mid-GDR: restart the refresh wave after it so
                    // the stream returns to walls-free steady state.
                    let words = std::slice::from_raw_parts_mut(
                        pic.codec_pic_params.as_mut_ptr() as *mut u32,
                        8,
                    );
                    words[H264_PIC_FORCE_INTRA_REFRESH_IDX] = 0;
                }
            }

            let status =
                (self.api.encode_picture.ok_or("no EncodePicture")?)(self.encoder, &mut *pic);
            let outcome = if status == NV_ENC_SUCCESS {
                let mut lock: Box<LockBitstream> = Box::new(std::mem::zeroed());
                lock.version = sv(2);
                lock.output_bitstream = self.bitstream;
                let status =
                    (self.api.lock_bitstream.ok_or("no LockBitstream")?)(self.encoder, &mut *lock);
                if status != NV_ENC_SUCCESS {
                    let _ = self
                        .api
                        .unmap_input_resource
                        .map(|f| f(self.encoder, map.mapped_resource));
                    return Err(self.err("LockBitstream", status));
                }
                let data = std::slice::from_raw_parts(
                    lock.bitstream_buffer_ptr as *const u8,
                    lock.bitstream_size_in_bytes as usize,
                )
                .to_vec();
                let key = lock.picture_type == NV_ENC_PIC_TYPE_IDR
                    || lock.picture_type == NV_ENC_PIC_TYPE_I;
                if let Some(unlock) = self.api.unlock_bitstream {
                    let status = unlock(self.encoder, self.bitstream);
                    if status != NV_ENC_SUCCESS {
                        tracing::debug!("NVENC UnlockBitstream: status {status}");
                    }
                }
                EncodeOutcome::consumed(Some((data, key)))
            } else if status == NV_ENC_ERR_NEED_MORE_INPUT {
                // Can't happen with PTD + no B-frames + sync mode, but the
                // seam has a lossless answer for it, so use it.
                EncodeOutcome::consumed(None)
            } else {
                let _ = self
                    .api
                    .unmap_input_resource
                    .map(|f| f(self.encoder, map.mapped_resource));
                return Err(self.err("EncodePicture", status));
            };
            if let Some(unmap) = self.api.unmap_input_resource {
                let status = unmap(self.encoder, map.mapped_resource);
                if status != NV_ENC_SUCCESS {
                    tracing::debug!("NVENC UnmapInputResource: status {status}");
                }
            }
            Ok(outcome)
        }
    }

    /// Re-aim the rate controller in place — the SDK's guaranteed form of
    /// what the MF rung only partially honors: mean, peak, and VBV all
    /// move, no reset, no IDR.
    pub fn set_bitrate(&mut self, bitrate: u32) -> bool {
        unsafe {
            let Some(reconfigure) = self.api.reconfigure_encoder else {
                return false;
            };
            self.config.rc_params.average_bit_rate = bitrate;
            let (peak, vbv) = crate::video::burst_bounds(bitrate, self.intra_refresh);
            self.config.rc_params.max_bit_rate = peak;
            self.config.rc_params.vbv_buffer_size = vbv;
            let mut params: Box<ReconfigureParams> = Box::new(std::mem::zeroed());
            params.version = sv(1) | (1 << 31);
            std::ptr::copy_nonoverlapping(&*self.init, &mut params.re_init_encode_params, 1);
            params.re_init_encode_params.encode_config = &mut *self.config;
            let status = reconfigure(self.encoder, &mut *params);
            if status != NV_ENC_SUCCESS {
                tracing::debug!("NVENC ReconfigureEncoder: status {status}");
                return false;
            }
            true
        }
    }

    /// Register (once) the texture with the session, keyed by interface
    /// pointer — the GPU lane cycles a small fixed ring, so this settles
    /// after the first lap.
    unsafe fn register(&mut self, tex: &ID3D11Texture2D) -> Result<*mut c_void, String> {
        let ptr = tex.as_raw();
        if let Some(r) = self.registered.iter().find(|r| r.tex_ptr == ptr) {
            return Ok(r.handle);
        }
        let mut reg = RegisterResource {
            version: sv(4),
            resource_type: NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX,
            width: self.width,
            height: self.height,
            pitch: 0,
            sub_resource_index: 0,
            resource_to_register: ptr,
            registered_resource: std::ptr::null_mut(),
            buffer_format: NV_ENC_BUFFER_FORMAT_NV12,
            buffer_usage: NV_ENC_BUFFER_USAGE_INPUT_IMAGE,
            p_input_fence_point: std::ptr::null_mut(),
            reserved1: [0; 247],
            reserved2: [std::ptr::null_mut(); 61],
        };
        let status =
            (self.api.register_resource.ok_or("no RegisterResource")?)(self.encoder, &mut reg);
        if status != NV_ENC_SUCCESS {
            return Err(self.err("RegisterResource", status));
        }
        self.registered.push(Registered {
            tex_ptr: ptr,
            handle: reg.registered_resource,
        });
        Ok(reg.registered_resource)
    }

    fn err(&self, what: &str, status: u32) -> String {
        let detail = unsafe {
            self.api
                .get_last_error_string
                .map(|f| {
                    let p = f(self.encoder);
                    if p.is_null() {
                        String::new()
                    } else {
                        std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
                    }
                })
                .unwrap_or_default()
        };
        format!("NVENC {what}: status {status} ({detail})")
    }
}

impl Drop for NvencH264 {
    fn drop(&mut self) {
        unsafe {
            for r in self.registered.drain(..) {
                if let Some(f) = self.api.unregister_resource {
                    let _ = f(self.encoder, r.handle);
                }
            }
            if !self.bitstream.is_null() {
                if let Some(f) = self.api.destroy_bitstream_buffer {
                    let _ = f(self.encoder, self.bitstream);
                }
            }
            if let Some(f) = self.api.destroy_encoder {
                let _ = f(self.encoder);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole SDK lane in isolation, mirroring `gpu_lane_end_to_end`:
    /// synthetic BGRA → VideoProcessor NV12 (the exact texture shape the
    /// live lane feeds) → **direct NVENC** → Annex-B → openh264 decode
    /// with dimension + luma asserts. Skips (passing) without an NVIDIA
    /// driver. Also proves in-place `set_bitrate` on the SDK path.
    #[test]
    fn nvenc_sdk_end_to_end() {
        let (w, h) = (640u32, 480u32);
        let mut gpu = match crate::gpu_pipeline::GpuConvert::new(w, h, w, h) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("SKIP: GPU convert unavailable: {e}");
                return;
            }
        };
        let mut enc =
            match NvencH264::open_on_device(&gpu.device(), w, h, 30, 4_000_000, false, false) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("SKIP: NVENC session unavailable: {e}");
                    return;
                }
            };
        let mut bgra = vec![0u8; (w * h * 4) as usize];
        let tex = gpu.bgra_texture_from(&bgra, w, h).expect("tex");
        let mut units: Vec<(Vec<u8>, bool)> = Vec::new();
        let mut fed = 0u32;
        for i in 0..60u32 {
            for row in 0..h as usize {
                let bright = (row as u32).is_multiple_of(2) == i.is_multiple_of(2);
                let v = if bright { 220u8 } else { 40u8 };
                for px in bgra[row * (w as usize) * 4..][..(w as usize) * 4].chunks_exact_mut(4) {
                    px[0] = v;
                    px[1] = v;
                    px[2] = v;
                    px[3] = 255;
                }
            }
            gpu.update_bgra(&tex, &bgra, w, h);
            let (slot, nv12) = gpu.convert(&tex).expect("convert").expect("slot");
            let out = enc.encode_texture(&nv12, i == 0).expect("encode");
            gpu.release(slot);
            if out.consumed {
                fed += 1;
            }
            if i == 30 {
                assert!(enc.set_bitrate(2_000_000), "SDK in-place reconfigure");
            }
            units.extend(out.units);
        }
        assert!(units.len() as u32 >= fed - 1, "sync mode: a unit per frame");
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
            if let Some(yuv) = dec.decode(d).expect("clean decode — SDK bitstream") {
                decoded += 1;
                last_dims = yuv.dimensions();
                let mut hi = 0u8;
                for &v in yuv.y().iter().take((w * 4) as usize) {
                    hi = hi.max(v);
                }
                assert!(hi > 150, "bright rows survived (hi {hi})");
            }
        }
        assert_eq!(last_dims, (w as usize, h as usize), "decoded dimensions");
        assert!(decoded >= fed - 3, "decoded {decoded} of {fed}");
    }

    /// Ignored-by-default bench: the SDK rung's per-frame cost at 1440p,
    /// with the exact parameters of `bench_gpu_lane_cycle` (30 Mbps, 150
    /// frames, full-frame gradient shift) so its encode column compares
    /// directly against the MF rung's. One semantic difference to read
    /// with the numbers: this rung is synchronous — the call time IS the
    /// frame's true encode-to-bits latency — while the MF rung's call
    /// returns while the hardware still works and its bits surface a call
    /// later (~a frame of hidden pipeline latency at the pump's cadence).
    /// Run: `cargo test --release -- --ignored bench_nvenc --nocapture --test-threads=1`
    #[test]
    #[ignore = "bench — run with --ignored --nocapture"]
    fn bench_nvenc_sdk_cycle() {
        // Interleaved posture matrix — the decomposition of what each
        // kernel costs and buys at the encoder: balanced vs game (VBV
        // shape), game at its uncorked 200 Mbps ceiling (the "extra bits
        // are latency-free under single-frame VBV" claim), and studio
        // (P5 + high-quality tuning at its 150 Mbps floor).
        const MATRIX: [(&str, bool, bool, u32); 4] = [
            ("balanced 30M", false, false, 30_000_000),
            ("game 30M", true, false, 30_000_000),
            ("game 200M", true, false, 200_000_000),
            ("studio 150M", false, true, 150_000_000),
        ];
        for round in [1u32, 2] {
            for (label, game, studio, bitrate) in MATRIX {
                bench_cycle_posture(round, label, game, studio, bitrate);
            }
        }
    }

    fn bench_cycle_posture(round: u32, label: &str, game: bool, studio: bool, bitrate: u32) {
        use std::time::{Duration, Instant};
        let (w, h) = (2560u32, 1440u32);
        let mut gpu = match crate::gpu_pipeline::GpuConvert::new(w, h, w, h) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("SKIP: {e}");
                return;
            }
        };
        let mut enc =
            match NvencH264::open_on_device(&gpu.device(), w, h, 60, bitrate, game, studio) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("SKIP: {e}");
                    return;
                }
            };
        let mut bgra = vec![0u8; (w * h * 4) as usize];
        let tex = gpu.bgra_texture_from(&bgra, w, h).expect("tex");
        let n = 150u32;
        let (mut t_up, mut t_conv, mut t_enc) = (Duration::ZERO, Duration::ZERO, Duration::ZERO);
        let mut enc_ms: Vec<f64> = Vec::with_capacity(n as usize);
        let mut units = 0usize;
        for i in 0..n {
            for (j, v) in bgra.iter_mut().enumerate() {
                *v = ((j as u32).wrapping_add(i.wrapping_mul(7)) % 255) as u8;
            }
            let t0 = Instant::now();
            gpu.update_bgra(&tex, &bgra, w, h);
            let t1 = Instant::now();
            let (slot, nv12) = gpu.convert(&tex).expect("convert").expect("slot");
            let t2 = Instant::now();
            let out = enc.encode_texture(&nv12, i == 0).expect("encode");
            let t3 = Instant::now();
            gpu.release(slot);
            units += out.units.len();
            t_up += t1 - t0;
            t_conv += t2 - t1;
            t_enc += t3 - t2;
            enc_ms.push((t3 - t2).as_secs_f64() * 1000.0);
        }
        enc_ms.sort_by(f64::total_cmp);
        let ms = |d: Duration| d.as_secs_f64() * 1000.0 / f64::from(n);
        let p95 = enc_ms[(enc_ms.len() * 95 / 100).min(enc_ms.len() - 1)];
        let max = enc_ms[enc_ms.len() - 1];
        println!(
            "bench NVENC SDK @1440p [round {round} · {label}] over {n} frames ({units} units):",
        );
        println!("  upload (synthetic, not paid live): {:6.3} ms", ms(t_up));
        println!("  convert (blt queue)              : {:6.3} ms", ms(t_conv));
        println!(
            "  encode_texture (sync, true latency): {:6.3} ms avg · p95 {p95:6.3} · max {max:6.3}",
            ms(t_enc)
        );
    }

    /// The GDR pilot: with intra-refresh on, a two-second stream contains
    /// **no IDR after the first frame** — the intra data rides refresh
    /// waves — yet decodes cleanly end to end. This is the burst-shrinking
    /// mode: no more keyframe walls for the transport to spray.
    #[test]
    fn nvenc_intra_refresh_replaces_idr_walls() {
        let (w, h) = (640u32, 480u32);
        let mut gpu = match crate::gpu_pipeline::GpuConvert::new(w, h, w, h) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("SKIP: GPU convert unavailable: {e}");
                return;
            }
        };
        let mut enc =
            match NvencH264::open_on_device(&gpu.device(), w, h, 30, 3_000_000, true, false) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("SKIP: NVENC session unavailable: {e}");
                    return;
                }
            };
        let mut bgra = vec![0u8; (w * h * 4) as usize];
        let tex = gpu.bgra_texture_from(&bgra, w, h).expect("tex");
        let mut units: Vec<(Vec<u8>, bool)> = Vec::new();
        for i in 0..90u32 {
            for (j, px) in bgra.chunks_exact_mut(4).enumerate() {
                let v = ((j as u32).wrapping_add(i.wrapping_mul(977)) % 255) as u8;
                px[0] = v;
                px[1] = v.wrapping_add(40);
                px[2] = v.wrapping_add(80);
                px[3] = 255;
            }
            gpu.update_bgra(&tex, &bgra, w, h);
            let (slot, nv12) = gpu.convert(&tex).expect("convert").expect("slot");
            let out = enc.encode_texture(&nv12, i == 0).expect("encode");
            gpu.release(slot);
            units.extend(out.units);
        }
        let idrs = units.iter().skip(1).filter(|(_, k)| *k).count();
        assert_eq!(idrs, 0, "no IDR walls after the entry frame (got {idrs})");
        let mut dec = openh264::decoder::Decoder::with_api_config(
            openh264::OpenH264API::from_source(),
            openh264::decoder::DecoderConfig::new(),
        )
        .expect("decoder");
        let mut decoded = 0u32;
        for (d, _) in &units {
            if dec
                .decode(d)
                .expect("clean decode across refresh waves")
                .is_some()
            {
                decoded += 1;
            }
        }
        assert!(decoded as usize >= units.len() - 3, "decoded {decoded}");
    }
}
