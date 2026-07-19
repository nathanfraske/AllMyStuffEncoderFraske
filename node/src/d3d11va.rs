//! D3D11VA (DXVA) HEVC decode — the vendor-neutral receive rung.
//!
//! Why this exists: [`crate::nvdec`] gave Studio·Lossless a hardware
//! decoder, but only on NVIDIA — which made the posture an NVIDIA-*pair*
//! feature while every vendor's silicon (AMD, Intel, iGPUs included)
//! carries an HEVC decode engine, reachable through the same
//! `ID3D11VideoDecoder` interface on any Windows box with a GPU driver.
//! This module drives that interface directly: no codec packs (the
//! retired OS HEVC package is a Media Foundation concern, not a DXVA
//! one), no vendor SDK, no build-time dependency. With it, an NVIDIA
//! host streams bit-exact HEVC to an AMD or Intel viewer.
//!
//! D3D11VA is a *stateless* decode API: the driver runs the entropy and
//! reconstruction engines, and the host parses the bitstream and hands
//! over picture parameters, reference lists, and slice tables per frame.
//! So this module carries a deliberately scoped HEVC header parser —
//! sized to the streams our own encoders emit (8-bit 4:2:0, IPP with
//! short-term refs only; no tiles-with-lists, scaling lists, PCM,
//! long-term refs, or B slices) and to conformant streams shaped like
//! them. Anything outside that scope fails *soft* with a named reason:
//! the bridge drops the session and the stream re-enters at the next key
//! unit, exactly the openh264/NVDEC recovery shape. The DXVA structures
//! are hand-transcribed from the Windows SDK's `dxva.h` — which wraps
//! them in `#pragma pack(1)`, so the slice entry is 10 bytes, not the
//! naturally-aligned 12 — layouts pinned by size asserts, fill semantics
//! matched field-for-field to FFmpeg's battle-tested `dxva2_hevc.c`.
//!
//! Picture assembly: the send-side pacer splits each sliced AU into
//! several track sends and the daemon delivers each as its own sample,
//! so [`crate::video_decode::DecodeBridge`] feeds this decoder *chunks*
//! of a picture, not whole AUs. NVDEC's push parser absorbs that
//! internally; a stateless API cannot, so this module buffers slice NALs
//! and closes a picture when (a) a slice arrives carrying
//! `first_slice_segment_in_pic_flag`, (b) the RTP timestamp changes, or
//! (c) the stream's learned slices-per-picture count is reached. The
//! count is learned only after two matching, successfully submitted
//! boundary-closed pictures and only ratchets up (`max`), so one picture
//! missing chunks cannot teach a short count and cascade partial submits.
//! Steady state closes each picture on its own final chunk — no added
//! latency; only the first two pictures may wait for their successors.
//!
//! Threading contract: one owner thread (the route's decode thread),
//! same as the encoder twin. The D3D11 device is multithread-protected
//! anyway (shared `create_video_device` path).

#![cfg(windows)]

use std::collections::HashMap;

use windows::core::{Interface, GUID};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, ID3D11VideoContext, ID3D11VideoDecoder,
    ID3D11VideoDecoderOutputView, ID3D11VideoDevice, D3D11_BIND_DECODER, D3D11_CPU_ACCESS_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_TEX2D_VDOV, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING, D3D11_VDOV_DIMENSION_TEXTURE2D,
    D3D11_VIDEO_DECODER_BUFFER_BITSTREAM, D3D11_VIDEO_DECODER_BUFFER_DESC,
    D3D11_VIDEO_DECODER_BUFFER_INVERSE_QUANTIZATION_MATRIX,
    D3D11_VIDEO_DECODER_BUFFER_PICTURE_PARAMETERS, D3D11_VIDEO_DECODER_BUFFER_SLICE_CONTROL,
    D3D11_VIDEO_DECODER_BUFFER_TYPE, D3D11_VIDEO_DECODER_CONFIG, D3D11_VIDEO_DECODER_DESC,
    D3D11_VIDEO_DECODER_OUTPUT_VIEW_DESC, D3D11_VIDEO_DECODER_OUTPUT_VIEW_DESC_0,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};

use crate::nvdec::NvFrame;

/// `D3D11_DECODER_PROFILE_HEVC_VLD_MAIN` — the 8-bit 4:2:0 HEVC profile,
/// present on every HEVC-capable adapter of every vendor (the
/// decoder-profile probe in `gpu_pipeline` prints it as "HEVC VLD Main").
const HEVC_VLD_MAIN: GUID = GUID::from_u128(0x5b11d51b_2f4c_4452_bcc3_09f2a1160cc0);

/// `D3D11_DECODER_PROFILE_AV1_VLD_PROFILE0` — the 8/10-bit 4:2:0 AV1
/// profile, present on RDNA/Xe/Ampere+ (the profile probe prints it as
/// "AV1 VLD Profile0"). Named now for the AV1 rung; unused until
/// [`D3d11vaAv1`] is implemented. NOTE: AV1's DXVA structures
/// (`DXVA_PicParams_AV1`, tile buffers) are an entirely separate, larger
/// transcription unit than HEVC's — that's the bulk of the AV1 D3D11VA
/// work, not the plumbing here.
#[allow(dead_code)]
const AV1_VLD_PROFILE0: GUID = GUID::from_u128(0xb8be4ccb_cf53_46ba_8d59_d6b8a6da5d2a);

// ---------------------------------------------------------------------------
// DXVA structures — transcribed from dxva.h (Windows SDK), inside its
// `#pragma pack(1)`. Sizes pinned by asserts.
// ---------------------------------------------------------------------------

/// `DXVA_PicEntry_HEVC`: `Index7Bits | (AssociatedFlag << 7)`; 0xFF = empty.
type PicEntry = u8;
const PIC_ENTRY_EMPTY: PicEntry = 0xff;

/// `DXVA_PicParams_HEVC`.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct PicParamsHevc {
    pic_width_in_min_cbs_y: u16,
    pic_height_in_min_cbs_y: u16,
    /// chroma_format_idc:2 | separate_colour_plane:1 |
    /// bit_depth_luma_minus8:3 | bit_depth_chroma_minus8:3 |
    /// log2_max_pic_order_cnt_lsb_minus4:4 | NoPicReorderingFlag:1 |
    /// NoBiPredFlag:1 | reserved:1
    format_and_sequence_info: u16,
    curr_pic: PicEntry,
    sps_max_dec_pic_buffering_minus1: u8,
    log2_min_luma_coding_block_size_minus3: u8,
    log2_diff_max_min_luma_coding_block_size: u8,
    log2_min_transform_block_size_minus2: u8,
    log2_diff_max_min_transform_block_size: u8,
    max_transform_hierarchy_depth_inter: u8,
    max_transform_hierarchy_depth_intra: u8,
    num_short_term_ref_pic_sets: u8,
    num_long_term_ref_pics_sps: u8,
    num_ref_idx_l0_default_active_minus1: u8,
    num_ref_idx_l1_default_active_minus1: u8,
    init_qp_minus26: i8,
    uc_num_delta_pocs_of_ref_rps_idx: u8,
    w_num_bits_for_short_term_rps_in_slice: u16,
    reserved_bits2: u16,
    /// SPS tool bits: scaling_list:0 | amp:1 | sao:2 | pcm:3 |
    /// pcm_bd_luma:4..7 | pcm_bd_chroma:8..11 | pcm_log2_min:12..13 |
    /// pcm_log2_diff:14..15 | pcm_lf_disabled:16 | long_term_present:17 |
    /// temporal_mvp:18 | strong_intra_smoothing:19 | dependent_slices:20 |
    /// output_flag_present:21 | extra_slice_bits:22..24 |
    /// sign_data_hiding:25 | cabac_init_present:26
    coding_param_tool_flags: u32,
    /// PPS/picture bits: constrained_intra:0 | transform_skip:1 |
    /// cu_qp_delta:2 | slice_chroma_qp:3 | weighted_pred:4 |
    /// weighted_bipred:5 | transquant_bypass:6 | tiles:7 |
    /// entropy_sync:8 | uniform_spacing:9 | lf_across_tiles:10 |
    /// lf_across_slices:11 | deblock_override:12 | deblock_disabled:13 |
    /// lists_modification:14 | slice_ext:15 | Irap:16 | Idr:17 | Intra:18
    coding_setting_picture_property_flags: u32,
    pps_cb_qp_offset: i8,
    pps_cr_qp_offset: i8,
    num_tile_columns_minus1: u8,
    num_tile_rows_minus1: u8,
    column_width_minus1: [u16; 19],
    row_height_minus1: [u16; 21],
    diff_cu_qp_delta_depth: u8,
    pps_beta_offset_div2: i8,
    pps_tc_offset_div2: i8,
    log2_parallel_merge_level_minus2: u8,
    curr_pic_order_cnt_val: i32,
    ref_pic_list: [PicEntry; 15],
    reserved_bits5: u8,
    pic_order_cnt_val_list: [i32; 15],
    ref_pic_set_st_curr_before: [u8; 8],
    ref_pic_set_st_curr_after: [u8; 8],
    ref_pic_set_lt_curr: [u8; 8],
    reserved_bits6: u16,
    reserved_bits7: u16,
    status_report_feedback_number: u32,
}
const _: () = assert!(std::mem::size_of::<PicParamsHevc>() == 232);

/// `DXVA_Qmatrix_HEVC` — submitted flat (all 16s): our streams never
/// enable scaling lists, and the driver ignores this buffer when
/// `scaling_list_enabled_flag` is 0. Submitted anyway because that's the
/// shape every field driver was hardened against (FFmpeg always sends it).
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct QmatrixHevc {
    lists0: [[u8; 16]; 6],
    lists1: [[u8; 64]; 6],
    lists2: [[u8; 64]; 6],
    lists3: [[u8; 64]; 2],
    dc_size_id2: [u8; 6],
    dc_size_id3: [u8; 2],
}
const _: () = assert!(std::mem::size_of::<QmatrixHevc>() == 1000);

/// `DXVA_Slice_HEVC_Short` — 10 bytes under dxva.h's pack(1); the
/// naturally-aligned 12 would shear every entry after the first.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct SliceShort {
    bs_nal_unit_data_location: u32,
    slice_bytes_in_buffer: u32,
    w_bad_slice_chopping: u16,
}
const _: () = assert!(std::mem::size_of::<SliceShort>() == 10);

// ---------------------------------------------------------------------------
// Bitstream reading — RBSP extraction + the Exp-Golomb alphabet.
// ---------------------------------------------------------------------------

