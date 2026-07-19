# AllMyStuff Encoder Pass — Consolidated Report (2026-07-17)

Optimization + fix pass on the encoder and remote-control path. All changes are
in the working tree (uncommitted), gated green: **152/152 node tests + 12/12
pixels tests, clippy `-D warnings`, rustfmt**. Benchmarked before/after on the
dev box (20-core hybrid Intel, NVIDIA GPU, 2560×1440, "NVIDIA H.264 Encoder
MFT" = NVENC).

---

## 1. The two reported symptoms → root causes → fixes

### "Freezes / can crash when GPU or CPU is under high utilization"
| Root cause | Fix |
|---|---|
| MF async pump parked the capture thread up to **1 s per frame** waiting for the just-fed frame's output | Lossless event-drain pump: feed on credit/NeedInput, drain everything available, **50 ms bounded grace**; late output rides the next call (`mediafoundation.rs`) |
| The pump **discarded** `METransformNeedInput` events → input pipeline starved permanently (found & fixed during the rewrite; the old code survived only by returning early) | Input-credit bank: every NeedInput is either fed or banked, never dropped |
| One encoder error (TDR, driver update) permanently downgraded DXGI→GDI screenshots feeding a **dead encoder** — frozen viewer + pegged CPU forever | `HealingEncoder`: bounded, spaced ladder rebuilds (3 per 120 s window, 2 s spacing) → MJPEG floor → clean route end with in-band status (`video.rs`) |
| `panic = "abort"` + unguarded slicing: a short capture buffer aborts the whole node | Length guards at the encode boundary and rotator → stream `Err` the healer absorbs |
| Media threads schedulable onto downclocked E-cores / EcoQoS-tagged | `os_perf::boost_media_thread()`: ABOVE_NORMAL + EcoQoS opt-out + P-core CPU-set preference (soft, not hard affinity); 1 ms timer-resolution guard per stream |

### "Window switch tears + classic low-bitrate transition, super slow"
| Root cause | Fix |
|---|---|
| Backlogged pump **overwrote all but the newest access unit** → dropped reference frames → decoder smear until next IDR (2–8 s) | `EncodeOutcome { units: Vec, consumed }` seam: **every** drained unit reaches the wire in order; RTP durations split across a drained backlog |
| Frames the stalled encoder never accepted were marked "sent" → static-skip suppressed the retry; refresh asks eaten | `consumed` flag: prev/last_sent only update on acceptance; unserved refresh asks re-arm |
| After a burst, damage-driven capture goes quiet → one VBV-starved IDR, then **nothing ever sharpens** the picture | Post-quiesce refinement: convergence IDR + **2 spaced non-IDR re-encodes** (+350 ms, +800 ms) so rate control spends idle bandwidth sharpening what the viewer is reading (H.264 only; tests assert the IDR/non-IDR pattern) |
| VideoToolbox variant: a pending unit made `encode_i420` **skip encoding the input frame** + 100 ms hard waits | Same lossless-drain shape on VT; vestigial `pending` queue removed |

---

## 2. Everything shipped (by phase)

1. **P1 — MF pump rewrite** (`mediafoundation.rs`, `video.rs`, `videotoolbox.rs`, `hwenc.rs`): `EncodeOutcome` seam, input credits, 50 ms grace, per-unit hwenc drain. HW test `hardware_pump_is_lossless_and_decodable` proves conservation + decode-validity on real NVENC.
2. **P2 — Encoder healing** (`video.rs`): `RebuildPolicy` + `HealingEncoder`; grab-vs-encode error separation in the oneshot loop; viewer told why on give-up.
3. **P3 — Post-quiesce refinement** (`video.rs`): REFINE_PASSES=2, +350 ms / +800 ms.
4. **P4 — win_capture CPU pass** (`win_capture.rs`, `pixels`): persistent cursor-free desktop buffer (kills the per-frame full-frame clone + zeroed alloc), `ReleaseFrame` before `Map` (duplication no longer throttled by CPU readback), swizzle moved to the O3 pixels crate (`bgra_to_rgba_into`).
5. **P5 — NV12-direct + fast paths** (`pixels`, `video.rs`, `mediafoundation.rs`): `scale_rgba_to_nv12`, codec-declared `YuvFormat`, encoder-side interleave deleted, no-scale contiguous fast path for both layouts (equality-tested vs the scaled path).
6. **P6 — Scheduling** (`os_perf.rs`, wired in `video.rs`/`win_capture.rs`/`input_inject.rs`): timer guard, priority, EcoQoS opt-out, hybrid P-core preference (verified by test on this box).
7. **P7 — Remote control** (`input_inject.rs`, `ownership.rs`, `mesh.rs`): ScreenMap 2 s TTL (fixes mouse landing wrong after a resolution/DPI change — the primary-screen path never refreshed before), allocation-free fleet-membership probe on the per-event gate.
8. **P8 — Hardening**: short-buffer guards (no more panic-aborts from bad frames), duplicate-Tune no-op (no capture restart for same dials).
9. **P9 — Adapter pin** (`mediafoundation.rs`): `ALLMYSTUFF_VIDEO_ENCODE_ADAPTER=intel|nvidia|amd|<index>` via `MFTEnum2` + `MFT_ENUM_ADAPTER_LUID`, DXGI LUID resolution, soft fallback; full-enumeration debug logging; `MF_MT_DEFAULT_STRIDE` guard for Intel.
10. **Bench harness** (test-only, `--ignored bench_`): pipeline decomposition on the real screen, MF call-latency histogram + units conservation, swizzle/clone/convert/rotate micro-benches, sleep-granularity probe (guarded/unguarded).

New env dials: `ALLMYSTUFF_VIDEO_ENCODE_ADAPTER`. All existing dials unchanged.

---

## 3. Profiling: before → after

Pipeline decomposition (live desktop, 1440p, NVENC, release):

| Stage | Before | After | Δ |
|---|---|---|---|
| scale+convert | 9.99 ms (p95 11.8) | 8.12 ms (p95 9.8) | −19% |
| encode call | 11.18 ms (p95 14.9 / max 19.4) | 9.60 ms (p95 13.7 / max 16.4) | −14% |
| busy total | 21.2 ms | 17.7 ms | −16% |
| **observed arrival** | **34.6 fps** | **40.8 fps** | **+18%** |

Standalone / micro:

| Item | Before | After |
|---|---|---|
| MF encode call 1440p avg/p95/max | 8.35 / 10.20 / 12.25 ms | 7.02 / 8.56 / 9.29 ms |
| worst-case stall under GPU load | 1000 ms + unit loss | 50 ms, lossless |
| BGRA→RGBA swizzle 4K | 15.46 ms (−Os) | 10.00 ms (O3, −36%) |
| cursor-path full-frame clone 4K | 8.76 ms **per frame** | eliminated |
| RGBA→4:2:0 4K native | 21.17 ms (+ hidden interleave) | 17.40–17.70 ms NV12-direct |
| units conservation | lost under load | 150/150 by construction + tested |

**The 60 fps math:** convert-side ≈ 14.3 ms and encode ≈ 9.6 ms at 1440p share
one thread → ~42 fps ceiling. Split them (roadmap #1) → max(14.3, 9.6) ≈
**~70 fps**. 4K stays CPU-bound (~32 ms) → 4K60 needs the zero-copy GPU path.
Timer resolution on this box is already ~1 ms (a browser holds it); the guard
is insurance for headless hosts. `rotate_rgba` (rotated monitors only) is
58 ms/frame at 4K — tiling candidate, untouched.

---

## 4. Research syntheses (5 Opus agents; full reports in session transcript)

### QuickSync
Intel's H.264 MFT is **already enumerated** on hybrid boxes — it just never wins
the "first MFT that emits a frame" race against NVENC. Encoder input being
system-memory NV12 means **cross-adapter encode is free today** → the adapter
pin (shipped) is the "encode on the idle iGPU while the dGPU games" lever.
Gotchas: Intel wants explicit stride (guard shipped; validate on Intel
silicon), and Intel's MFT **ignores runtime ICodecAPI** (mid-session
force-keyframe/bitrate) — refresh asks may wait for the 4 s GOP backstop on
Intel; future ABR must rebuild (the healing machinery) instead of live-set.
oneVPL native backend: not now (thin Rust bindings, Intel-only) — "native SDK
later" track. Linux `h264_qsv` misses `scenario=displayremoting`,
`async_depth=1`, `b_strategy=0` (S follow-up; QSV sits behind VA-API in the
ladder anyway).

### AV1 / HEVC
**No vendor ships an AV1 encode MFT** — hardware AV1 on Windows is native-SDK
only (NVENC SDK / AMF / oneVPL). **HEVC encode MFTs ship with every vendor's
driver** → HEVC is a near-mechanical ladder extension, and Chromium/WebView2
has had default-on `hvc1` hardware decode since 2022 — but fleet-wide it's
gated on the *other* viewers (openh264 floor is H.264-only; Safari WebCodecs
HEVC only reliable from Safari 26). Wire: the Rust `rtp` crate has AV1
packetization since 0.17.1 (2026-06); MyOwnMesh's webrtc-rs pin predates it —
bump + round-trip spike (external repo). Software AV1 floor: **don't** (SVT-AV1
realtime tops out ~1080p; rav1e not realtime). AV1's payoff is WAN
(screen-content tools ≈ halve bits vs H.264); minimal at 80 Mbps LAN.

