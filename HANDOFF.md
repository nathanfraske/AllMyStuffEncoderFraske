# AllMyStuff encoder — engineering handoff

_Last updated 2026-07-18 (evening). Branch `main`. Fork:
`github.com/nathanfraske/AllMyStuffEncoderFraske` (upstream
`mrjeeves/AllMyStuff`, main dev "Chris"). This is nathanfraske's encoder
line; commit to fork `main`._

## TL;DR for the next agent

The GPU zero-copy encode/decode pipeline is built and shipping, and
**Studio·Lossless decode is now cross-vendor**: the D3D11VA rung
(`d3d11va.rs`, commit `91c5448`) drives `ID3D11VideoDecoder` on any
Windows GPU — AMD/Intel/iGPU viewers included — and is proven byte-exact
against NVENC on this box (60/60 at 1280×718 with a live conformance
crop; 30/30 through real pacer chunking). The bridge's HEVC arm is a
ladder: NVDEC first where the NVIDIA driver lives (measured faster:
4.24 vs 5.76 ms avg decode+copy at 1440p), else D3D11VA;
`ALLMYSTUFF_HEVC_DECODER=nvdec|d3d11va` pins a rung for demos/A-B.
Four postures work end-to-end: **Balanced**, **Game** (GDR,
latency-first), **Studio** (lossy high-bitrate), **Studio · Lossless**
(bit-exact HEVC). Latest field build:
`C:\Users\Admin\AppData\Local\Temp\amsgui\release\bundle\nsis\AllMyStuff-PROTOTYPE-<hash>_0.2.46_x64-setup.exe`
(2026-07-18 night; the closed WAN loop + OS scheduling honesty + the
D3D11VA rung + popout fullscreen fit). **VERSIONING RULE (user,
2026-07-18): versions are CHRIS'S to cut — the fork's builds are strict
prototypes until his signoff, so every manifest stays at his last
release (0.2.46, the merge-base `78b1c76`) and prototype installers are
distinguished by the PROTOTYPE-<commit> file name, never by claiming
0.2.47+.** (The earlier 0.2.47/0.2.48 bumps were reverted in-history by
a follow-up commit; don't repeat them.) Sidecar staged manually per the
recipe below — build.rs's "sidecar bundle skipped" warning is benign,
the staged copy is what ships; `ALLMYSTUFF_SERVE_BIN=<path>` makes
build.rs stage it itself. `docs/INTEGRATION-REPORT-2026-07.md` is the
hand-to-Chris dossier (diffs, pipeline, blast radius). Two hardware
arcs wait
on boxes the user will provide: **AMF** (AMD encode) needs the Radeon
9060 XT, **AV1** needs a 50-series. Next high-value item: the **viewer
decode-capability handshake** (fail-soft before the AMD box lands).

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
  rate-matched inter-chunk gaps. Drain model is LINK-FITTED as of
  0.2.48: LAN keeps the 800 Mbps shallow-buffer shape; WAN spreads at
  the route's send rate ×1.5 over ≤1 frame interval (`route_pace` →
  `RouteRate` seam). Gaps run deadline-based through
  `os_perf::precise_sleep`. Speaks both codecs. Lossless IDRs = 32
  slices. The closed loop: viewer times the chunk trains
  (`note_video_arrival`) → BWE + delay trend ride `VideoFeedback` →
  `note_feedback` AIMD (`rate_adapt_step`, unit-tested) → encode thread
  applies via NVENC in-place reconfigure. Watch `pace gaps` and
  `video in` minute lines; `video rate` logs every AIMD step.
- **Decode lane** (`video_decode.rs`): per-route bridge; codec sniffed
  from the AU's first NAL byte (exact bytes 0x40/0x42/0x44/0x26 for
  HEVC — do NOT use masked types, collides with H.264 0x41). H.264 →
  openh264 (universal). **HEVC → hardware ladder** (`HevcRung`): NVDEC
  (`nvdec.rs`, nvcuvid) → **D3D11VA** (`d3d11va.rs`,
  `ID3D11VideoDecoder`, vendor-neutral — see below). NV12→RGBA is a
  threaded/quad-shared fast path (11.5→2.8 ms @1440p). Byte-exact round
  trips proven on BOTH rungs.
- **D3D11VA rung** (`d3d11va.rs`): stateless DXVA decode — a scoped
  in-house HEVC header parser (SPS/PPS/slice, POC, RPS, DPB) feeds
  hand-transcribed pack(1) DXVA structs (slice entry is 10 bytes).
  Because the pacer delivers pictures as several samples, the rung
  assembles pictures itself: close on first_slice flag | ts change |
  learned slices-per-picture (a max-only ratchet — loss can't teach a
  short count). Steady state adds zero latency. Config truth learned
  the hard way: HEVC has ONE slice format; drivers report it as
  `ConfigBitstreamRaw=1` (NVIDIA offers [1,1]; FFmpeg ships short-only
  and accepts only 1 for HEVC — there is no HEVC long struct in any
  SDK). Out-of-scope streams (10-bit, scaling lists, LT refs, B
  slices) fail SOFT with a named reason → bridge re-keys.
  `probe_d3d11va_hevc_configs` (ignored test) is the field kit to run
  first on the Radeon box.
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
  FSR1 upscaler on the popout stage. The popout canvas now FILLS the
  window (`width/height:100%` + `object-fit:contain`) so fullscreen
  scales the picture up to the monitor with letterbox bars; `norm`
  computes the object-fit inset (it always did), `presentFsr` pins the
  FSR overlay to the computed content box (dpr-exact), and a
  ResizeObserver re-presents on fullscreen/resize so a static stream
  refits instantly. Console THEATER mode still has the old
  never-upscale behavior — spawned task covers it (`updateCrosshair`/
  `followCursor` map through the raw element rect and need the same
  inset math first; `normPoint` is already inset-safe).

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
- **Studio · Lossless (HEVC):** encode is the remaining gap.
  - Encode: NVENC-only for lossless. Non-NVIDIA hosts auto-degrade to
    lossy Studio (works). AMD/Intel HEVC encode exists but not bit-exact
    lossless.
  - Decode: **SOLVED cross-vendor** — the ladder runs NVDEC on NVIDIA,
    D3D11VA everywhere else, so **NVIDIA host → AMD/Intel/iGPU viewer
    now decodes Studio-LL in hardware**. (Still no software HEVC floor:
    a GPU-less viewer can't take HEVC — the capability handshake should
    keep HEVC off such pairings.)