/// Strip emulation-prevention bytes (`00 00 03` → `00 00`) from a NAL
/// payload, capped at `limit` output bytes — header parsing never needs
/// more, and slice NALs are huge. The GPU gets the *raw* bytes; only our
/// own header reads use this.
fn rbsp(nal: &[u8], limit: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len().min(limit));
    let mut zeros = 0u32;
    for &b in nal {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
        if out.len() >= limit {
            break;
        }
    }
    out
}

/// MSB-first bit reader over an RBSP slice. Every read is checked — a
/// truncated header surfaces as a parse error, never a panic.
struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn bit_pos(&self) -> usize {
        self.pos
    }

    fn u(&mut self, n: u32) -> Result<u32, String> {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = self
                .data
                .get(self.pos / 8)
                .ok_or_else(|| "bitstream truncated".to_string())?;
            v = (v << 1) | u32::from((byte >> (7 - (self.pos % 8))) & 1);
            self.pos += 1;
        }
        Ok(v)
    }

    fn flag(&mut self) -> Result<bool, String> {
        Ok(self.u(1)? == 1)
    }

    /// ue(v) — unsigned Exp-Golomb.
    fn ue(&mut self) -> Result<u32, String> {
        let mut zeros = 0u32;
        while self.u(1)? == 0 {
            zeros += 1;
            if zeros > 31 {
                return Err("Exp-Golomb run too long (malformed)".into());
            }
        }
        Ok((1u32 << zeros) - 1 + self.u(zeros)?)
    }

    /// se(v) — signed Exp-Golomb: 0, 1, −1, 2, −2, …
    fn se(&mut self) -> Result<i32, String> {
        let k = self.ue()?;
        Ok(if k % 2 == 1 {
            (k / 2 + 1) as i32
        } else {
            -((k / 2) as i32)
        })
    }
}

// ---------------------------------------------------------------------------
// HEVC header parsing — the subset DXVA's picture parameters need.
// ---------------------------------------------------------------------------

/// One short-term reference picture set: `(delta_poc, used_by_curr)` with
/// negative deltas in `s0` (closest first) and positive in `s1`.
#[derive(Clone, Default, PartialEq)]
struct StRps {
    s0: Vec<(i32, bool)>,
    s1: Vec<(i32, bool)>,
}

impl StRps {
    fn num_delta_pocs(&self) -> usize {
        self.s0.len() + self.s1.len()
    }
}

/// `st_ref_pic_set(idx)`, spec 7.3.7 — both the explicit form and the
/// inter-set-predicted form (7.4.8's derivation). Returns the set plus
/// the reference set's NumDeltaPocs when prediction was used (DXVA's
/// `ucNumDeltaPocsOfRefRpsIdx`; 0 for explicit).
fn parse_st_rps(r: &mut BitReader, idx: usize, sets: &[StRps]) -> Result<(StRps, u8), String> {
    if idx != 0 && r.flag()? {
        // inter_ref_pic_set_prediction_flag
        let delta_idx_minus1 = if idx == sets.len() {
            r.ue()? as usize
        } else {
            0
        };
        let ref_idx = idx
            .checked_sub(1 + delta_idx_minus1)
            .ok_or("RPS delta_idx out of range")?;
        let rref = sets.get(ref_idx).ok_or("RPS reference set missing")?;
        let sign = r.flag()?;
        let abs_minus1 = r.ue()?;
        let delta_rps = if sign {
            -((abs_minus1 + 1) as i32)
        } else {
            (abs_minus1 + 1) as i32
        };
        let n = rref.num_delta_pocs();
        let mut used = vec![false; n + 1];
        let mut use_delta = vec![true; n + 1];
        for j in 0..=n {
            used[j] = r.flag()?;
            if !used[j] {
                use_delta[j] = r.flag()?;
            }
        }
        // Spec 7.4.8: project the reference set's deltas (and deltaRps
        // itself) through deltaRps; negatives land in s0 and positives in
        // s1, each in the spec's enumeration order.
        let mut s0 = Vec::new();
        for j in (0..rref.s1.len()).rev() {
            let d = rref.s1[j].0 + delta_rps;
            if d < 0 && use_delta[rref.s0.len() + j] {
                s0.push((d, used[rref.s0.len() + j]));
            }
        }
        if delta_rps < 0 && use_delta[n] {
            s0.push((delta_rps, used[n]));
        }
        for j in 0..rref.s0.len() {
            let d = rref.s0[j].0 + delta_rps;
            if d < 0 && use_delta[j] {
                s0.push((d, used[j]));
            }
        }
        let mut s1 = Vec::new();
        for j in (0..rref.s0.len()).rev() {
            let d = rref.s0[j].0 + delta_rps;
            if d > 0 && use_delta[j] {
                s1.push((d, used[j]));
            }
        }
        if delta_rps > 0 && use_delta[n] {
            s1.push((delta_rps, used[n]));
        }
        for j in 0..rref.s1.len() {
            let d = rref.s1[j].0 + delta_rps;
            if d > 0 && use_delta[rref.s0.len() + j] {
                s1.push((d, used[rref.s0.len() + j]));
            }
        }
        Ok((StRps { s0, s1 }, n as u8))
    } else {
        let num_neg = r.ue()? as usize;
        let num_pos = r.ue()? as usize;
        if num_neg + num_pos > 16 {
            return Err("RPS larger than any legal DPB".into());
        }
        let mut s0 = Vec::with_capacity(num_neg);
        let mut prev = 0i32;
        for _ in 0..num_neg {
            prev -= r.ue()? as i32 + 1;
            s0.push((prev, r.flag()?));
        }
        let mut s1 = Vec::with_capacity(num_pos);
        prev = 0;
        for _ in 0..num_pos {
            prev += r.ue()? as i32 + 1;
            s1.push((prev, r.flag()?));
        }
        Ok((StRps { s0, s1 }, 0))
    }
}

/// The SPS subset the picture parameters and the readback crop need.
/// `PartialEq` drives session reuse: an identical repeat (every IDR
/// re-sends it) keeps the decoder; any change rebuilds it.
#[derive(Clone, PartialEq)]
struct Sps {
    id: u32,
    chroma_format_idc: u32,
    separate_colour_plane: bool,
    pic_width: u32,
    pic_height: u32,
    /// Conformance-window crop in luma samples (left, right, top, bottom).
    crop: (u32, u32, u32, u32),
    bit_depth_luma_minus8: u32,
    bit_depth_chroma_minus8: u32,
    log2_max_poc_lsb: u32,
    max_dec_pic_buffering_minus1: u32,
    log2_min_cb_minus3: u32,
    log2_diff_max_min_cb: u32,
    log2_min_tb_minus2: u32,
    log2_diff_max_min_tb: u32,
    max_th_depth_inter: u32,
    max_th_depth_intra: u32,
    amp_enabled: bool,
    sao_enabled: bool,
    pcm_enabled: bool,
    pcm_bit_depth_luma_minus1: u32,
    pcm_bit_depth_chroma_minus1: u32,
    pcm_log2_min_cb_minus3: u32,
    pcm_log2_diff_max_min_cb: u32,
    pcm_loop_filter_disabled: bool,
    st_rps: Vec<StRps>,
    temporal_mvp: bool,
    strong_intra_smoothing: bool,
}

/// Skip `profile_tier_level(1, max_sub_layers_minus1)` — fixed-size
/// blocks this parser validates nothing from.
fn skip_profile_tier_level(r: &mut BitReader, max_sub_layers_minus1: u32) -> Result<(), String> {
    r.u(2)?; // general_profile_space
    r.u(1)?; // general_tier_flag
    r.u(5)?; // general_profile_idc
    r.u(32)?; // general_profile_compatibility_flag[32]
    r.u(32)?; // progressive/interlaced/non-packed/frame-only + reserved
    r.u(16)?; // …the rest of the 43 reserved bits + inbld
    r.u(8)?; // general_level_idc
    let mut profile_present = Vec::new();
    let mut level_present = Vec::new();
    for _ in 0..max_sub_layers_minus1 {
        profile_present.push(r.flag()?);
        level_present.push(r.flag()?);
    }
    if max_sub_layers_minus1 > 0 {
        for _ in max_sub_layers_minus1..8 {
            r.u(2)?; // reserved_zero_2bits
        }
    }
    for i in 0..max_sub_layers_minus1 as usize {
        if profile_present[i] {
            r.u(32)?;
            r.u(32)?;
            r.u(24)?; // 88 bits of sub-layer profile
        }
        if level_present[i] {
            r.u(8)?;
        }
    }
    Ok(())
}

