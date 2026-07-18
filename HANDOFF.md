# AllMyStuff encoder — engineering handoff

_Last updated 2026-07-18. Branch `main` @ `bcd06f8`, tree clean. Fork:
`github.com/nathanfraske/AllMyStuffEncoderFraske` (upstream
`mrjeeves/AllMyStuff`, main dev "Chris"). This is nathanfraske's encoder
line; commit to fork `main`._

## TL;DR for the next agent

The GPU zero-copy encode/decode pipeline is built and shipping. Three
stream postures work end-to-end on an all-NVIDIA pair: **Balanced**,
**Game** (GDR/intra-refresh, latency-first), **Studio** (lossy
high-bitrate), and **Studio · Lossless** (bit-exact HEVC, proven
byte-exact NVENC→NVDEC). A latest field build sits at
`C:\Users\Admin\AppData\Local\Temp\amsgui\release\bundle\AllMyStuff_0.2.46_x64-setup.exe`
(2026-07-18 14:29). Two hardware arcs are staged and waiting on boxes
the user will provide: **AMF** (AMD encode) needs a Radeon (9060 XT
incoming), **AV1** needs a 50-series. The next high-value build item is
a **D3D11VA vendor-neutral decode rung** so HEVC/Studio-LL works on
non-NVIDIA and iGPU viewers.

## How to build & test on this box

- Rust deps need PATH prefixes: `~/.cargo/bin`, the cmake at
  `C:\Users\Admin\AppData\Local\Temp\cmk\cmake-3.31.6-windows-x86_64\bin`.
- Use a SHORT `CARGO_TARGET_DIR` (MAX_PATH): node crate builds under
  `C:\Users\Admin\AppData\Local\Temp\amst`, the GUI/tauri under
  `…\amsgui`. `-j 16` is approved.
- Node crate lives in `node/`. Gate = `cargo fmt` + `cargo clippy
  --release --all-targets -j 16` + `cargo check --no-default-features`
  (the capture-less/stub build — it has broken before, always check it)
  + the suite. **Skip the audio test** (`--skip audio::`): it crashes
  with an access violation on this box (environmental, filed as a
  spawned task, not our regression). Full suite is **157 passed / 0
  failed** minus audio.
- GUI: `cd gui; npm run check` (svelte-check). One pre-existing a11y
  warning is expected; 0 errors is the bar.
- Hardware tests **skip clean** without the relevant silicon (NVENC/
  NVDEC/AMF), printing `SKIP: …`. That's how the cross-vendor arcs stay
  green on any box.
- Full bundle: build `allmystuff-serve` release bin, copy it to
  `gui/src-tauri/binaries/allmystuff-serve-x86_64-pc-windows-msvc.exe`
  (the sidecar), then `npm run tauri build` in `gui/`.
- **Never touch the live `allmystuff-serve` / `myownmesh` processes** on
  this box.
- **Transport rule (Chris, hard constraint):** zero data/pacing changes
  in MyOwnMesh. All pacing is app-side; the daemon's `H264AuAssembler`
  reassembles per RTP marker and its contiguity anchor spans split
  writes. Everything in `send_video_paced` respects this.

## The pipeline, oriented

- **Capture + convert (GPU, vendor-neutral):** `win_capture.rs` DXGI
  duplication → `gpu_pipeline.rs` D3D11 VideoProcessor BGRA→NV12 on the
  capture adapter. Zero CPU pixel work. Works on any D3D11 device incl.
  iGPU. `ClockKeeper` (in `gpu_pipeline.rs`) holds boost clocks while a
  route streams (256×256 copy @2ms — the encode engine alone doesn't
  hold boost; measured 23.7→14.7 ms studio).
- **Encode ladder** (`video.rs run_gpu_lane`): NVENC → AMF (AMD only,
  in progress) → MF (vendor-neutral hardware H.264) → software openh264.
  - `nvenc.rs`: direct NVENC SDK, runtime-loaded `nvEncodeAPI64.dll`, FFI
    hand-transcribed from ffnvcodec n12.0.16.0 (header staged in the
    session scratchpad). Postures: game=P2+ULL+GDR, studio=P5+HQ,
    lossless=P3+constQP0+transform-bypass (H.264 Hi444PP measurement rung
    / HEVC Main shipping rung). Deep DPB (maxNumRefFrames=8) on lossy
    H.264 for ref-invalidation. `ALLMYSTUFF_NVENC_PRESET=1..7` overrides.
  - `amf.rs`: **loader + version probe + vendor gate only** (the vtables
    are the next transcription unit — see arcs below).
  - `mediafoundation.rs`: MF hardware H.264 MFT, adapter-pinned.