### Multi-encoder GPUs
Per-route architecture is already engine-parallel (verified: independent MFT
sessions, no shared locks; NVIDIA driver auto-balances sessions across
engines). Engine counts: 4090=2, 5070 Ti/5080=2, 5090=3, most consumer=1;
GeForce cap = 8 sessions system-wide (2024+) — ladder soft-fails correctly at
the cap. AMD dual-VCN does **not** auto-balance (verified Linux; Windows
undocumented) — test if AMD hosts matter. Split-frame encoding: HEVC/AV1 +
native SDK only; single-giant-stream tool, not our shape. Future: latency-spike
→ rebuild-on-other-adapter failover (both halves now exist; wiring is M).

### Stream upscaling (DLSS/FSR/XeSS)
Motion-vector upscalers (DLSS, FSR2+, XeSS-SR, DirectSR) **don't apply** to
decoded video. Applicable: **NIS** (single-pass, official GLSL, gentlest on
text — recommended default), **FSR1** EASU (+ low/off RCAS — sharpening
amplifies H.264 ringing on text), CAS. The pipeline is already
"stream-reduced, stretch-on-display" — today's stretch is Chromium's bilinear
via CSS on a 2D canvas. **Recommendation ①: WebGL2 NIS/FSR1 pass** at the
canvas paint sites (Console/VideoPopout/RoomTile), client-only toggle +
sharpness (never rides Tune → no IDR hiccup), <2 ms/frame on any GPU.
RTX/Intel VSR only engages on hardware `<video>` overlays — a
future-native-viewer play (Moonlight precedent: <1 ms via D3D11
VideoProcessor). Carry `source_width/height` in the spare H.264 IPC header
slots so the upscaler can clamp at native. Never upscale before encode or
before IPC.