fn parse_sps(rb: &[u8]) -> Result<Sps, String> {
    let mut r = BitReader::new(rb);
    r.u(4)?; // sps_video_parameter_set_id
    let max_sub_layers_minus1 = r.u(3)?;
    r.u(1)?; // sps_temporal_id_nesting_flag
    skip_profile_tier_level(&mut r, max_sub_layers_minus1)?;
    let id = r.ue()?;
    let chroma_format_idc = r.ue()?;
    if chroma_format_idc != 1 {
        return Err(format!(
            "unsupported chroma_format_idc {chroma_format_idc} (this rung is 4:2:0/NV12)"
        ));
    }
    let pic_width = r.ue()?;
    let pic_height = r.ue()?;
    if !(16..=8192).contains(&pic_width) || !(16..=8192).contains(&pic_height) {
        return Err(format!("implausible geometry {pic_width}×{pic_height}"));
    }
    let crop = if r.flag()? {
        // Conformance window, in chroma units — ×2 to luma for 4:2:0.
        // Red team: each of these is later used INDIVIDUALLY as an
        // absolute byte offset into the mapped surface (`read_surface`),
        // so validating only the sum is not enough — a crafted SPS can
        // make `l + rr` wrap under u32 past the guard while `l` alone is
        // ~2 GB, turning the offset into an out-of-bounds read. Multiply
        // and sum with checked arithmetic and reject any overflow; the
        // `< pic_width/height` (≤ 8192) test then bounds each component.
        let mul2 = |v: u32| v.checked_mul(2).ok_or("conformance window overflow");
        let l = mul2(r.ue()?)?;
        let rr = mul2(r.ue()?)?;
        let t = mul2(r.ue()?)?;
        let b = mul2(r.ue()?)?;
        let w_off = l.checked_add(rr).ok_or("conformance window overflow")?;
        let h_off = t.checked_add(b).ok_or("conformance window overflow")?;
        if w_off >= pic_width || h_off >= pic_height {
            return Err("conformance window swallows the picture".into());
        }
        (l, rr, t, b)
    } else {
        (0, 0, 0, 0)
    };
    let bit_depth_luma_minus8 = r.ue()?;
    let bit_depth_chroma_minus8 = r.ue()?;
    if bit_depth_luma_minus8 != 0 || bit_depth_chroma_minus8 != 0 {
        return Err("unsupported bit depth (this rung is 8-bit Main)".into());
    }
    let log2_max_poc_lsb = r.ue()? + 4;
    if log2_max_poc_lsb > 16 {
        return Err("log2_max_pic_order_cnt_lsb out of range".into());
    }
    let ordering_present = r.flag()?;
    let start = if ordering_present {
        0
    } else {
        max_sub_layers_minus1
    };
    let mut max_dec_pic_buffering_minus1 = 0;
    for _ in start..=max_sub_layers_minus1 {
        max_dec_pic_buffering_minus1 = r.ue()?; // keep the highest layer's
        r.ue()?; // sps_max_num_reorder_pics
        r.ue()?; // sps_max_latency_increase_plus1
    }
    // HEVC caps the DPB at 16 (minus1 ≤ 15). Bounding it here keeps the
    // later `+ 3` (ensure_session) and `as u8` (pic-params fill) honest —
    // an unbounded value would wrap under overflow-checks instead of
    // relying on a distant clamp.
    if max_dec_pic_buffering_minus1 > 15 {
        return Err("sps_max_dec_pic_buffering out of range".into());
    }
    let log2_min_cb_minus3 = r.ue()?;
    let log2_diff_max_min_cb = r.ue()?;
    let log2_min_tb_minus2 = r.ue()?;
    let log2_diff_max_min_tb = r.ue()?;
    // These log2 coding/transform-block parameters feed left-shifts and
    // coding-block-count geometry below. HEVC keeps each to a few bits
    // (CTB ≤ 64, min CB ≥ 8); a crafted large value would produce a
    // masked (wrong) shift in release and a shift-overflow panic under
    // overflow-checks. Reject out-of-range rather than misdecode.
    if log2_min_cb_minus3 > 8
        || log2_diff_max_min_cb > 8
        || log2_min_tb_minus2 > 8
        || log2_diff_max_min_tb > 8
    {
        return Err("coding-block log2 parameters out of range".into());
    }
    let max_th_depth_inter = r.ue()?;
    let max_th_depth_intra = r.ue()?;
    if r.flag()? {
        // scaling_list_enabled_flag — never set by our encoders; real
        // list parsing (and DXVA fill) is scope this rung doesn't carry.
        return Err("scaling lists unsupported (not emitted by our encoders)".into());
    }
    let amp_enabled = r.flag()?;
    let sao_enabled = r.flag()?;
    let pcm_enabled = r.flag()?;
    let (mut pcm_bd_l, mut pcm_bd_c, mut pcm_min, mut pcm_diff, mut pcm_lf) = (0, 0, 0, 0, false);
    if pcm_enabled {
        pcm_bd_l = r.u(4)?;
        pcm_bd_c = r.u(4)?;
        pcm_min = r.ue()?;
        pcm_diff = r.ue()?;
        pcm_lf = r.flag()?;
    }
    let num_st_rps = r.ue()? as usize;
    if num_st_rps > 64 {
        return Err("num_short_term_ref_pic_sets out of range".into());
    }
    let mut st_rps = Vec::with_capacity(num_st_rps);
    for i in 0..num_st_rps {
        let (set, _) = parse_st_rps(&mut r, i, &st_rps)?;
        st_rps.push(set);
    }
    if r.flag()? {
        // long_term_ref_pics_present_flag: the slice-header fields it
        // implies aren't parsed here — refuse loudly rather than misread.
        return Err("long-term reference pictures unsupported".into());
    }
    let temporal_mvp = r.flag()?;
    let strong_intra_smoothing = r.flag()?;
    // VUI and extensions: nothing DXVA needs.
    let min_cb = 1u32 << (log2_min_cb_minus3 + 3);
    if pic_width % min_cb != 0 || pic_height % min_cb != 0 {
        return Err("picture size not MinCb-aligned (malformed SPS)".into());
    }
    Ok(Sps {
        id,
        chroma_format_idc,
        separate_colour_plane: false, // only present when idc == 3
        pic_width,
        pic_height,
        crop,
        bit_depth_luma_minus8,
        bit_depth_chroma_minus8,
        log2_max_poc_lsb,
        max_dec_pic_buffering_minus1,
        log2_min_cb_minus3,
        log2_diff_max_min_cb,
        log2_min_tb_minus2,
        log2_diff_max_min_tb,
        max_th_depth_inter,
        max_th_depth_intra,
        amp_enabled,
        sao_enabled,
        pcm_enabled,
        pcm_bit_depth_luma_minus1: pcm_bd_l,
        pcm_bit_depth_chroma_minus1: pcm_bd_c,
        pcm_log2_min_cb_minus3: pcm_min,
        pcm_log2_diff_max_min_cb: pcm_diff,
        pcm_loop_filter_disabled: pcm_lf,
        st_rps,
        temporal_mvp,
        strong_intra_smoothing,
    })
}

/// The PPS subset the picture parameters need.
#[derive(Clone, PartialEq)]
struct Pps {
    id: u32,
    sps_id: u32,
    dependent_slice_segments: bool,
    output_flag_present: bool,
    num_extra_slice_header_bits: u32,
    sign_data_hiding: bool,
    cabac_init_present: bool,
    num_ref_idx_l0_default_minus1: u32,
    num_ref_idx_l1_default_minus1: u32,
    init_qp_minus26: i32,
    constrained_intra_pred: bool,
    transform_skip: bool,
    cu_qp_delta: bool,
    diff_cu_qp_delta_depth: u32,
    cb_qp_offset: i32,
    cr_qp_offset: i32,
    slice_chroma_qp_offsets_present: bool,
    weighted_pred: bool,
    weighted_bipred: bool,
    transquant_bypass: bool,
    tiles_enabled: bool,
    entropy_coding_sync: bool,
    num_tile_columns_minus1: u32,
    num_tile_rows_minus1: u32,
    uniform_spacing: bool,
    column_widths_minus1: Vec<u32>,
    row_heights_minus1: Vec<u32>,
    loop_filter_across_tiles: bool,
    loop_filter_across_slices: bool,
    deblocking_override_enabled: bool,
    deblocking_disabled: bool,
    beta_offset_div2: i32,
    tc_offset_div2: i32,
    lists_modification_present: bool,
    log2_parallel_merge_minus2: u32,
    slice_header_extension_present: bool,
}

fn parse_pps(rb: &[u8]) -> Result<Pps, String> {
    let mut r = BitReader::new(rb);
    let id = r.ue()?;
    let sps_id = r.ue()?;
    let dependent_slice_segments = r.flag()?;
    let output_flag_present = r.flag()?;
    let num_extra_slice_header_bits = r.u(3)?;
    let sign_data_hiding = r.flag()?;
    let cabac_init_present = r.flag()?;
    let num_ref_idx_l0_default_minus1 = r.ue()?;
    let num_ref_idx_l1_default_minus1 = r.ue()?;
    let init_qp_minus26 = r.se()?;
    let constrained_intra_pred = r.flag()?;
    let transform_skip = r.flag()?;
    let cu_qp_delta = r.flag()?;
    let diff_cu_qp_delta_depth = if cu_qp_delta { r.ue()? } else { 0 };
    let cb_qp_offset = r.se()?;
    let cr_qp_offset = r.se()?;
    let slice_chroma_qp_offsets_present = r.flag()?;
    let weighted_pred = r.flag()?;
    let weighted_bipred = r.flag()?;
    let transquant_bypass = r.flag()?;
    let tiles_enabled = r.flag()?;
    let entropy_coding_sync = r.flag()?;
    let (mut cols, mut rows, mut uniform) = (0, 0, true);
    let (mut col_w, mut row_h) = (Vec::new(), Vec::new());
    let mut lf_tiles = true;
    if tiles_enabled {
        cols = r.ue()?;
        rows = r.ue()?;
        if cols >= 19 || rows >= 21 {
            return Err("tile grid larger than DXVA carries".into());
        }
        uniform = r.flag()?;
        if !uniform {
            for _ in 0..cols {
                col_w.push(r.ue()?);
            }
            for _ in 0..rows {
                row_h.push(r.ue()?);
            }
        }
        lf_tiles = r.flag()?;
    }
    let loop_filter_across_slices = r.flag()?;
    let (mut db_override, mut db_disabled) = (false, false);
    let (mut beta2, mut tc2) = (0, 0);
    if r.flag()? {
        // deblocking_filter_control_present_flag
        db_override = r.flag()?;
        db_disabled = r.flag()?;
        if !db_disabled {
            beta2 = r.se()?;
            tc2 = r.se()?;
        }
    }
    if r.flag()? {
        // pps_scaling_list_data_present_flag
        return Err("scaling lists unsupported (not emitted by our encoders)".into());
    }
    let lists_modification_present = r.flag()?;
    let log2_parallel_merge_minus2 = r.ue()?;
    let slice_header_extension_present = r.flag()?;
    Ok(Pps {
        id,
        sps_id,
        dependent_slice_segments,
        output_flag_present,
        num_extra_slice_header_bits,
        sign_data_hiding,
        cabac_init_present,
        num_ref_idx_l0_default_minus1,
        num_ref_idx_l1_default_minus1,
        init_qp_minus26,
        constrained_intra_pred,
        transform_skip,
        cu_qp_delta,
        diff_cu_qp_delta_depth,
        cb_qp_offset,
        cr_qp_offset,
        slice_chroma_qp_offsets_present,
        weighted_pred,
        weighted_bipred,
        transquant_bypass,
        tiles_enabled,
        entropy_coding_sync,
        num_tile_columns_minus1: cols,
        num_tile_rows_minus1: rows,
        uniform_spacing: uniform,
        column_widths_minus1: col_w,
        row_heights_minus1: row_h,
        loop_filter_across_tiles: lf_tiles,
        loop_filter_across_slices,
        deblocking_override_enabled: db_override,
        deblocking_disabled: db_disabled,
        beta_offset_div2: beta2,
        tc_offset_div2: tc2,
        lists_modification_present,
        log2_parallel_merge_minus2,
        slice_header_extension_present,
    })
}

/// What the first slice of a picture declares — everything picture-level
/// the DXVA parameters need.
struct SliceHead {
    pps_id: u32,
    poc_lsb: u32,
    rps: StRps,
    /// Exact bit length of the slice's own `st_ref_pic_set` (DXVA's
    /// `wNumBitsForShortTermRPSInSlice`); 0 when the SPS-index form was
    /// used.
    rps_bits: u16,
    /// `ucNumDeltaPocsOfRefRpsIdx` — nonzero only for a slice-local RPS
    /// with inter-set prediction.
    ref_rps_num_delta_pocs: u8,
}