- **App-side slice pacer** (`mesh.rs send_video_paced` + `video.rs
  split_annexb_paced`): keyframes leave as byte-capped slices with
  rate-matched inter-chunk gaps (800 Mbps drain model). Speaks both
  codecs (exact param-set byte detection). Lossless IDRs = 32 slices.
- **Decode lane** (`video_decode.rs`): per-route bridge; codec sniffed
  from the AU's first NAL byte (exact bytes 0x40/0x42/0x44/0x26 for
  HEVC — do NOT use masked types, collides with H.264 0x41). H.264 →
  openh264 (universal). **HEVC → NVDEC only** (`nvdec.rs`, nvcuvid, FFI
  from the dynlink headers). NV12→RGBA is a threaded/quad-shared fast
  path (11.5→2.8 ms @1440p). Byte-exact round trip proven.
- **Telemetry** (`telemetry.rs`, started by `serve`): 1 Hz line — CPU
  proc/total, per-thread media CPU, per-engine GPU busy (3d/enc/dec/
  copy), VRAM, monitor topology. Vendor-neutral WDDM counters, so a
  9060 XT log compares directly to this box. `capture bound to
  \\.\DISPLAYn` links a route's stream to the physical panel.
- **Logs:** `%LOCALAPPDATA%\AllMyStuff\logs\allmystuff-serve.log`
  (guaranteed-writable; the install-dir cwd copy is best-effort). Path
  printed to stderr at startup.
- **GUI:** `gui/src/ui/ModePill.svelte` is the shared Mode control
  (Balanced/Game/Studio/Studio·LL + bandwidth warning) used by BOTH the
  console strip (`Console.svelte`) and the popped-out bar
  (`VideoPopout.svelte`) — do not re-fork it. `fsr1.ts` is the WebGL2
  FSR1 upscaler on the popout stage.

## Cross-GPU / inter-vendor matrix (the "inter-GPU probe")

The bitstream is the interop layer — H.264/HEVC are vendor-neutral, so
"inter-GPU" is really two availability questions (host has an encoder
for the posture's codec+features; viewer has a decoder for that codec).

- **Game (H.264+GDR):** works any→any (H.264 decode is universal:
  every GPU/iGPU + software floor). GDR *feature* needs an SDK encoder
  on the host (NVENC yes, AMF pending, MF/QSV no → degrades to plain
  low-latency H.264). iGPU↔GPU↔iGPU all fine.
- **Studio (lossy H.264):** fully cross-vendor today, any combo. Just
  high-bitrate H.264.
- **Studio · Lossless (HEVC):** the only posture with real gaps.
  - Encode: NVENC-only for lossless. Non-NVIDIA hosts auto-degrade to
    lossy Studio (works). AMD/Intel HEVC encode exists but not bit-exact
    lossless.
  - Decode: **NVDEC-only in our code** (`video_decode.rs:259`) though
    HEVC hardware decode exists on ALL vendors' silicon (proven via the
    D3D11 decoder-profile probe — `probe_d3d11_decoder_profiles`). So
    **NVIDIA host → AMD/Intel viewer FAILS at decode today** (no
    software HEVC fallback).
- **Hybrid-laptop trap:** encode pins to the DISPLAY-owning adapter
  (zero-copy). On Optimus where the desktop is on the Intel iGPU, we
  encode via QSV even with an NVIDIA dGPU present → no GDR. Task #12
  territory; surface before laptop field runs.

**The unlocks, in value order:**
1. **D3D11VA decode rung** (`ID3D11VideoDecoder`) — vendor-neutral HEVC
   (and H.264) decode on any GPU/iGPU. Single biggest move: converts
   Studio-LL from NVIDIA-pair to any-Windows-GPU. ~size of the NVDEC
   rung; partly testable on this box. **RECOMMENDED next build item.**
2. **Viewer decode-capability handshake** — the host currently offers
   HEVC-LL based on ITS hardware, blind to the viewer. Advertise viewer
   decode caps in presence/route negotiation so the host only sends HEVC
   where the viewer can decode it (else lossy Studio). Small; makes every
   pairing fail SOFT instead of hard. Good to land before the AMD box.
