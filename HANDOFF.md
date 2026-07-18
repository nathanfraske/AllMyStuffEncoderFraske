# AllMyStuff encoder line — what we did, exactly

_Fork `nathanfraske/AllMyStuffEncoderFraske` (branch `main`) against
upstream `mrjeeves/AllMyStuff` at merge-base `78b1c76`
(`chore(release): 0.2.46`). Written for Chris's review AND as the next
agent's handoff. Last updated 2026-07-18 (late night). **Everything is a
strict prototype until Chris's signoff; versioning is his to cut — all
manifests sit at his 0.2.46 and prototype installers are named
`AllMyStuff-PROTOTYPE-<commit>_0.2.46_x64-setup.exe`.**_

**The one-line charter:** Chris owns the software architecture; this
fork owns the computer-engineering layer — silicon interfaces, kernels,
scheduling, transport-shaping — for encoder/decoder speed and
smoothness. Zero MyOwnMesh changes, zero signaling-path changes, ever.

**The review set:** this file (narrative) ·
[`docs/INTEGRATION-REPORT-2026-07.md`](docs/INTEGRATION-REPORT-2026-07.md)
(blast radius, wire compat, per-file rings, review order) ·
[`docs/TESTER-KIT-2026-07.md`](docs/TESTER-KIT-2026-07.md) (how to
verify every claim + the PR draft + security-push shortlist) ·
[`docs/SMOOTHNESS-IDEAS-2026-07.md`](docs/SMOOTHNESS-IDEAS-2026-07.md) +
[`docs/EXPERIMENTAL-ARC-PLAN-2026-07.md`](docs/EXPERIMENTAL-ARC-PLAN-2026-07.md)
(what's next, gated) ·
[`docs/ENCODER-PASS-2026-07.md`](docs/ENCODER-PASS-2026-07.md) (the
first pass's before/after profiles). Fork total: **53+ commits,
41 files, ≈+15.7k/−0.9k** — each commit message carries its full
reasoning; the trail at the bottom is the index.

## What we did — the complete list, arc by arc

1. **Encoder correctness + latency pass** (`b3b40c4`…`9bf7485`):
   lossless pump fixes, healing-ladder hardening, refinement passes,
   capture/convert ∥ encode threading, buffer-reuse lanes (A/B
   adjudicated), DXGI re-promotion, pipe-write timeout. Report:
   `docs/ENCODER-PASS-2026-07.md`.
2. **GPU zero-copy lane** (`bb53784`, `b7aa331`): DXGI duplication →
   D3D11 VideoProcessor BGRA→NV12 on one device → texture-fed encoder
   via `IMFDXGIDeviceManager`. Zero CPU pixel touches; fails soft into
   the untouched CPU pipeline. `ClockKeeper` boost-clock heartbeat
   (measured 23.7→14.7 ms studio frames). NV12 ring with depth-2
   retirement (field tearing fix, `47be07f`).
3. **Direct NVENC SDK rung** (`176c47b`, default-on `3aa2c2d`):
   runtime-loaded `nvEncodeAPI64.dll`, FFI hand-transcribed from MIT
   ffnvcodec headers, size-asserted; texture-native; MF rung as in-lane
   fallback; presets measured not guessed (`4fd853e`: game P2,
   lossless P3, studio P5; P6/P7 rejected for hidden lookahead).
4. **Four postures, end to end** (`b78f103`…`3fe189b`): Balanced ·
   Game (GDR intra-refresh — zero IDR walls, recovery-point SEI,
   single-frame VBV, 1 ms pacing quanta, 50 ms quiet wake) · Studio
   (uncapped-by-mode quality-first) · **Studio·Lossless** (HEVC Main
   constQP-0 + transquant bypass — bit-exact, proven byte-equal
   end-to-end in hardware). Shared Mode control in the GUI (`bcd06f8`),
   FSR1 viewer upscaler (`59e9197`), fullscreen pointer lock
   (`91b8b7b`), popout fullscreen fit (`be32d3c`).
5. **Hardware decode, cross-vendor** (`3e9c0fd`, `72c0467`,
   `91c5448`): NVDEC rung (nvcuvid) byte-exact against NVENC; then the
   **D3D11VA rung** — vendor-neutral `ID3D11VideoDecoder` with an
   in-house scoped HEVC parser (SPS/PPS/slice, POC, RPS, DPB;
   defensive on remote input; picture assembly over the pacer's
   chunked delivery) — byte-exact 60/60 incl. a live conformance-window
   crop and 30/30 through real chunking. Ladder: NVDEC (4.24 ms) →
   D3D11VA (5.4–5.8 ms) at 1440p, measured. NVIDIA host → AMD/Intel
   viewer now decodes Studio·LL in hardware.
6. **App-side slice pacer + closed WAN loop** (`32d94a1`, `7f7084b`,
   `1a8d188`): AUs leave as slice-bounded chunks with rate-matched
   gaps; drain model LINK-FITTED (LAN keeps the 800 Mbps
   shallow-buffer shape; WAN spreads at the route's send rate ×1.5
   over ≤1 frame interval); gaps execute against deadlines via
   `precise_sleep`. The loop: viewer times the pacer's own chunk
   trains → bandwidth estimate + one-way-delay trend ride the existing
   `VideoFeedback` (additive serde-default fields, CHANNEL_CONTROL on
   the ICE datapath) → AIMD bitrate applied through NVENC's in-place
   reconfigure (no reset, no IDR). **Reserved to Game posture by
   default** (`d100022`) per the rule: automatic rate changers only
   where beneficial in every case for the mode; Balanced/Studio keep
   the picked quality (`ALLMYSTUFF_RATE_ADAPT=1` opts all in, `=0`
   kills). Loss-aware GDR wave length (2 losses in 10 s → 3-frame
   heal). Frame-health loss naming + wave heal (`3a00a4c`);
   ref-invalidation mechanism built and scoped (`e382997`).
7. **OS scheduling honesty** (`a97678f`): Win11 process-wide
   power-throttling + timer-resolution-ignore opt-outs (the sidecar is
   exactly the windowless shape Win11 quietly degrades);
   `precise_sleep` = high-res waitable timer + bounded spin (435–635 µs
   worst overshoot measured vs ms-class rounding); MMCSS Games class
   opt-in (`ALLMYSTUFF_MMCSS=1`); WDDM GPU scheduling class High opt-in
   (`ALLMYSTUFF_GPU_SCHED=1`, `c370d21`).
8. **CPU kernels at the memory wall** (`b992776`): NV12→RGBA viewer
   convert — AVX2 lane with scalar-identical integer math + non-temporal
   stores (RFO eliminated), **3.5 → 1.8 ms avg @1440p** (p95 4.5→2.2),
   byte-exactness pinned by test; pacer's Annex-B walk
   memchr-anchored (provably equivalent). Already-O(n) honesty: the
   remaining algorithmic wins are pass deletion (T2.8) and n-shrinking
   (T2.9 damage grouping — the user's idea, planned).
9. **Observability that names where every millisecond and megabit
   goes** (`fec7168`…`becc220`, `1a8d188`, `d100022`, `c370d21`):
   1 Hz telemetry (CPU proc/thread, per-engine GPU busy, VRAM, monitor
   topology); per-layer bandwidth on the standard log — sender
   `raw → wire` Mbps + M1 capture-age span (compositor present →
   encode start, QPC-mapped), `pace gaps` requested-vs-actual +
   daemon-write split, viewer chunk-train dispersion + estimate,
   decoder `wire → nv12 → rgba`; `video rate` logs every AIMD step;
   guaranteed-writable log at
   `%LOCALAPPDATA%\AllMyStuff\logs\allmystuff-serve.log` (`55cec0e`).
10. **Process discipline** (`86615bd`, `f5f1f21`, docs): versioning
    held at Chris's 0.2.46 (his to cut; earlier same-day bumps
    reverted); integration report; tester kit; idea bank with an
    explicitly-rejected list that encodes the frozen-daemon rule; the
    Experimental arc PLANNED but nothing experimental on by default.
11. **Staged for hardware the user will provide:** AMF rung (loader/
    probe/vendor-gate, `f460b13`) awaits the Radeon 9060 XT; AV1
    lossless probe (`cdea827`) awaits a 50-series; run
    `probe_d3d11va_hevc_configs` first on any new box.

## What we did NOT touch — the guarantees

- **MyOwnMesh: zero changes.** The bundled daemon is the pinned v0.3.1,
  byte-identical (`.myownmesh-rev` diff vs merge-base is empty).
- **Signaling: zero bytes added.** All media rides the existing track
  lanes; all control rides the existing CHANNEL_CONTROL messages with
  additive `#[serde(default)]` fields; old↔new peers degrade to
  today's behavior in all four pairings (session-crate tests pin both
  directions).
- **RTP/packetization/FEC:** untouched (daemon's). The pacer only
  shapes what bytes we hand it and when, verified against
  `H264AuAssembler`'s per-marker semantics.
- **Chris's CPU pipeline:** intact and load-bearing as every lane's
  soft-fallback; MJPEG floor behaviorally unchanged; resolution
  auto-scale still opt-in; Balanced remains the default posture.

## How to build & test on this box (next-agent operational notes)

- PATH prefixes: `~/.cargo/bin` + cmake at
  `C:\Users\Admin\AppData\Local\Temp\cmk\cmake-3.31.6-windows-x86_64\bin`.
- SHORT `CARGO_TARGET_DIR` (MAX_PATH): node → `…\Temp\amst`, GUI →
  `…\Temp\amsgui`, root workspace → `…\Temp\amsr`. `-j 16` approved.
- Gate: `cargo fmt` · `cargo clippy --release --all-targets` (43
  pre-existing warnings are the accepted baseline — add ZERO) ·
  `cargo check --no-default-features` · `cargo test --release --
  --skip audio::` (**166/0** expected; audio crash is environmental) ·
  `cd gui; npm run check` (0 errors, 1 known a11y warning) ·
  root `cargo test --workspace` (~140).
- Hardware tests skip clean without silicon (`SKIP: …` = passing).
- Bundle recipe: build serve release → copy to
  `gui/src-tauri/binaries/allmystuff-serve-x86_64-pc-windows-msvc.exe`
  (build.rs's "sidecar bundle skipped" warning is benign — the staged
  copy ships; or set `ALLMYSTUFF_SERVE_BIN`) → `npm run tauri build` →
  **rename the exe to `AllMyStuff-PROTOTYPE-<short-hash>_0.2.46…`**
  (never claim versions).
- **Never touch the live `allmystuff-serve`/`myownmesh` processes.**
- Memory files at `~/.claude/...projects.../memory/` — read `MEMORY.md`
  first (`versioning-is-chriss`, `transport-signaling-rule`,
  `build-env-this-box`, `project-direction`, `hardware-arcs-status`).

## Open arcs (value order)

1. **Upstream PR** — the kit's §5 has the draft; the user opens it.
2. **Viewer decode-capability handshake** — the remaining hard-fail
   (HEVC at a GPU-less viewer); small, fail-soft, pre-Radeon.
3. **Experimental arc** — per
   `docs/EXPERIMENTAL-ARC-PLAN-2026-07.md`: T2.9 damage grouping,
   T2.2 sub-frame slices, T2.8 zero-copy present, T2.5 damage-QP,
   T2.6 paint pacing, T2.4 rescue layer, T1.4 LTR (+T2.3) — all
   behind `ALLMYSTUFF_EXPERIMENTAL`, nothing on by default.
4. **2-machine field validation** of the closed loop: `pace gaps`,
   `video in`, `video rate`, `age` lines are the kit; `clumsy` ladder
   for BWE accuracy + freeze-seconds A/B. Also the standing
   monitor-refuses-to-connect capture (#26) and console-theater
   fullscreen fit (spawned task).
5. **Hardware arcs on arrival:** Radeon 9060 XT (AMF vtables + e2e;
   D3D11VA probe first) · 50-series (AV1 probe → ~half-day impl).
6. Lower: 4:4:4 tier (#24 remainder), audio uplift (deferred by user),
   Mac/Linux compile checks, RISC-V floor (#27).

## Commit trail (newest first)

`c370d21` M1 spans + GPU sched class · `50b7963` T2.9 idea (user's) ·
`b992776` **CPU kernels (AVX2+NT, 3.5→1.8 ms)** · `f5f1f21`
integration report · `86615bd` **versioning held (Chris's)** ·
`d100022` **auto-bitrate reserved to Game + layer logs** · `1a8d188`
**closed WAN loop** · `a97678f` **OS honesty** · `1a437f1` idea bank ·
`97803f7` (reverted bump) · `be32d3c` fullscreen fit · `91c5448`
**D3D11VA rung** · `9341cf8` handoff · `bcd06f8` Mode control ·
`e382997` ref-invalidation · `55cec0e` log location · `3a00a4c` frame
health · `becc220`/`5746ee8`/`fec7168`/`c77c7d0` telemetry · `4fd853e`
presets · `7f7084b` hardening+pacing · `f460b13` AMF loader ·
`cdea827` AV1 probe · `3fe189b` Studio·LL mode · `72c0467`/`3e9c0fd`/
`d895621` HEVC encode+decode · `2e90097`…`ecff5fa` lossless rung +
benches · `59e9197` FSR1 · `83e2dcd`/`7b50607`/`b78f103` Studio arc ·
`91b8b7b` pointer lock · `2db15bd` game kernel · `01ba54c` mojibake ·
`3aa2c2d` NVENC default-on · `e45bf09` transport pin · `47be07f` v2
field round · `d46d06b`/`1cba76a` benches · `32d94a1` **pacer** ·
`7c56cdf` CI · `176c47b` **NVENC rung** · `19e6429` MF bitrate ·
`b7aa331`/`bb53784` **GPU lane** · `9bf7485`…`b3b40c4` encoder pass.
(+ this docs/kit commit.)
