# AV1 seams — the stubs wired for the AV1 arc

_All backend-only (per the pipeline boundary): AV1 is a codec addition
confined to `node/src` video modules — no protocol/session/GUI change,
because the codec is carried key-to-key by the sniff, not on the wire.
Both field boxes support AV1 now: RTX 5070 (Blackwell — AV1 encode +
decode), Radeon 9060 XT (RDNA4 — AV1 encode via AMF + decode). This doc
is the map: every stub, and what fills it. Wired 2026-07-18; nothing is
implemented — the stubs compile, fail soft, and are dormant until an
encoder emits AV1._

## The one structural gotcha: AV1 has no start codes

H.264/HEVC are Annex-B (00 00 01 start codes). **AV1 is OBUs** with
leb128 sizes — no start codes. That breaks two assumptions the pipeline
made everywhere:

1. **Codec sniff** — `sniff_codec` looked for a start code. It now falls
   through to `sniff_av1_obu` when none is found, which detects a leading
   sequence-header OBU (the AV1 analog of repeated SPS/PPS on a key
   frame). **Done + unit-tested** (`sniff_routes_h264_hevc_and_av1_obu`):
   collision-free — H.264/HEVC always have start codes, so the OBU branch
   only fires on genuinely start-code-less data, and is conservative
   (requires a real seq-header OBU).
2. **Pacer split** — `split_annexb_paced` cuts at slice-NAL boundaries.
   For AV1 it finds none → returns the whole AU as one chunk (safe:
   AV1 rides unpaced, like a paramless HEVC delta). The AV1-aware seam
   is documented in that fn: an `obu_split` cutting at tile-group/frame
   OBU boundaries (walk OBU headers via leb128, group to `max_chunk`).

## The stubs, by file

### Decode

| Seam | File | State | What fills it |
|---|---|---|---|
| `AuCodec::Av1` + OBU sniff | `video_decode.rs` | **done** | — (the routing seam; tested) |
| `Av1Rung` dispatch (NVDEC → D3D11VA) | `video_decode.rs` | stub struct + `Active::Av1` arm | fill the two rung bodies below; `ALLMYSTUFF_AV1_DECODER` dial mirrors HEVC's |
| `NvdecAv1` | `nvdec.rs` | `open`/`decode` return Err | mirror `NvdecHevc` with `CUDA_VIDEO_CODEC_AV1` (=11, named); set the parser `bAnnexb` bit for AV1's temporal-unit framing; film-grain at map time |
| `D3d11vaAv1` | `d3d11va.rs` | `open`/`decode` return Err | reuse the `ID3D11VideoDecoder` plumbing; **transcribe `DXVA_PicParams_AV1` + tile buffers** (the bulk of the work — AV1's DXVA structs are separate + larger than HEVC's) and an OBU parser in place of the SPS/PPS/slice parser; profile `AV1_VLD_PROFILE0` (=GUID, named) |
| Software floor (`dav1d`) | — (new) | not stubbed | a `dav1d` rung for viewers with no AV1 hardware — the AV1 analog of openh264 for H.264. Add a `dav1d` dep behind a feature; wire into `Av1Rung` after the hardware rungs |

### Encode

| Seam | File | State | What fills it |
|---|---|---|---|
| `NV_ENC_CODEC_AV1_GUID` | `nvenc.rs` | const named | `open_av1_on_device` mirroring `open_on_device` with this GUID + `NV_ENC_CONFIG_AV1` (distinct config-union member); `probe_nvenc_av1_lossless` already exists — run it on the 5070 first (AV1 lossless = profile-0 qindex 0) |
| `AMF_VIDEO_ENCODER_AV1` | `amf.rs` | const named | `open_av1_on_device` mirroring the AVC path; AV1 has its own property names (`Av1TargetBitrate` etc., from `components_VideoEncoderAV1.h`) so a distinct config block, same context→component→submit→query flow |
| MF AV1 (optional) | `mediafoundation.rs` | not stubbed | Windows 11 ships an AV1 MFT on some GPUs; low priority — NVENC/AMF cover the field boxes |

### Selection / posture

- The encode ladder (`video.rs run_gpu_lane` `'open:` block) picks the
  rung by posture. AV1 becomes an option there once a rung exists —
  likely gated behind the **capability handshake** (offer AV1 only where
  the viewer decodes it) and/or a Labs feature (`labs.rs` — add a
  `Feature::Av1` and gate on it, backend-only). AV1-lossless could join
  Studio·Lossless as a second lossless codec.

## Order to implement (when the arc opens)

1. **Probe first** on the 5070: `cargo test --release -- --ignored
   probe_nvenc_av1_lossless --nocapture` and `probe_d3d11va` /
   `probe_d3d11_decoder_profiles` (confirms AV1 VLD Profile0 on both
   boxes). Read the verdict before writing structs.
2. **Decode before encode** (so you can validate against reference
   streams): `NvdecAv1` (smallest — reuses the HEVC parser path), then
   `D3d11vaAv1` (the DXVA AV1 transcription is the big unit).
3. **Encode**: `nvenc.rs` AV1 (5070) and `amf.rs` AV1 (9060 XT), each
   with a byte-exact round-trip test against its decode rung (the same
   pattern as `nvdec_hevc_lossless_round_trip` /
   `d3d11va_hevc_lossless_round_trip`).
4. **Pacer** `obu_split` so AV1 keyframes chunk like H.264/HEVC.
5. **Ladder + gate**: wire AV1 selection behind the handshake/Labs.

Nothing above touches `crates/` or the GUI — the sniff carries the codec,
and the boundary holds.