- **Hybrid-laptop trap:** encode pins to the DISPLAY-owning adapter
  (zero-copy). On Optimus where the desktop is on the Intel iGPU, we
  encode via QSV even with an NVIDIA dGPU present → no GDR. Task #12
  territory; surface before laptop field runs.

**The unlocks, in value order:**
1. ~~**D3D11VA decode rung**~~ — **DONE** (`91c5448`, `d3d11va.rs`).
   Studio-LL decode is any-Windows-GPU; byte-exact proven on this box;
   run the probe + round-trip on the Radeon when it lands.
2. **Viewer decode-capability handshake** — the host still offers
   HEVC-LL based on ITS hardware, blind to the viewer. Advertise viewer
   decode caps in presence/route negotiation so the host only sends HEVC
   where the viewer can decode it (else lossy Studio). Small; makes every
   pairing fail SOFT instead of hard. **RECOMMENDED next build item**
   (the remaining hard-fail: HEVC at a GPU-less/ancient viewer).
3. **Cross-vendor HEVC encode** (MF-HEVC / QSV / VCN) for HEVC Studio on
   non-NVIDIA hosts (visually-lossless, not bit-exact).

## Open arcs & tasks (see the in-tool task list #16–#34)

- **Smoothness/pacing/latency idea bank** —
  `docs/SMOOTHNESS-IDEAS-2026-07.md` (deep-brainstorm pass, 2026-07-18):
  19 graded ideas + the M1–M5 measure-first plan + an explicitly-
  rejected list that encodes the frozen-daemon rule. **LANDED same day
  (`a97678f` + `1a8d188`, in 0.2.48):** T3.2+T3.1 (process power/timer
  opt-outs, `precise_sleep` — 435 µs worst overshoot measured — MMCSS
  opt-in via `ALLMYSTUFF_MMCSS=1`), M2 (`pace gaps` minute line), M3
  (`video in` chunk-train line), T2.1a (WAN pacer drain = route rate
  ×1.5, one-frame budget; `ALLMYSTUFF_PACE_DRAIN_MBPS` A/B dial), T1.1
  (chunk-train BWE + delay trend → serde-default `VideoFeedback`
  fields), T1.2 (AIMD bitrate on the in-place reconfigure — **RESERVED
  to Game by default** (`d100022`): the user's rule is that automatic
  bitrate changers run only where they're beneficial in every case for
  the mode's use case, and only Game qualifies; Balanced/Studio keep
  the picked quality unless `ALLMYSTUFF_RATE_ADAPT=1` opts all lossy
  postures in; `=0` kills; never lossless/pinned), T2.7 (loss-aware
  wave length: 2 losses in 10 s → 3-frame heal). The Game-mode
  "transport is open-loop" caveat is retired. Field logs carry
  **per-layer bandwidth**: sender `raw → wire` Mbps on the `video out`
  line, viewer `wire → nv12 → rgba` on the `video decode` line, plus
  the `pace gaps` and `video in` (chunk-train) minute lines — end to
  end, the log names where every megabit goes. **Field validation
  pending on the 2-machine rig:** est accuracy vs `clumsy`-imposed
  rates, freeze-seconds A/B, gap-fidelity before/after — the new log
  lines are the kit. **Still open from the bank:** T1.4 LTR recovery +
  T1.3 sample parity + T2.3 gap-NACK (all need M5's loss
  characterization on the rig first), T2.2 sub-frame slices, T2.6
  paint pacing, T2.8 zero-copy present, T2.5 damage-driven QP.

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
- **#24 decode-side pass** — D3D11VA rung SHIPPED (`91c5448`); the
  4:4:4 tier (red-text-fringe fix) remains on this task.
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

- Green-light the **capability handshake** (unlock #2) as the next build.
- Console-bar unification (`bcd06f8`) — verify on next smoke test
  that the main console Mode control cycles Balanced→Game→Studio→Studio·LL
  with the warning on first Studio entry.

## Commit trail (newest first)

`d100022` **auto-bitrate reserved to Game + per-layer bandwidth logs** ·
`1a8d188` **closed WAN loop** (link-fitted pacer, BWE, AIMD, shaped
waves, M2/M3 lines) · `a97678f` **OS scheduling honesty** ·
`1a437f1` idea bank · `97803f7` 0.2.47 · `be32d3c` popout fullscreen
fit · `91c5448` **D3D11VA decode rung** · `9341cf8` handoff ·
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