### Apple platforms
Today's Mac path pays **three redundant CPU passes** on frames born as GPU
IOSurfaces (xcap BGRA→RGBA swizzle → RGBA→I420 → CVPixelBuffer plane-copy).
Do-soon (independent, S/S–M): **VT low-latency mode**
(`EnableLowLatencyRateControl` at session create) + **`DataRateLimits`** (the
VBV twin) + leave `MaximizePowerEfficiency` off; **QoS classes**
(`QOS_CLASS_USER_INTERACTIVE` via pthread — the only P/E steering on Apple
silicon; `os_perf` is currently a no-op there); **App Nap guard**
(`NSProcessInfo.beginActivity(.latencyCritical)`) — a backgrounded host can be
silently throttled. The pivot: **ScreenCaptureKit** (M) — xcap rides
AVCaptureScreenInput (deprecated macOS 13; Sequoia 15.1+ *flags* legacy-capture
apps), SCK adds dirty rects + free GPU Retina downscale + emits NV12/BGRA
IOSurfaces → unlocks the Mac zero-copy epic (L) that turns Retina Macs from
slowest to fastest encoders. Unknown-unknowns: Sequoia's **monthly**
screen-recording re-consent (undisableable; escape = undocumented
`persistent-content-capture` entitlement, needs real signing — current build is
ad-hoc); headless `serve` can't obtain the TCC grant without a GUI-session
bootstrap; clamshell hosting needs a virtual-display story; WKWebView rAF is
hard-capped at 60 Hz (no 120 fps iPad promise without a native viewer); iOS
should adopt WebCodecs HW decode (16.4+) behind the existing decode seam.
Doc drift: ARCHITECTURE.md still says macOS encode = FFmpeg hwenc; shipped
path is native videotoolbox.rs.