3. **Cross-vendor HEVC encode** (MF-HEVC / QSV / VCN) for HEVC Studio on
   non-NVIDIA hosts (visually-lossless, not bit-exact).

## Open arcs & tasks (see the in-tool task list #16–#34)

- **#33 AMF rung (AMD encode)** — loader shipped (`f460b13`). NEXT: C-ABI
  vtable transcription from staged headers in the session scratchpad
  `…/scratchpad/amf/` (Factory→Context InitDX11→Component
  SubmitInput/QueryOutput; FFmpeg `amfenc.c` is the flow reference).
  Latency-first config (ULL usage, SPEED preset for game, GDR via
  INTRA_REFRESH_NUM_MBS_PER_SLOT). Add `bench_amf_quality_grid` mirroring
  `bench_nvenc_preset_grid`. AMF has **no lossless** (Studio-LL stays
  NVIDIA-pair). **User provides a Radeon 9060 XT** — make it tight before
  then; e2e + grid run there.
- **#22 ref-invalidation** — MECHANISM landed (`e382997`):
  `invalidate_ref`, deep DPB, `EncodeOutcome.input_ts`, proving test.
  FINDING: driver accepts invalidate & recovery is a P-frame (no IDR),
  BUT strict decoders (openh264/WebCodecs) still error on the frame_num
  gap without a re-entry. So the shipped GDR wave + IDR re-entry stays
  correct; production ring NOT built (would be redundant). Payoff needs a
  viewer that rides the gap (recognizes GDR recovery-point SEI — NVDEC
  does natively) — FIELD-TEST-GATED on the 2-machine lossy link.
  `VideoFeedback.lost_ts_us` already ships to drive it.
- **AV1 arc** — probe committed (`cdea827`,
  `probe_nvenc_av1_lossless`). This box: AV1 decode YES, encode NO
  (Ampere; AV1 encode is Ada+). AV1 lossless is plain profile-0 syntax
  (qindex 0), so any conformant decoder handles it. **User provides a
  50-series box** — run the probe there; if the verdict line reads
  lossless-class bytes, implementation is ~half a day on the existing
  rails (new GUIDs/config struct + parameterize NVDEC codec=11 + OBU-aware
  sniff/pacer branch since AV1 has no Annex-B start codes).
- **#24 decode-side pass** — carries the D3D11VA rung (unlock #1 above)
  and the 4:4:4 tier (red-text-fringe fix).
- **#26 monitor-connect bug** — awaiting a field `allmystuff-serve.log`
  now that telemetry + `capture bound to …` logging lands; the monitor
  topology line + binding will show the cause.
- **Audio uplift** (not yet a task): today Opus mono 96 kbps
  (`audio.rs`). "Studio sound" = stereo end-to-end + 256–320 kbps + a
  better resampler. App-side only, wire-compat (Opus self-describes
  channels). Deferred by the user.
- **#16 Mac**, **#17 MyOwnMesh coord**, **#20 HDR tone-map**, **#27
  RISC-V/MJPEG-floor**, **#28 fallback sweep** — lower priority, see task
  list.

## Awaiting user decision

- Which to build first: the **D3D11VA decode rung** (biggest unlock) or
  the **capability handshake** (fail-soft safety before the AMD box).
- Console-bar unification (#done, `bcd06f8`) — verify on next smoke test
  that the main console Mode control cycles Balanced→Game→Studio→Studio·LL
  with the warning on first Studio entry.

## Commit trail this session (newest first)

`bcd06f8` shared Mode control · `e382997` ref-invalidation mechanism ·
`55cec0e` log location · `3a00a4c` frame health · `becc220`/`5746ee8`/
`fec7168`/`c77c7d0` telemetry · `4fd853e` preset defaults · `7f7084b`
red-team hardening + heartbeat + pacing · `f460b13` AMF loader ·
`cdea827` AV1 probe · `3fe189b` Studio-LL mode · `72c0467`/`3e9c0fd`/
`d895621` HEVC encode+decode.

## Memory

The user's auto-memory at `~/.claude/…/memory/` holds durable context:
`hardware-arcs-status.md` (this matrix), `build-env-this-box.md`,
`project-direction.md`, `transport-signaling-rule.md`. Read `MEMORY.md`
first.