fn parse_slice_header(
    nal_type: u8,
    rb: &[u8],
    spss: &HashMap<u32, Sps>,
    ppss: &HashMap<u32, Pps>,
) -> Result<SliceHead, String> {
    let mut r = BitReader::new(rb);
    if !r.flag()? {
        // first_slice_segment_in_pic_flag == 0: this picture's opening
        // chunk was lost upstream — nothing here can decode.
        return Err("picture opened mid-slice (its first chunk was lost)".into());
    }
    if (16..=23).contains(&nal_type) {
        r.flag()?; // no_output_of_prior_pics_flag
    }
    let pps_id = r.ue()?;
    let pps = ppss
        .get(&pps_id)
        .ok_or_else(|| format!("slice names PPS {pps_id} before it arrived"))?;
    let sps = spss
        .get(&pps.sps_id)
        .ok_or_else(|| format!("PPS {pps_id} names SPS {} before it arrived", pps.sps_id))?;
    for _ in 0..pps.num_extra_slice_header_bits {
        r.flag()?;
    }
    let slice_type = r.ue()?;
    if slice_type == 0 {
        return Err("B slices unsupported (our encoders are IPP)".into());
    }
    if pps.output_flag_present {
        r.flag()?; // pic_output_flag
    }
    if sps.separate_colour_plane {
        r.u(2)?; // colour_plane_id
    }
    if nal_type == 19 || nal_type == 20 {
        // IDR: no POC, no RPS.
        return Ok(SliceHead {
            pps_id,
            poc_lsb: 0,
            rps: StRps::default(),
            rps_bits: 0,
            ref_rps_num_delta_pocs: 0,
        });
    }
    let poc_lsb = r.u(sps.log2_max_poc_lsb)?;
    let (rps, rps_bits, ref_ndp) = if !r.flag()? {
        // short_term_ref_pic_set_sps_flag == 0: the set is right here.
        let start = r.bit_pos();
        let (rps, ndp) = parse_st_rps(&mut r, sps.st_rps.len(), &sps.st_rps)?;
        (rps, (r.bit_pos() - start) as u16, ndp)
    } else {
        let idx = if sps.st_rps.len() > 1 {
            let bits = usize::BITS - (sps.st_rps.len() - 1).leading_zeros();
            r.u(bits)? as usize
        } else {
            0
        };
        let rps = sps
            .st_rps
            .get(idx)
            .ok_or("slice RPS index out of range")?
            .clone();
        (rps, 0, 0)
    };
    // Long-term refs were refused at SPS parse; nothing further needed.
    Ok(SliceHead {
        pps_id,
        poc_lsb,
        rps,
        rps_bits,
        ref_rps_num_delta_pocs: ref_ndp,
    })
}

// ---------------------------------------------------------------------------
// The decoder session.
// ---------------------------------------------------------------------------

/// A decoded picture still in the reference set, keyed by output surface.
struct DpbEntry {
    surface: u8,
    poc: i32,
}

/// The per-SPS D3D11 half: decoder + surface pool + staging readback.
struct Session {
    sps: Sps,
    decoder: ID3D11VideoDecoder,
    /// The NV12 texture array the decoder writes into; the views index
    /// its slices.
    textures: ID3D11Texture2D,
    views: Vec<ID3D11VideoDecoderOutputView>,
    staging: ID3D11Texture2D,
    tex_h: u32,
    refs: Vec<DpbEntry>,
    /// `(PicOrderCntMsb, pic_order_cnt_lsb)` of the previous Tid0
    /// reference picture — the anchor of 8.3.1's POC derivation.
    prev_poc: (i32, i32),
    has_prev: bool,
}

/// One buffered (not yet submitted) picture: its slice NALs, arrival
/// timestamp, and the NAL type of its opening slice.
struct PendingPicture {
    slices: Vec<Vec<u8>>,
    ts_us: u64,
    nal_type: u8,
}

/// Conservative evidence for the fixed slice count our NVENC sessions
/// request. A single boundary-closed picture is not enough: if the first
/// picture lost a chunk, some drivers can still accept the partial submit,
/// and learning that short count would then close every healthy successor
/// early. Two equal, successfully submitted boundary closes establish the
/// steady count; later observations may raise it but never lower it.
#[derive(Default)]
struct SliceCountLearner {
    learned: usize,
    candidate: usize,
    confirmations: u8,
}

/// Once a stable slices-per-picture count is known, a timestamp/first-slice
/// boundary with fewer slices is evidence of loss, not completion. Submitting
/// that partial picture can paint green/black bands and poison its reference
/// chain; drop it so the bridge requests a clean entry instead.
fn boundary_picture_is_complete(expected: Option<usize>, observed: usize) -> bool {
    expected.is_none_or(|expected| observed >= expected)
}

impl SliceCountLearner {
    fn expected(&self) -> Option<usize> {
        (self.learned > 0).then_some(self.learned)
    }

    fn observe_boundary(&mut self, count: usize) {
        if count == 0 {
            return;
        }
        if self.candidate == count {
            self.confirmations = self.confirmations.saturating_add(1);
        } else {
            self.candidate = count;
            self.confirmations = 1;
        }
        if self.confirmations >= 2 {
            self.learned = self.learned.max(count);
        }
    }
}

/// A successful `DecoderBeginFrame` owns a transaction until exactly one
/// `DecoderEndFrame`. The decode body has many fallible buffer/submit steps;
/// this guard closes the frame on every early return and exposes `finish` for
/// the success path so an `EndFrame` error is still reported to the caller.
struct DecoderFrameGuard<'a> {
    ctx: &'a ID3D11VideoContext,
    decoder: &'a ID3D11VideoDecoder,
    active: bool,
}

impl<'a> DecoderFrameGuard<'a> {
    unsafe fn begin(
        ctx: &'a ID3D11VideoContext,
        decoder: &'a ID3D11VideoDecoder,
        view: &ID3D11VideoDecoderOutputView,
    ) -> Result<Self, String> {
        // The engine can still be draining a previous frame; the
        // documented response is "try again shortly".
        let mut tries = 0;
        loop {
            match ctx.DecoderBeginFrame(decoder, view, 0, None) {
                Ok(()) => {
                    return Ok(Self {
                        ctx,
                        decoder,
                        active: true,
                    });
                }
                Err(e) if tries < 50 && matches!(e.code().0 as u32, 0x8000_000a | 0x887a_000a) => {
                    // E_PENDING / DXGI_ERROR_WAS_STILL_DRAWING
                    tries += 1;
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(e) => return Err(format!("DecoderBeginFrame: {e}")),
            }
        }
    }

    unsafe fn finish(mut self) -> Result<(), String> {
        let result = self
            .ctx
            .DecoderEndFrame(self.decoder)
            .map_err(|e| format!("DecoderEndFrame: {e}"));
        // EndFrame was attempted even when it reported failure; never issue a
        // second EndFrame from Drop against the same transaction.
        self.active = false;
        result
    }
}

impl Drop for DecoderFrameGuard<'_> {
    fn drop(&mut self) {
        if self.active {
            unsafe {
                let _ = self.ctx.DecoderEndFrame(self.decoder);
            }
            self.active = false;
        }
    }
}