---

## 5. Game Mode design

**Phase 1 — a preset over existing machinery (no new pipeline):**
1. fps floor 60 (auto on LAN; explicit off-LAN until BWE — the one caveat).
2. Resolution strategy: pin native, or **encode 1440p + viewer NIS upscale**
   (same perceived sharpness, ~half the bits, faster loss recovery).
3. Latency-biased rate control: VBV 1 s → ~0.5 s, peak 2× → ~1.5× (halves
   burst-queueing; refinement passes cover the transition-quality cost).
4. Relaxed IDR cadence (refresh-on-demand is now reliable) → fewer bursts.
5. Scheduling boost (always-on now) + adapter pin (iGPU carries the stream).
6. Auto-adapt off, refinement on (already defaults).

**Phase 2 — the architecture it wants (= the priority list anyway):**
capture/encode thread split (#1), MyOwnMesh pacer + BWE + in-place
`set_bitrate` (#6/#7), unreliable input lane (#8), zero-copy (#10), and
optionally NVENC-SDK intra-refresh (GDR) for burst-free WAN keyframing.

---

## 6. Priority plan

- **P0 (done)** — this pass. Validate live: `ALLMYSTUFF_VIDEO_STATS=1`, watch
  `encode ms`/`dropped`/fps during window switches and under game load.
- **P1 — capture/encode thread split** (days): measured → ~70 fps at 1440p.
  The single biggest remaining smoothness item; also cuts capture-to-wire
  latency.
- **P2 — zero-copy GPU path** (weeks): D3D11 texture → VideoProcessor NV12 →
  MFT via `IMFDXGIDeviceManager` (no vendor SDK needed). Unlocks 4K60. Decide
  the adapter-affinity policy (conflicts with iGPU offload) first. Mac twin:
  SCK → VT zero-copy (bigger win, cheaper on unified memory).
- **P3 — transport** (MyOwnMesh): pacer, BWE + in-place bitrate, input lane.
  Mandatory for WAN gaming; LAN fine without.
- **P4 — codec/quality**: HEVC rung (cheap, gated on fleet decode), viewer
  upscale pass, AV1 behind the SDK epic.
- **NVENC SDK positioning**: not needed for LAN 1440p60/4K60 H.264 (P1+P2 get
  there vendor-neutrally). It IS the gate for AV1, intra-refresh/GDR,
  split-frame, in-place reconfigure. Adopt when WAN gaming becomes first-class;
  intra-refresh is the best pilot feature.

## 7. Follow-ups / known gaps

- **macOS + `--features hwenc` compile check** — videotoolbox.rs/hwenc.rs edits
  were compile-blind (Windows box); mechanical, but verify on a Mac/Linux.
- **Intel-silicon validation** of the stride guard + adapter pin.
- Media-pipe **write timeout** (hung-daemon output stall — deferred; the one
  remaining silent-freeze vector, transport-side).
- `rotate_rgba` tiling (58 ms/frame at 4K, rotated monitors only).
- Linux `h264_qsv` low-latency opts (S).
- Mac quick trio: VT low-latency + QoS + App Nap (S each, high value).
- MJPEG `re_emit` stamps scaled dims as source dims (cosmetic).
- ARCHITECTURE.md macOS-encode prose drift.
- Env-tooling note: this box builds with a portable CMake
  (`Temp\cmk\...`), short `CARGO_TARGET_DIR=Temp\amsbld` (MAX_PATH), `-j 16`.