/// Write one DXVA buffer: get, bounds-check, copy, release.
unsafe fn fill_buffer(
    ctx: &ID3D11VideoContext,
    decoder: &ID3D11VideoDecoder,
    kind: D3D11_VIDEO_DECODER_BUFFER_TYPE,
    bytes: &[u8],
) -> Result<(), String> {
    let mut size = 0u32;
    let mut ptr: *mut core::ffi::c_void = std::ptr::null_mut();
    ctx.GetDecoderBuffer(decoder, kind, &mut size, &mut ptr)
        .map_err(|e| format!("GetDecoderBuffer({kind:?}): {e}"))?;
    if ptr.is_null() {
        let _ = ctx.ReleaseDecoderBuffer(decoder, kind);
        return Err(format!("GetDecoderBuffer({kind:?}) returned a null buffer"));
    }
    if (size as usize) < bytes.len() {
        let _ = ctx.ReleaseDecoderBuffer(decoder, kind);
        return Err(format!(
            "decoder buffer {kind:?} too small ({size} < {})",
            bytes.len()
        ));
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
    ctx.ReleaseDecoderBuffer(decoder, kind)
        .map_err(|e| format!("ReleaseDecoderBuffer({kind:?}): {e}"))
}

/// A D3D11VA HEVC session: Annex-B units (whole AUs or the pacer's
/// chunks) in, NV12 pictures out. Mirrors [`crate::nvdec::NvdecHevc`]'s
/// seam so the bridge's HEVC ladder treats the rungs alike.
pub struct D3d11vaHevc {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    spss: HashMap<u32, Sps>,
    ppss: HashMap<u32, Pps>,
    session: Option<Session>,
    pending: Option<PendingPicture>,
    /// Slices per picture, learned from two matching boundary-closed
    /// pictures. The count only ratchets up, so one lossy picture cannot
    /// teach a short completion count (see module docs).
    slice_counts: SliceCountLearner,
    report: u32,
}

// SAFETY: owned and driven by one route-decode thread (the same contract
// as NvdecHevc); the device is multithread-protected besides.
unsafe impl Send for D3d11vaHevc {}

impl D3d11vaHevc {
    /// Probe the default adapter for HEVC Main decode and hold a device
    /// for it. `Err` = the rung doesn't exist on this box (no HEVC
    /// profile, no NV12 decode target) — the ladder logs it and moves on.
    pub fn open() -> Result<Self, String> {
        let (device, context) = crate::gpu_pipeline::create_video_device()?;
        unsafe {
            let video_device: ID3D11VideoDevice = device
                .cast()
                .map_err(|e| format!("ID3D11VideoDevice: {e}"))?;
            let video_context: ID3D11VideoContext = context
                .cast()
                .map_err(|e| format!("ID3D11VideoContext: {e}"))?;
            let n = video_device.GetVideoDecoderProfileCount();
            let found = (0..n).any(|i| {
                video_device
                    .GetVideoDecoderProfile(i)
                    .is_ok_and(|g| g == HEVC_VLD_MAIN)
            });
            if !found {
                return Err(format!(
                    "no HEVC Main decode profile among this adapter's {n}"
                ));
            }
            let nv12_ok = video_device
                .CheckVideoDecoderFormat(&HEVC_VLD_MAIN, DXGI_FORMAT_NV12)
                .map_err(|e| format!("CheckVideoDecoderFormat: {e}"))?
                .as_bool();
            if !nv12_ok {
                return Err("HEVC Main profile present but won't decode to NV12".into());
            }
            Ok(Self {
                device,
                context,
                video_device,
                video_context,
                spss: HashMap::new(),
                ppss: HashMap::new(),
                session: None,
                pending: None,
                slice_counts: SliceCountLearner::default(),
                report: 0,
            })
        }
    }

    pub fn label(&self) -> &'static str {
        "D3D11VA (HEVC, hardware, vendor-neutral)"
    }

    /// Feed one delivered unit; returns every picture it completed (the
    /// unit's own when the learned slice count closes it, its predecessor
    /// when a boundary does). An `Err` wedges the session — the bridge
    /// drops it and re-enters at the sender's next IDR.
    pub fn decode(&mut self, au: &[u8], ts_us: u64) -> Result<Vec<NvFrame>, String> {
        let mut out = Vec::new();
        let mut i = 0usize;
        // Annex-B walk: each NAL's byte range, start code stripped.
        while i + 3 < au.len() {
            let three = au[i] == 0 && au[i + 1] == 0 && au[i + 2] == 1;
            let four = !three
                && au[i] == 0
                && au[i + 1] == 0
                && au[i + 2] == 0
                && au.get(i + 3) == Some(&1);
            if !three && !four {
                i += 1;
                continue;
            }
            let start = i + if three { 3 } else { 4 };
            let mut end = au.len();
            let mut j = start;
            while j + 2 < au.len() {
                if au[j] == 0
                    && au[j + 1] == 0
                    && (au[j + 2] == 1 || (au[j + 2] == 0 && au.get(j + 3) == Some(&1)))
                {
                    end = j;
                    break;
                }
                j += 1;
            }
            self.take_nal(&au[start..end], ts_us, &mut out)?;
            i = end;
        }
        Ok(out)
    }

    /// Route one NAL: parameter sets update the stores (closing any
    /// buffered picture first — they open a new AU), slices buffer, and
    /// every close point drains through [`Self::close_pending`].
    fn take_nal(&mut self, nal: &[u8], ts_us: u64, out: &mut Vec<NvFrame>) -> Result<(), String> {
        if nal.len() < 3 {
            return Ok(());
        }
        let nal_type = (nal[0] >> 1) & 0x3f;
        match nal_type {
            // VPS/SPS/PPS/AUD/prefix-SEI only open AUs — a buffered
            // picture is complete when one arrives.
            32..=35 | 39 => {
                self.close_pending(out, true)?;
                match nal_type {
                    33 => {
                        let sps = parse_sps(&rbsp(&nal[2..], 4096))?;
                        self.spss.insert(sps.id, sps);
                    }
                    34 => {
                        let pps = parse_pps(&rbsp(&nal[2..], 4096))?;
                        self.ppss.insert(pps.id, pps);
                    }
                    _ => {}
                }
            }
            // Slice NALs of the trailing and IRAP families.
            0..=9 | 16..=21 => {
                let first_in_pic = nal[2] & 0x80 != 0;
                let boundary = self
                    .pending
                    .as_ref()
                    .is_some_and(|p| first_in_pic || p.ts_us != ts_us);
                if boundary {
                    self.close_pending(out, true)?;
                }
                let pend = self.pending.get_or_insert_with(|| PendingPicture {
                    slices: Vec::new(),
                    ts_us,
                    nal_type,
                });
                // Red team: a stream whose first picture never closes (every
                // slice carries first_in_pic=0 with an unchanging ts) never
                // teaches `slice_counts`, so without a cap `slices` grows
                // per access unit until the heap is exhausted (abort). Real
                // pictures have far fewer than this even at 8K; a stream that
                // exceeds it is malformed — drop the picture and let the
                // bridge re-key.
                const MAX_SLICES_PER_PICTURE: usize = 4096;
                if pend.slices.len() >= MAX_SLICES_PER_PICTURE {
                    self.pending = None;
                    return Err("slice count exceeds per-picture cap — dropping".into());
                }
                pend.slices.push(nal.to_vec());
                // The learned count closes a picture on its final slice —
                // the zero-latency steady state.
                if self
                    .slice_counts
                    .expected()
                    .is_some_and(|expected| pend.slices.len() >= expected)
                {
                    self.close_pending(out, false)?;
                }
            }
            _ => {} // suffix SEI, EOS/EOB, reserved: nothing to do
        }
        Ok(())
    }

    /// Submit the buffered picture (if any) and hand back its pixels. The
    /// slices-per-picture ratchet learns only from successful closes.
    fn close_pending(
        &mut self,
        out: &mut Vec<NvFrame>,
        boundary_evidence: bool,
    ) -> Result<(), String> {
        let Some(pend) = self.pending.take() else {
            return Ok(());
        };
        if pend.slices.is_empty() {
            return Ok(());
        }
        let count = pend.slices.len();
        if boundary_evidence && !boundary_picture_is_complete(self.slice_counts.expected(), count) {
            return Err(format!(
                "incomplete HEVC picture at boundary: received {count}/{} slices",
                self.slice_counts.expected().unwrap_or_default()
            ));
        }
        let frame = self.submit_picture(&pend)?;
        // After: a session rebuild inside submit resets the learner, and this
        // boundary close becomes the first observation for the fresh session.
        // Learned-count closes merely confirm the count already in force;
        // feeding them back would not be independent evidence.
        if boundary_evidence {
            self.slice_counts.observe_boundary(count);
        }
        out.push(frame);
        Ok(())
    }

    /// The stateless-decode transaction for one complete picture: parse
    /// its first slice header, run POC/RPS/DPB, fill the DXVA buffers,
    /// BeginFrame→Submit→EndFrame, read the surface back, update the DPB.
    fn submit_picture(&mut self, pend: &PendingPicture) -> Result<NvFrame, String> {
        let head = parse_slice_header(
            pend.nal_type,
            &rbsp(&pend.slices[0][2..], 512),
            &self.spss,
            &self.ppss,
        )?;
        let pps = self.ppss.get(&head.pps_id).ok_or("PPS vanished")?.clone();
        let sps = self.spss.get(&pps.sps_id).ok_or("SPS vanished")?.clone();
        self.ensure_session(&sps)?;
        self.report = self.report.wrapping_add(1);
        let report = self.report;
        let idr = pend.nal_type == 19 || pend.nal_type == 20;
        let irap = (16..=21).contains(&pend.nal_type);
        // Sub-layer non-reference NALs (TRAIL_N and kin) never join the
        // DPB or anchor POC.
        let slnr = pend.nal_type <= 14 && pend.nal_type.is_multiple_of(2);

        let session = self.session.as_mut().ok_or("no session")?;

        // --- POC (spec 8.3.1) -------------------------------------------------
        let poc = if idr {
            0
        } else {
            let max = 1i32 << sps.log2_max_poc_lsb;
            let lsb = head.poc_lsb as i32;
            let (prev_msb, prev_lsb) = session.prev_poc;
            let msb = if !session.has_prev {
                0
            } else if lsb < prev_lsb && prev_lsb - lsb >= max / 2 {
                prev_msb + max
            } else if lsb > prev_lsb && lsb - prev_lsb > max / 2 {
                prev_msb - max
            } else {
                prev_msb
            };
            msb + lsb
        };
        if !slnr {
            session.prev_poc = (poc - head.poc_lsb as i32, head.poc_lsb as i32);
            session.has_prev = true;
        }

        // --- RPS → reference retention + the DXVA POC sets --------------------
        let mut before = Vec::new();
        let mut after = Vec::new();
        let mut foll = Vec::new();
        for &(d, used) in &head.rps.s0 {
            (if used { &mut before } else { &mut foll }).push(poc + d);
        }
        for &(d, used) in &head.rps.s1 {
            (if used { &mut after } else { &mut foll }).push(poc + d);
        }
        if idr {
            session.refs.clear();
        } else {
            let keep: Vec<i32> = before.iter().chain(&after).chain(&foll).copied().collect();
            session.refs.retain(|e| keep.contains(&e.poc));
            for need in before.iter().chain(&after) {
                if !session.refs.iter().any(|e| e.poc == *need) {
                    return Err(format!("reference POC {need} missing (loss upstream)"));
                }
            }
        }
        if session.refs.len() > 15 {
            return Err("reference set exceeds DXVA's 15 slots".into());
        }

        // --- Surface pick -----------------------------------------------------
        let pool = session.views.len() as u8;
        let curr = (0..pool)
            .find(|s| !session.refs.iter().any(|e| e.surface == *s))
            .ok_or("no free decode surface (DPB overflow)")?;

        // --- Fill the DXVA picture parameters ---------------------------------
        let mut pp: PicParamsHevc = unsafe { std::mem::zeroed() };
        let min_cb = sps.log2_min_cb_minus3 + 3;
        pp.pic_width_in_min_cbs_y = (sps.pic_width >> min_cb) as u16;
        pp.pic_height_in_min_cbs_y = (sps.pic_height >> min_cb) as u16;
        pp.format_and_sequence_info = (sps.chroma_format_idc
            | (u32::from(sps.separate_colour_plane) << 2)
            | (sps.bit_depth_luma_minus8 << 3)
            | (sps.bit_depth_chroma_minus8 << 6)
            | ((sps.log2_max_poc_lsb - 4) << 9)) as u16;
        pp.curr_pic = curr;
        pp.sps_max_dec_pic_buffering_minus1 = sps.max_dec_pic_buffering_minus1 as u8;
        pp.log2_min_luma_coding_block_size_minus3 = sps.log2_min_cb_minus3 as u8;
        pp.log2_diff_max_min_luma_coding_block_size = sps.log2_diff_max_min_cb as u8;
        pp.log2_min_transform_block_size_minus2 = sps.log2_min_tb_minus2 as u8;
        pp.log2_diff_max_min_transform_block_size = sps.log2_diff_max_min_tb as u8;
        pp.max_transform_hierarchy_depth_inter = sps.max_th_depth_inter as u8;
        pp.max_transform_hierarchy_depth_intra = sps.max_th_depth_intra as u8;
        pp.num_short_term_ref_pic_sets = sps.st_rps.len() as u8;
        pp.num_ref_idx_l0_default_active_minus1 = pps.num_ref_idx_l0_default_minus1 as u8;
        pp.num_ref_idx_l1_default_active_minus1 = pps.num_ref_idx_l1_default_minus1 as u8;
        pp.init_qp_minus26 = pps.init_qp_minus26 as i8;
        pp.uc_num_delta_pocs_of_ref_rps_idx = head.ref_rps_num_delta_pocs;
        pp.w_num_bits_for_short_term_rps_in_slice = head.rps_bits;
        pp.coding_param_tool_flags = (u32::from(sps.amp_enabled) << 1)
            | (u32::from(sps.sao_enabled) << 2)
            | (u32::from(sps.pcm_enabled) << 3)
            | (if sps.pcm_enabled {
                (sps.pcm_bit_depth_luma_minus1 << 4)
                    | (sps.pcm_bit_depth_chroma_minus1 << 8)
                    | (sps.pcm_log2_min_cb_minus3 << 12)
                    | (sps.pcm_log2_diff_max_min_cb << 14)
                    | (u32::from(sps.pcm_loop_filter_disabled) << 16)
            } else {
                0
            })
            | (u32::from(sps.temporal_mvp) << 18)
            | (u32::from(sps.strong_intra_smoothing) << 19)
            | (u32::from(pps.dependent_slice_segments) << 20)
            | (u32::from(pps.output_flag_present) << 21)
            | (pps.num_extra_slice_header_bits << 22)
            | (u32::from(pps.sign_data_hiding) << 25)
            | (u32::from(pps.cabac_init_present) << 26);
        pp.coding_setting_picture_property_flags = u32::from(pps.constrained_intra_pred)
            | (u32::from(pps.transform_skip) << 1)
            | (u32::from(pps.cu_qp_delta) << 2)
            | (u32::from(pps.slice_chroma_qp_offsets_present) << 3)
            | (u32::from(pps.weighted_pred) << 4)
            | (u32::from(pps.weighted_bipred) << 5)
            | (u32::from(pps.transquant_bypass) << 6)
            | (u32::from(pps.tiles_enabled) << 7)
            | (u32::from(pps.entropy_coding_sync) << 8)
            | (u32::from(pps.uniform_spacing) << 9)
            | (u32::from(pps.tiles_enabled && pps.loop_filter_across_tiles) << 10)
            | (u32::from(pps.loop_filter_across_slices) << 11)
            | (u32::from(pps.deblocking_override_enabled) << 12)
            | (u32::from(pps.deblocking_disabled) << 13)
            | (u32::from(pps.lists_modification_present) << 14)
            | (u32::from(pps.slice_header_extension_present) << 15)
            | (u32::from(irap) << 16)
            | (u32::from(idr) << 17)
            | (u32::from(irap) << 18);
        pp.pps_cb_qp_offset = pps.cb_qp_offset as i8;
        pp.pps_cr_qp_offset = pps.cr_qp_offset as i8;
        if pps.tiles_enabled {
            pp.num_tile_columns_minus1 = pps.num_tile_columns_minus1 as u8;
            pp.num_tile_rows_minus1 = pps.num_tile_rows_minus1 as u8;
            let mut cols = [0u16; 19];
            for (dst, src) in cols.iter_mut().zip(&pps.column_widths_minus1) {
                *dst = *src as u16;
            }
            pp.column_width_minus1 = cols;
            let mut rows = [0u16; 21];
            for (dst, src) in rows.iter_mut().zip(&pps.row_heights_minus1) {
                *dst = *src as u16;
            }
            pp.row_height_minus1 = rows;
        }
        pp.diff_cu_qp_delta_depth = pps.diff_cu_qp_delta_depth as u8;
        pp.pps_beta_offset_div2 = pps.beta_offset_div2 as i8;
        pp.pps_tc_offset_div2 = pps.tc_offset_div2 as i8;
        pp.log2_parallel_merge_level_minus2 = pps.log2_parallel_merge_minus2 as u8;
        pp.curr_pic_order_cnt_val = poc;
        let mut ref_list = [PIC_ENTRY_EMPTY; 15];
        let mut poc_list = [0i32; 15];
        for (i, e) in session.refs.iter().take(15).enumerate() {
            ref_list[i] = e.surface;
            poc_list[i] = e.poc;
        }
        pp.ref_pic_list = ref_list;
        pp.pic_order_cnt_val_list = poc_list;
        let index_of = |p: i32, refs: &[DpbEntry]| -> Option<u8> {
            refs.iter().position(|e| e.poc == p).map(|i| i as u8)
        };
        let mut before_set = [PIC_ENTRY_EMPTY; 8];
        for (dst, p) in before_set.iter_mut().zip(&before) {
            *dst = index_of(*p, &session.refs).ok_or("before-set POC left the list")?;
        }
        let mut after_set = [PIC_ENTRY_EMPTY; 8];
        for (dst, p) in after_set.iter_mut().zip(&after) {
            *dst = index_of(*p, &session.refs).ok_or("after-set POC left the list")?;
        }
        pp.ref_pic_set_st_curr_before = before_set;
        pp.ref_pic_set_st_curr_after = after_set;
        pp.ref_pic_set_lt_curr = [PIC_ENTRY_EMPTY; 8];
        pp.status_report_feedback_number = report;

        let qm = QmatrixHevc {
            lists0: [[16; 16]; 6],
            lists1: [[16; 64]; 6],
            lists2: [[16; 64]; 6],
            lists3: [[16; 64]; 2],
            dc_size_id2: [16; 6],
            dc_size_id3: [16; 2],
        };

        // --- The decode transaction ------------------------------------------
        unsafe {
            let view = &session.views[curr as usize];
            let frame = DecoderFrameGuard::begin(&self.video_context, &session.decoder, view)?;

            fill_buffer(
                &self.video_context,
                &session.decoder,
                D3D11_VIDEO_DECODER_BUFFER_PICTURE_PARAMETERS,
                std::slice::from_raw_parts(
                    &pp as *const PicParamsHevc as *const u8,
                    std::mem::size_of::<PicParamsHevc>(),
                ),
            )?;
            fill_buffer(
                &self.video_context,
                &session.decoder,
                D3D11_VIDEO_DECODER_BUFFER_INVERSE_QUANTIZATION_MATRIX,
                std::slice::from_raw_parts(
                    &qm as *const QmatrixHevc as *const u8,
                    std::mem::size_of::<QmatrixHevc>(),
                ),
            )?;

            // Bitstream: every slice NAL re-prefixed with 00 00 01, then
            // zero-padded toward the spec's 128-byte multiple.
            let need = pend.slices.iter().try_fold(0usize, |total, slice| {
                total.checked_add(3)?.checked_add(slice.len())
            });
            let need = need.ok_or("bitstream size overflow")?;
            let mut size = 0u32;
            let mut ptr: *mut core::ffi::c_void = std::ptr::null_mut();
            self.video_context
                .GetDecoderBuffer(
                    &session.decoder,
                    D3D11_VIDEO_DECODER_BUFFER_BITSTREAM,
                    &mut size,
                    &mut ptr,
                )
                .map_err(|e| format!("GetDecoderBuffer(bitstream): {e}"))?;
            if ptr.is_null() {
                let _ = self
                    .video_context
                    .ReleaseDecoderBuffer(&session.decoder, D3D11_VIDEO_DECODER_BUFFER_BITSTREAM);
                return Err("GetDecoderBuffer(bitstream) returned a null buffer".into());
            }
            if (size as usize) < need {
                let _ = self
                    .video_context
                    .ReleaseDecoderBuffer(&session.decoder, D3D11_VIDEO_DECODER_BUFFER_BITSTREAM);
                return Err(format!("bitstream buffer too small ({size} < {need})"));
            }
            let base = ptr as *mut u8;
            let mut cursor = 0usize;
            let mut slice_table = Vec::with_capacity(pend.slices.len());
            for s in &pend.slices {
                slice_table.push(SliceShort {
                    bs_nal_unit_data_location: cursor as u32,
                    slice_bytes_in_buffer: (3 + s.len()) as u32,
                    w_bad_slice_chopping: 0,
                });
                std::ptr::copy_nonoverlapping([0u8, 0, 1].as_ptr(), base.add(cursor), 3);
                std::ptr::copy_nonoverlapping(s.as_ptr(), base.add(cursor + 3), s.len());
                cursor += 3 + s.len();
            }
            let pad = ((cursor + 127) & !127).min(size as usize) - cursor;
            std::ptr::write_bytes(base.add(cursor), 0, pad);
            let bitstream_len = cursor + pad;
            self.video_context
                .ReleaseDecoderBuffer(&session.decoder, D3D11_VIDEO_DECODER_BUFFER_BITSTREAM)
                .map_err(|e| format!("ReleaseDecoderBuffer(bitstream): {e}"))?;

            fill_buffer(
                &self.video_context,
                &session.decoder,
                D3D11_VIDEO_DECODER_BUFFER_SLICE_CONTROL,
                std::slice::from_raw_parts(
                    slice_table.as_ptr() as *const u8,
                    slice_table.len() * std::mem::size_of::<SliceShort>(),
                ),
            )?;

            let desc = |kind, len: usize| D3D11_VIDEO_DECODER_BUFFER_DESC {
                BufferType: kind,
                DataSize: len as u32,
                ..Default::default()
            };
            let descs = [
                desc(
                    D3D11_VIDEO_DECODER_BUFFER_PICTURE_PARAMETERS,
                    std::mem::size_of::<PicParamsHevc>(),
                ),
                desc(
                    D3D11_VIDEO_DECODER_BUFFER_INVERSE_QUANTIZATION_MATRIX,
                    std::mem::size_of::<QmatrixHevc>(),
                ),
                desc(D3D11_VIDEO_DECODER_BUFFER_BITSTREAM, bitstream_len),
                desc(
                    D3D11_VIDEO_DECODER_BUFFER_SLICE_CONTROL,
                    slice_table.len() * std::mem::size_of::<SliceShort>(),
                ),
            ];
            self.video_context
                .SubmitDecoderBuffers(&session.decoder, &descs)
                .map_err(|e| format!("SubmitDecoderBuffers: {e}"))?;
            frame.finish()?;
        }

        // --- Readback (display-cropped) --------------------------------------
        let frame = self.read_surface(curr, pend.ts_us)?;

        // --- DPB update -------------------------------------------------------
        let session = self.session.as_mut().ok_or("no session")?;
        if !slnr {
            session.refs.push(DpbEntry { surface: curr, poc });
        }
        Ok(frame)
    }

    /// Copy one decoded surface down to tightly-packed NV12, applying the
    /// SPS conformance-window crop (the coded frame carries MinCb padding
    /// the display never shows).
    fn read_surface(&self, surface: u8, ts_us: u64) -> Result<NvFrame, String> {
        let session = self.session.as_ref().ok_or("no session")?;
        let sps = &session.sps;
        let (cl, cr, ct, cb) = sps.crop;
        let w = (sps.pic_width - cl - cr) as usize;
        let h = (sps.pic_height - ct - cb) as usize;
        let mut nv12 = vec![0u8; w * h * 3 / 2];
        unsafe {
            self.context.CopySubresourceRegion(
                &session.staging,
                0,
                0,
                0,
                0,
                &session.textures,
                surface as u32,
                None,
            );
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context
                .Map(&session.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(|e| format!("Map(staging): {e}"))?;
            let pitch = mapped.RowPitch as usize;
            let base = mapped.pData as *const u8;
            let (cl, ct) = (cl as usize, ct as usize);
            for row in 0..h {
                std::ptr::copy_nonoverlapping(
                    base.add((ct + row) * pitch + cl),
                    nv12.as_mut_ptr().add(row * w),
                    w,
                );
            }
            // Chroma plane starts after the full texture height; its rows
            // halve, its byte columns don't (interleaved U/V).
            let chroma = base.add(pitch * session.tex_h as usize);
            for row in 0..h / 2 {
                std::ptr::copy_nonoverlapping(
                    chroma.add((ct / 2 + row) * pitch + cl),
                    nv12.as_mut_ptr().add(w * h + row * w),
                    w,
                );
            }
            self.context.Unmap(&session.staging, 0);
        }
        Ok(NvFrame {
            width: w as u32,
            height: h as u32,
            nv12,
            ts_us,
        })
    }

    /// Build (or keep) the D3D11 half for `sps`. Identical repeats — every
    /// IDR re-sends the SPS — keep the live decoder and its references;
    /// any real change tears down and rebuilds, clean.
    fn ensure_session(&mut self, sps: &Sps) -> Result<(), String> {
        if self.session.as_ref().is_some_and(|s| s.sps == *sps) {
            return Ok(());
        }
        self.session = None;
        self.slice_counts = SliceCountLearner::default();
        // Texture dims: the MinCb-padded picture rounded to 16 — the
        // alignment the field's D3D11VA stacks allocate (drivers keep any
        // further CTB spill internal).
        let tex_w = (sps.pic_width + 15) & !15;
        let tex_h = (sps.pic_height + 15) & !15;
        // Surface pool: the DPB the stream declares, one being decoded,
        // and headroom for the retention edges.
        let pool = (sps.max_dec_pic_buffering_minus1 + 3).clamp(4, 16);
        unsafe {
            let desc = D3D11_VIDEO_DECODER_DESC {
                Guid: HEVC_VLD_MAIN,
                SampleWidth: tex_w,
                SampleHeight: tex_h,
                OutputFormat: DXGI_FORMAT_NV12,
            };
            let n = self
                .video_device
                .GetVideoDecoderConfigCount(&desc)
                .map_err(|e| format!("GetVideoDecoderConfigCount: {e}"))?;
            let mut config: Option<D3D11_VIDEO_DECODER_CONFIG> = None;
            let mut offered = Vec::new();
            for i in 0..n {
                let mut c: D3D11_VIDEO_DECODER_CONFIG = std::mem::zeroed();
                if self
                    .video_device
                    .GetVideoDecoderConfig(&desc, i, &mut c)
                    .is_ok()
                {
                    offered.push(c.ConfigBitstreamRaw);
                    // HEVC has exactly one slice-control format (the
                    // 10-byte short entry — no HEVC long struct exists in
                    // any SDK), and drivers report it as
                    // ConfigBitstreamRaw = 1: FFmpeg's config scoring
                    // accepts only 1 for HEVC on every vendor, and this
                    // box's NVIDIA driver offers [1, 1] and decodes the
                    // short table byte-exactly. (2 is the H.264-era
                    // "short" tag; taken too, same one format, defensive.)
                    if matches!(c.ConfigBitstreamRaw, 1 | 2) && config.is_none() {
                        config = Some(c);
                    }
                }
            }
            let config = config.ok_or_else(|| {
                format!("no usable HEVC config (driver offered ConfigBitstreamRaw {offered:?})")
            })?;
            let tex_desc = D3D11_TEXTURE2D_DESC {
                Width: tex_w,
                Height: tex_h,
                MipLevels: 1,
                ArraySize: pool,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_DECODER.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut textures: Option<ID3D11Texture2D> = None;
            self.device
                .CreateTexture2D(&tex_desc, None, Some(&mut textures))
                .map_err(|e| format!("CreateTexture2D(decode pool): {e}"))?;
            let textures = textures.ok_or("decode pool texture missing")?;
            let mut views = Vec::with_capacity(pool as usize);
            for slice in 0..pool {
                let view_desc = D3D11_VIDEO_DECODER_OUTPUT_VIEW_DESC {
                    DecodeProfile: HEVC_VLD_MAIN,
                    ViewDimension: D3D11_VDOV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_VIDEO_DECODER_OUTPUT_VIEW_DESC_0 {
                        Texture2D: D3D11_TEX2D_VDOV { ArraySlice: slice },
                    },
                };
                let mut view: Option<ID3D11VideoDecoderOutputView> = None;
                self.video_device
                    .CreateVideoDecoderOutputView(&textures, &view_desc, Some(&mut view))
                    .map_err(|e| format!("CreateVideoDecoderOutputView[{slice}]: {e}"))?;
                views.push(view.ok_or("decoder output view missing")?);
            }
            let staging_desc = D3D11_TEXTURE2D_DESC {
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                ArraySize: 1,
                ..tex_desc
            };
            let mut staging: Option<ID3D11Texture2D> = None;
            self.device
                .CreateTexture2D(&staging_desc, None, Some(&mut staging))
                .map_err(|e| format!("CreateTexture2D(staging): {e}"))?;
            let staging = staging.ok_or("staging texture missing")?;
            let decoder = self
                .video_device
                .CreateVideoDecoder(&desc, &config)
                .map_err(|e| format!("CreateVideoDecoder: {e}"))?;
            tracing::info!(
                "D3D11VA HEVC decoder up: {tex_w}×{tex_h} coded · {}×{} display · {pool} surfaces",
                sps.pic_width - sps.crop.0 - sps.crop.1,
                sps.pic_height - sps.crop.2 - sps.crop.3,
            );
            self.session = Some(Session {
                sps: sps.clone(),
                decoder,
                textures,
                views,
                staging,
                tex_h,
                refs: Vec::new(),
                prev_poc: (0, 0),
                has_prev: false,
            });
        }
        Ok(())
    }
}

/// D3D11VA AV1 decode — **STUB**, the vendor-neutral AV1 receive rung
/// (RDNA/Xe/Ampere+ via [`AV1_VLD_PROFILE0`]). It will reuse this file's
/// `ID3D11VideoDecoder` plumbing (device open, config pick, surface
/// pool, staging readback, picture assembly over the pacer's chunks),
/// but AV1 needs a distinct, larger DXVA transcription — `DXVA_PicParams_
/// AV1` plus tile buffers — and an OBU parser in place of the Annex-B
/// SPS/PPS/slice parser here. Same `decode(au, ts_us) -> Vec<NvFrame>`
/// seam as the HEVC twin. `open` reports the honest not-yet.
pub struct D3d11vaAv1 {
    _priv: (),
}

// SAFETY: will be owned+driven by one route-decode thread like D3d11vaHevc.
unsafe impl Send for D3d11vaAv1 {}

impl D3d11vaAv1 {
    pub fn open() -> Result<Self, String> {
        Err("D3D11VA AV1 decode not yet implemented".into())
    }

    pub fn label(&self) -> &'static str {
        "D3D11VA (AV1, hardware, vendor-neutral) [stub]"
    }

    pub fn decode(&mut self, _au: &[u8], _ts_us: u64) -> Result<Vec<NvFrame>, String> {
        Err("D3D11VA AV1 decode not yet implemented".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lossy first boundary must not become the steady-state completion
    /// count. Two matching successful boundaries activate the fast close;
    /// later short observations never ratchet it down.
    #[test]
    fn slice_count_learning_requires_stable_boundary_evidence() {
        let mut counts = SliceCountLearner::default();
        counts.observe_boundary(7); // potentially short after loss
        assert_eq!(counts.expected(), None);
        counts.observe_boundary(8); // first complete observation
        assert_eq!(counts.expected(), None);
        counts.observe_boundary(8); // independent confirmation
        assert_eq!(counts.expected(), Some(8));
        counts.observe_boundary(5); // later loss cannot shorten completion
        assert_eq!(counts.expected(), Some(8));
        counts.observe_boundary(10);
        assert_eq!(counts.expected(), Some(8));
        counts.observe_boundary(10);
        assert_eq!(counts.expected(), Some(10));
    }

    #[test]
    fn learned_slice_count_rejects_short_boundary_picture() {
        assert!(boundary_picture_is_complete(None, 3));
        assert!(boundary_picture_is_complete(Some(8), 8));
        assert!(boundary_picture_is_complete(Some(8), 9));
        assert!(!boundary_picture_is_complete(Some(8), 7));
    }

    /// The bit alphabet against hand-packed vectors: ue/se values, fixed
    /// reads across byte edges, truncation as error, and
    /// emulation-prevention stripping — the primitives every header parse
    /// leans on. (Binary literals group by syntax element, not nibble —
    /// the grouping is the point.)
    #[allow(clippy::unusual_byte_groupings)]
    #[test]
    fn bitreader_speaks_exp_golomb() {
        // ue: "1"→0 · "010"→1 · "011"→2 · "00100"→3
        let bytes = [0b1_010_011_0, 0b0100_0000];
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.ue().unwrap(), 0);
        assert_eq!(r.ue().unwrap(), 1);
        assert_eq!(r.ue().unwrap(), 2);
        assert_eq!(r.ue().unwrap(), 3);
        // se over ue codes 0..4 → 0, 1, −1, 2, −2
        let bytes = [0b1_010_011_0, 0b0100_0010, 0b1_0000000];
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.se().unwrap(), 0);
        assert_eq!(r.se().unwrap(), 1);
        assert_eq!(r.se().unwrap(), -1);
        assert_eq!(r.se().unwrap(), 2);
        assert_eq!(r.se().unwrap(), -2);
        // u(n) straddling bytes; then the well runs dry.
        let bytes = [0b1010_1100, 0b0111_0001];
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.u(3).unwrap(), 0b101);
        assert_eq!(r.u(7).unwrap(), 0b0110001);
        assert_eq!(r.u(6).unwrap(), 0b110001);
        assert!(r.u(1).is_err());
        // Emulation prevention: 00 00 03 → 00 00; a lone 03 stays.
        assert_eq!(
            rbsp(&[0, 0, 3, 1, 3, 0, 0, 3, 0], 64),
            [0, 0, 1, 3, 0, 0, 0]
        );
        // The cap stops the copy, byte-exact at the limit.
        assert_eq!(rbsp(&[9; 100], 4), [9; 4]);
    }

    /// The RPS parser against the two shapes slice headers use: the
    /// explicit one-negative-ref set every NVENC P slice writes, and an
    /// inter-predicted set (the general path), checked against the spec's
    /// 7.4.8 derivation order. (Literals group by syntax element.)
    #[allow(clippy::unusual_byte_groupings)]
    #[test]
    fn rps_parses_the_ipp_shape() {
        // Explicit: num_neg=1 ("010"), num_pos=0 ("1"),
        // delta_poc_s0_minus1=0 ("1"), used=1 ("1") → 6 bits.
        let bytes = [0b010_1_1_1_00];
        let mut r = BitReader::new(&bytes);
        let (rps, ndp) = parse_st_rps(&mut r, 0, &[]).unwrap();
        assert_eq!(ndp, 0);
        assert_eq!(rps.s0, vec![(-1, true)]);
        assert!(rps.s1.is_empty());
        assert_eq!(r.bit_pos(), 6, "exactly the bits the set spans");

        // Predicted from {-1 used} with deltaRps=-1: spec order puts the
        // projected deltaRps (-1) before the projected ref delta (-2).
        let base = StRps {
            s0: vec![(-1, true)],
            s1: vec![],
        };
        // idx==sets.len() (slice case): inter_pred=1, delta_idx_minus1=0
        // ("1"), sign=1, abs_minus1=0 ("1"), used[0]=1, used[1]=1.
        let bytes = [0b1_1_1_1_1_1_00];
        let mut r = BitReader::new(&bytes);
        let (rps, ndp) = parse_st_rps(&mut r, 1, std::slice::from_ref(&base)).unwrap();
        assert_eq!(ndp, 1, "reference set's NumDeltaPocs");
        assert_eq!(rps.s0, vec![(-1, true), (-2, true)]);
        assert!(rps.s1.is_empty());
    }

    /// The whole Studio·Lossless media plane across VENDOR-NEUTRAL glass:
    /// paint → GPU convert → NVENC HEVC lossless → Annex-B → **D3D11VA**
    /// → NV12, byte-exact against the encoder's input for every frame —
    /// the same claim the NVDEC twin proves, through the interface every
    /// vendor's driver exposes. 1280×**718** on purpose: the odd-ish
    /// height forces an SPS conformance window (codes 1280×720, crops 2
    /// bottom rows), so the readback crop is load-bearing here. A forced
    /// mid-stream IDR exercises the DPB reset. Skips (passing) without
    /// the encode-side hardware.
    #[test]
    fn d3d11va_hevc_lossless_round_trip() {
        let (w, h) = (1280u32, 718u32);
        let (wu, hu) = (w as usize, h as usize);
        let mut gpu = match crate::gpu_pipeline::GpuConvert::new(w, h, w, h) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("SKIP: GPU convert unavailable: {e}");
                return;
            }
        };
        let mut enc =
            match crate::nvenc::NvencH264::open_lossless_hevc_on_device(&gpu.device(), w, h, 60) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("SKIP: NVENC HEVC session unavailable: {e}");
                    return;
                }
            };
        let mut dec = match D3d11vaHevc::open() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("SKIP: D3D11VA unavailable: {e}");
                return;
            }
        };
        let mut doc = vec![0u8; wu * (hu + 300) * 4];
        crate::nvenc::tests_support::paint_document(&mut doc, wu, hu + 300);
        let mut bgra = vec![0u8; wu * hu * 4];
        let tex = gpu.bgra_texture_from(&bgra, w, h).expect("tex");
        let mut references: Vec<Vec<u8>> = Vec::new();
        let (mut decoded, mut exact) = (0u32, 0u32);
        let mut dec_ms: Vec<f32> = Vec::new();
        for i in 0..60u64 {
            let off = (i as usize) * 3;
            bgra.copy_from_slice(&doc[off * wu * 4..][..wu * hu * 4]);
            gpu.update_bgra(&tex, &bgra, w, h);
            let (slot, nv12_tex) = gpu.convert(&tex).expect("convert").expect("slot");
            references.push(unsafe { readback_nv12(&gpu.device(), &nv12_tex, wu, hu) });
            // IDR at 0 and 30: the mid-stream one resets the DPB and
            // re-runs the parameter sets through a live session.
            let out = enc
                .encode_texture(&nv12_tex, i == 0 || i == 30)
                .expect("encode");
            gpu.release(slot);
            for (au, _) in &out.units {
                let t = std::time::Instant::now();
                let frames = dec.decode(au, i * 16_667).expect("decode");
                dec_ms.push(t.elapsed().as_secs_f32() * 1000.0);
                for f in frames {
                    assert_eq!((f.width, f.height), (w, h), "display-cropped geometry");
                    let idx = (f.ts_us / 16_667) as usize;
                    let reference = &references[idx];
                    decoded += 1;
                    if f.nv12 == *reference {
                        exact += 1;
                    } else {
                        // Split the mismatch by plane; a shift probe says
                        // "geometry" (crop bug) vs "fidelity" (not
                        // lossless) — the two failures look identical in
                        // a bare count.
                        let plane = wu * hu;
                        let stat = |a: &[u8], b: &[u8]| {
                            let (mut n, mut max) = (0usize, 0i32);
                            for (x, y) in a.iter().zip(b) {
                                let d = (i32::from(*x) - i32::from(*y)).abs();
                                if d != 0 {
                                    n += 1;
                                    max = max.max(d);
                                }
                            }
                            (n, max)
                        };
                        let (ln, lmax) = stat(&f.nv12[..plane], &reference[..plane]);
                        let (cn, cmax) = stat(&f.nv12[plane..], &reference[plane..]);
                        let (mut best, mut best_at) = (usize::MAX, (0i32, 0i32));
                        for dy in -4i32..=4 {
                            for dx in -4i32..=4 {
                                let mut n = 0usize;
                                for y in (8..hu - 8).step_by(7) {
                                    let sy = (y as i32 + dy) as usize;
                                    for x in 8..wu - 8 {
                                        let sx = (x as i32 + dx) as usize;
                                        if f.nv12[y * wu + x] != reference[sy * wu + sx] {
                                            n += 1;
                                        }
                                    }
                                }
                                if n < best {
                                    (best, best_at) = (n, (dx, dy));
                                }
                            }
                        }
                        panic!(
                            "frame {idx}: luma {ln}/{plane} differ (max Δ{lmax}) · chroma {cn}/{} differ (max Δ{cmax}) · best shift (dx {}, dy {}) leaves {best} sampled diffs",
                            plane / 2,
                            best_at.0,
                            best_at.1,
                        );
                    }
                }
            }
        }
        // The stream's first picture waits for its successor (no learned
        // count yet); everything after closes in its own call, so all 60
        // are out by the loop's end.
        assert_eq!(decoded, 60, "a picture out for every frame in");
        assert_eq!(exact, 60, "every picture byte-exact");
        dec_ms.sort_by(f32::total_cmp);
        let n = dec_ms.len();
        println!(
            "D3D11VA HEVC lossless round-trip: {decoded}/60 byte-exact @1280×718 (crop live) · decode+copy avg {:.2} ms · p95 {:.2} · max {:.2}",
            dec_ms.iter().sum::<f32>() / n as f32,
            dec_ms[(n * 95 / 100).min(n - 1)],
            dec_ms[n - 1],
        );
    }

    /// Chunked delivery, exactly as the wire does it: every AU split at
    /// slice boundaries into pacer-sized pieces fed one at a time — the
    /// assembly path (boundary close, ts close, learned-count close) is
    /// the thing under test. Byte-exactness must survive re-chunking.
    #[test]
    fn d3d11va_survives_pacer_chunking() {
        let (w, h) = (640u32, 360u32);
        let (wu, hu) = (w as usize, h as usize);
        let mut gpu = match crate::gpu_pipeline::GpuConvert::new(w, h, w, h) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("SKIP: GPU convert unavailable: {e}");
                return;
            }
        };
        let mut enc =
            match crate::nvenc::NvencH264::open_lossless_hevc_on_device(&gpu.device(), w, h, 60) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("SKIP: NVENC HEVC session unavailable: {e}");
                    return;
                }
            };
        let mut dec = match D3d11vaHevc::open() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("SKIP: D3D11VA unavailable: {e}");
                return;
            }
        };
        let mut bgra = vec![0u8; wu * hu * 4];
        let tex = gpu.bgra_texture_from(&bgra, w, h).expect("tex");
        let (mut decoded, mut exact) = (0u32, 0u32);
        let mut references: Vec<Vec<u8>> = Vec::new();
        for i in 0..30u64 {
            for (j, v) in bgra.iter_mut().enumerate() {
                *v = ((j as u64).wrapping_add(i * 17) % 249) as u8;
            }
            gpu.update_bgra(&tex, &bgra, w, h);
            let (slot, nv12_tex) = gpu.convert(&tex).expect("convert").expect("slot");
            references.push(unsafe { readback_nv12(&gpu.device(), &nv12_tex, wu, hu) });
            let out = enc
                .encode_texture(&nv12_tex, i.is_multiple_of(10))
                .expect("encode");
            gpu.release(slot);
            for (au, _) in &out.units {
                // The pacer's split: chunks of whole slice NALs, capped
                // small relative to these frames so real chunking happens.
                for chunk in crate::video::split_annexb_paced(au, 4096) {
                    let frames = dec.decode(&au[chunk], i * 16_667).expect("decode");
                    for f in frames {
                        assert_eq!((f.width, f.height), (w, h));
                        let idx = (f.ts_us / 16_667) as usize;
                        decoded += 1;
                        if f.nv12 == references[idx] {
                            exact += 1;
                        }
                    }
                }
            }
        }
        assert!(decoded >= 29, "decoded {decoded}/30 (first may straddle)");
        assert_eq!(decoded, exact, "every decoded picture byte-exact");
        println!("D3D11VA chunked-delivery: {exact}/{decoded} byte-exact across pacer splits");
    }

    /// Read the exact NV12 bytes of a GPU-lane texture back to the CPU —
    /// the encoder's literal input, for byte-exact comparison. (Mirror of
    /// the NVDEC test's helper; test-only.)
    unsafe fn readback_nv12(
        device: &ID3D11Device,
        tex: &ID3D11Texture2D,
        w: usize,
        h: usize,
    ) -> Vec<u8> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        tex.GetDesc(&mut desc);
        desc.Usage = D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        desc.MiscFlags = 0;
        let mut staging = None;
        device
            .CreateTexture2D(&desc, None, Some(&mut staging))
            .expect("staging");
        let staging = staging.unwrap();
        let ctx = device.GetImmediateContext().expect("ctx");
        ctx.CopyResource(&staging, tex);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .expect("map");
        let pitch = mapped.RowPitch as usize;
        let base = mapped.pData as *const u8;
        let mut out = vec![0u8; w * h * 3 / 2];
        for row in 0..h {
            std::ptr::copy_nonoverlapping(base.add(row * pitch), out.as_mut_ptr().add(row * w), w);
        }
        let chroma_base = base.add(pitch * h);
        for row in 0..h / 2 {
            std::ptr::copy_nonoverlapping(
                chroma_base.add(row * pitch),
                out.as_mut_ptr().add(w * h + row * w),
                w,
            );
        }
        ctx.Unmap(&staging, 0);
        out
    }
}
