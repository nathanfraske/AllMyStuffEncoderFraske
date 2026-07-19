# Integration report — the encoder fork line, for Chris's review

_Prepared 2026-07-18 by nathanfraske's encoder line
(`github.com/nathanfraske/AllMyStuffEncoderFraske`, branch `main`)
against upstream `mrjeeves/AllMyStuff` at the shared merge-base
`78b1c76` (`chore(release): 0.2.46`). **Everything here is a strict
prototype until your signoff.** No version numbers were claimed: every
manifest sits at your 0.2.46 (an early same-day 0.2.47/0.2.48 bump was
reverted, `86615bd`); prototype installers are named
`AllMyStuff-PROTOTYPE-<commit>_0.2.46_x64-setup.exe`._

**The whole line: 53 commits · 41 files · +15,535 / −873.**

---

## 1 · The contract this line was built under

Two rules were treated as inviolable, and section 4 is the audit:

1. **The signaling layer carries zero media and zero new anything.**
   Every byte this line moves rides the ICE-negotiated STUN/TURN
   datapath through the daemon's existing surfaces: media on the
   `ChannelSendTo`/track-lane sends that already existed, control
   (feedback, tune, input) on `CHANNEL_CONTROL` — the same channel
   those messages always used. No new channels, no new daemon requests,
   nothing on the signaling path.
2. **MyOwnMesh is frozen.** The daemon in every prototype bundle is
   your pinned build, byte-identical (`myownmesh.exe`, 2026-07-17
   pin). All pacing/shaping is app-side: what bytes we hand the
   daemon, when, in what sample framing. The pacer was verified
   against `H264AuAssembler`'s per-marker reassembly + contiguity
   anchor semantics and depends on them exactly as documented — it
   never modifies them.

## 2 · The exact pipeline, as it ships today

### Sender (host) — per display route

```
DXGI duplication (damage-driven, cursor save-under)          win_capture.rs
  └─ D3D11 VideoProcessor BGRA→NV12, same device, zero CPU   gpu_pipeline.rs
     px touches; NV12 texture ring (6 slots, depth-2
     retirement); ClockKeeper boost-clock heartbeat
       └─ Encoder ladder (per posture)                        video.rs run_gpu_lane
          NVENC SDK (runtime-loaded nvEncodeAPI64)            nvenc.rs
            · Balanced: P-preset VBR, adaptive IDR cadence
            · Game:     P2+ULL, GDR intra-refresh (no IDR
                        walls), single-frame VBV, loss-aware
                        wave heals (3-frame short heal in a
                        lossy spell)
            · Studio:   P5 HQ, deep VBV, uncapped-by-mode
            · Studio·Lossless: HEVC Main constQP-0 +
                        transquant bypass (bit-exact)
          → AMF (loader staged, Radeon pending)               amf.rs
          → MF hardware MFT (vendor-neutral fallback)         mediafoundation.rs
          → openh264 software floor                            video.rs
          → MJPEG compatibility floor (untouched from
            upstream behavior)
       └─ In-place rate reconfigure seam (no reset, no IDR)   nvenc.rs set_bitrate
          driven by the AIMD controller — GAME POSTURE ONLY
          by default (see §5), never lossless, never
          user-pinned bitrates
  └─ App-side slice pacer                                     mesh.rs send_video_paced
     splits AUs at slice-NAL boundaries (encoders emit
     count-mode slices: 8/16/32), spreads chunks with
     rate-matched gaps:
       · LAN: 800 Mbps drain, 6/10 ms budgets (shallow-
         buffer burst shaping — the original tuning)
       · WAN: drain = route send-rate ×1.5, budget ≤ one
         frame interval (never hands a 40 Mbps path an
         890 Mbps instantaneous wall)
     gaps executed against deadlines via a high-resolution
     waitable timer + bounded spin (os_perf.rs precise_sleep;
     measured 435 µs worst overshoot on the dev box)
  └─ Daemon track-lane send (existing API, one send per        control_client.rs
     chunk; non-final chunks duration_us=0 → assembler
     emits per marker, exactly as before)
═══ MyOwnMesh (FROZEN) — RTP packetization, ICE/STUN/TURN ═══
```

### Viewer — per inbound route

```
═══ MyOwnMesh (FROZEN) — delivers assembled samples ═══
handle_video_inbound                                          mesh.rs
  ├─ arrival timing: the pacer's chunk trains are timed
  │  packet trains → bandwidth estimate (dispersion EWMA)
  │  + one-way-delay trend (standing-queue early warning)
  └─ DecodeBridge (native lane; webview WebCodecs path         video_decode.rs
     unchanged where it works)
       H.264 → openh264 (universal)
       HEVC  → NVDEC (NVIDIA, CUDA-warm)                      nvdec.rs
             → D3D11VA (vendor-neutral ID3D11VideoDecoder:    d3d11va.rs
               AMD/Intel/iGPU; in-house scoped HEVC parser,
               stateless DXVA submission, byte-exact-proven
               against NVENC on real silicon, 60/60)
       → NV12→RGBA threaded convert → IPC to the window
  └─ Feedback (receiver → sender, CHANNEL_CONTROL — the
     existing message, additively extended):
     recv_fps · decode_fails · queue_depth · lost_ts_us
     + est_kbps + delay_trend_us_per_s (serde-default)
       → sender: adaptive IDR cadence (existing), GDR wave
         restart w/ loss-aware length, AIMD bitrate
         (Game-only by default), resolution auto-scale
         (pre-existing, still opt-in)
```

### Observability (the field-log layers)

Every layer's bandwidth is named in the standard log
(`%LOCALAPPDATA%\AllMyStuff\logs\allmystuff-serve.log`):
`video out` = `raw N Mbps → wire N Mbps` (pixels into the encoder →
encoded bytes to the track lane) · `pace gaps` = requested-vs-actual
gap fidelity per minute · viewer `video in` = chunk-train dispersion
percentiles + estimate · `video decode` = `wire → nv12 → rgba Mbps` ·
1 Hz telemetry = CPU (proc/total/per-thread), per-engine GPU busy,
VRAM, monitor topology.

## 3 · Blast radius — what was touched, ring by ring

### Ring 0 — new files (pure additions; nothing of yours modified)

| File | What it is |
|---|---|
| `node/src/nvenc.rs` (+2463) | Direct NVENC SDK rung, runtime-loaded, FFI hand-transcribed from MIT ffnvcodec headers |
| `node/src/nvdec.rs` (+1012) | NVDEC HEVC decode rung (nvcuvid), byte-exact-proven vs NVENC |
| `node/src/d3d11va.rs` (+1960) | Vendor-neutral D3D11VA HEVC decode rung (stateless DXVA, in-house scoped parser) |
| `node/src/gpu_pipeline.rs` (+739) | D3D11 VideoProcessor convert + NV12 ring + device manager + ClockKeeper |
| `node/src/amf.rs` (+139) | AMD AMF loader/probe/vendor-gate (staged; Radeon box pending) |
| `node/src/telemetry.rs` (+299) | 1 Hz vendor-neutral WDDM telemetry line |
| `node/src/os_perf.rs` (+430) | Timer/priority/EcoQoS/CPU-set levers + Win11 process opt-outs + precise_sleep + MMCSS opt-in |
| `gui/src/fsr1.ts` (+256), `gui/src/ui/ModePill.svelte` (+219) | Popout FSR1 upscaler; shared Mode control |
| `.github/workflows/node-check.yml` | Soft cross-platform clippy CI |
| `docs/*` (3 reports), `HANDOFF.md` | Engineering documentation |

### Ring 1 — media-plane files substantially extended (the fork's purpose)

| File | Nature |
|---|---|
| `node/src/video.rs` (+3877/−…) | Encoder ladder, postures, pacer support, stats, AIMD controller, wave chooser. **Your CPU pipeline remains intact as the fallback** — the GPU lane fails soft into it; the MJPEG floor is behaviorally unchanged. |
| `node/src/win_capture.rs` (+988) | GPU capture path added beside the existing one; CPU capture retained |
| `node/src/mediafoundation.rs` (+652) | Device-manager/texture feed, adapter pinning, in-place bitrate; the MFT remains the vendor-neutral default rung |
| `node/src/video_decode.rs` (+402) | Native decode bridge: HEVC ladder, layer-bandwidth stats |
| `node/src/mesh.rs` (+389) | The pacer (`send_video_paced`), arrival timing, feedback enrichment — all within the existing forwarder task; every daemon call is a pre-existing API |
| `node/pixels/src/lib.rs` (+312) | Convert-path speedups (from the July encoder pass) |
| `node/src/bin/serve.rs`, `hwenc.rs`, `videotoolbox.rs`, `input_inject.rs`, `control_client.rs` | Log location, QSV/VT low-latency opts, injector boost, bounded daemon-pipe write (a wedged-daemon guard — drops/reconnects instead of stalling forever) |

### Ring 2 — shared/wire surfaces (review these closest)

| File | Exact change | Wire compatibility |
|---|---|---|
| `crates/allmystuff-protocol/src/app.rs` | `RouteControl::Tune` += `game: bool`, `mode: Option<String>`, **`ext: serde_json::Value`**; `RouteControl::VideoFeedback` += `lost_ts_us: Option<u64>`, **`ext: serde_json::Value`** — all `#[serde(default)]`, additive, nothing renamed/removed. **`ext` is the pipeline's opaque bag** (see the boundary note below): the protocol crate relays it verbatim and never inspects it, so future pipeline signals never add fields here. | Old peer receiving new fields: ignores them (serde). New peer receiving old messages: defaults (off/zero/null). Proven both directions by the session round-trip tests, including an explicit "ext relayed verbatim" assertion. |
| `crates/allmystuff-session/src/media.rs` (+7) | `InputAction::MouseMoveRel` (pointer-lock deltas) — additive enum variant behind the existing catch-all | Old receivers drop unknown actions by the pre-existing catch-all arm |
| `crates/allmystuff-session/src/lib.rs` (+34) | `Effect::VideoFeedback`/`TuneMedia` carry the new fields through; same gate logic (`is_active() && peer == from`) untouched | Internal to the app; wire shape is the protocol crate's |
| `crates/allmystuff-mobile-core/src/control.rs` (+7) | The phone's `video_feedback` constructor fills the new fields with absent/zero | Pure constructor parity |
| `gui/src-tauri/src/main.rs`, `gui/mobile/src/commands.rs` (+4 each) | Tune command plumbs `game`/`mode` through | GUI-internal |
| `node/src/node_control.rs` (+19) | `video_feedback` op passes through; no new ops | GUI-internal |

## 3.5 · The backend boundary (why future pipeline work won't touch your crates)

By design, all encoder/decoder/pacing tuning is confined to the node's
video modules; the protocol/session/GUI never need touching for it. The
enforcing seam: `VideoFeedback`/`Tune` carry an **opaque
`ext: serde_json::Value`** the protocol and session crates relay
verbatim and never inspect. The node backend owns its shape
(`video::PipelineFeedback::to_ext`/`from_ext` at the mesh edge). So a
new receiver-side signal is a field on that struct; a viewer-requested
encoder knob reads `Tune`'s `ext`; a Labs feature is `labs::on(...)`.
None cross into your crates. This keeps every future pipeline change's
blast radius inside `node/src/{video,nvenc,nvdec,d3d11va,amf,
mediafoundation,gpu_pipeline,win_capture,labs,os_perf}.rs` + the
`mesh.rs` pacer — review those, and pipeline evolution stays there.

## 4 · Untouched surfaces — the guarantees

- **Signaling: zero changes, zero new payloads.** Nothing in this line
  touches, wraps, or adds to the signaling path. All additions ride
  the ICE-negotiated STUN/TURN datapath on surfaces that already
  carried the same class of traffic (media → existing track lanes;
  control → the existing `CHANNEL_CONTROL` messages, additively
  extended). Verifiable: `git diff 78b1c76..HEAD` contains no
  signaling-layer code at all — the signaling implementation lives in
  MyOwnMesh, which is…
- **MyOwnMesh: not in this repo's diff, and the pinned daemon binary
  in every prototype bundle is byte-identical to yours.** The
  `.myownmesh-rev` pin is unchanged. `H264AuAssembler` semantics
  (emit per RTP marker, contiguity anchor across split writes) are
  *depended on*, verified against, and unmodified.
- **RTP/packetization/FEC/retransmission:** untouched (daemon's).
  Explicitly rejected as out of bounds in
  `docs/fork/SMOOTHNESS-IDEAS-2026-07.md` (fork-internal) §"Explicitly rejected", so future
  sessions don't drift there either.
- **Your CPU capture/encode pipeline:** intact and load-bearing — it
  is the fallback every new lane fails soft into, and the only path on
  non-Windows/adapter-pinned/duplication-denied routes.
- **The MJPEG compatibility floor, presence/rooms/terminal/files/
  clipboard/audio planes:** behaviorally unchanged (audio untouched
  this line beyond the pre-existing plane; the deferred "studio sound"
  uplift was explicitly parked).
- **Defaults philosophy:** Balanced stays the default posture with
  your stream shape; everything aggressive is posture- or env-gated
  (see §5). The resolution auto-scaler you'd find in `AutoAdapt`
  remains **opt-in** exactly as before.

## 5 · Automatic behaviors and their gates (the reservation rule)

Ranked by how much they act without being asked:

| Behavior | Default | Gate/dial |
|---|---|---|
| AIMD bitrate (closed loop) | **Game posture only** — the one mode whose use case (smoothness > quality, always) makes an automatic cut correct in every case. Balanced/Studio never auto-change quality. | `ALLMYSTUFF_RATE_ADAPT`: unset=game-only · `1`=all lossy postures (field A/B) · `0`=off. Never lossless, never user-pinned bitrates. |
| Resolution auto-scale | OFF (pre-existing) | `ALLMYSTUFF_AUTO_ADAPT=1` |
| Adaptive IDR cadence | ON (pre-existing, benign recovery lever: 2 s↔8 s) | — |
| GDR wave + loss-aware length | Game posture only (GDR streams) | posture-gated |
| Link-fitted pacer drain | ON (LAN keeps original constants; WAN stops inheriting them) | `ALLMYSTUFF_PACED_SLICES=0` off · `ALLMYSTUFF_PACE_DRAIN_MBPS` pin |
| NVENC rung | ON with soft fallback to MF | `ALLMYSTUFF_NVENC=0` pins MF |
| GPU zero-copy lane | ON with soft fallback to CPU lane | `ALLMYSTUFF_GPU_LANE=0` |
| MMCSS scheduling class | OFF | `ALLMYSTUFF_MMCSS=1` |
| HEVC decode rung choice | auto (NVDEC→D3D11VA) | `ALLMYSTUFF_HEVC_DECODER=nvdec\|d3d11va` |

Full dial inventory: `AUTO_ADAPT, CWD_LOG, DIAG_DIR, GAME_MODE,
GPU_HEARTBEAT[_MS|_PX], GPU_LANE, GUI_LOG, HEVC_DECODER, HEVC_DUMP,
LOG, MJPEG_MAX_EDGE, MMCSS, NVENC, NVENC_PRESET, PACED_SLICES,
PACE_DRAIN_MBPS, RATE_ADAPT, SERVE_BIN, SOAK_*, TELEMETRY[_SECS],
VIDEO_BITRATE, VIDEO_ENCODE_ADAPTER, VIDEO_FPS, VIDEO_MAX_EDGE,
VIDEO_STATS`.

## 6 · The full commit list (oldest → newest)

Each is reviewable in isolation; conventional-commit messages carry the
full reasoning.

```
b3b40c4 perf(video): encoder correctness + latency pass
2e7aed8 docs: encoder-pass report (before/after profiles)
cb66461 feat(video): game-mode slice 1 + QSV low-latency + macOS QoS + VT low-latency
83d0b1c perf(video): pipeline the pump — capture/convert ∥ encode
fa136b6 perf(video): buffer-reuse lanes (interleaved A/B adjudicated)
9bf7485 fix(video): stable-arc hardening (pipe timeout, DXGI re-promotion)
bb53784 feat(video): GPU zero-copy core (VideoProcessor + texture-fed MFT)
b7aa331 feat(video): GPU zero-copy lane goes live (soft-fallback)
1cba76a bench(gpu): matched A/B/C encode benches
19e6429 feat(mf): in-place bitrate re-aim (downshift pilot)
176c47b feat(nvenc): direct NVENC rung — SDK scaffold, GDR proven
7c56cdf ci: cross-platform node check (soft)
32d94a1 feat(video): app-side slice pacer — zero MyOwnMesh changes
d46d06b bench(nvenc): SDK-vs-MF cycle bench
47be07f feat(video,gui): v2 field round (slot-race fix, faster GDR heal)
e45bf09 fix(video): pin negotiated transport through heals
3aa2c2d feat(video): NVENC default-on (soft-fallback keeps MF)
01ba54c chore(video): repair UTF-8 double-encoding
2db15bd feat(video): game-kernel pass (1 ms quanta, single-frame VBV)
91b8b7b feat(input,gui): fullscreen pointer lock (MouseMoveRel)
b78f103 feat(video,gui): Studio mode framework (tri-state)
7b50607 feat(gui): codec labels + Studio bandwidth confirmation
83e2dcd feat(video,gui): uncap Studio; raise Game ceiling
a1d86de bench(nvenc): four-posture matrix
ecff5fa bench(nvenc): posture soak harness
59e9197 feat(gui): FSR1-style viewer upscaling
2e90097 feat(nvenc): lossless measurement rung + bandwidth bench
57bd013 docs(nvenc): soak-gradient methodology note
d895621 feat(nvenc): HEVC-lossless rung (hardware-decodable lossless)
3e9c0fd feat(nvdec): NVDEC HEVC decode, byte-exact vs NVENC
72c0467 feat(decode): native lane speaks HEVC (bridge wiring)
3fe189b feat(video): Studio-Lossless as a mode
cdea827 probe(av1): AV1-lossless hardware probe (50-series pending)
f460b13 feat(amf): AMF loader/probe/vendor gate (Radeon pending)
7f7084b feat+fix: pre-presentation hardening (red team ×3)
4fd853e perf(nvenc): preset ladder measured (game P2, lossless P3)
fec7168 feat(telemetry): field-test line (CPU/GPU-engines/VRAM)
5746ee8 feat(telemetry): per-thread CPU + monitor topology
c77c7d0 feat(capture): log datastream-to-monitor binding
becc220 feat(telemetry): 1 Hz default cadence
3a00a4c feat(video): frame health (loss names the frame; GDR wave heal)
55cec0e fix(serve): guaranteed-writable log location
e382997 feat(nvenc): reference-invalidation mechanism + scoping finding
bcd06f8 refactor(gui): one shared Mode control
9341cf8 docs: engineering handoff
91c5448 feat(decode): D3D11VA rung — Studio·Lossless crosses vendors
be32d3c fix(gui): fullscreen fits the popout picture to the monitor
97803f7 build: 0.2.47 (REVERTED by 86615bd — versioning is yours)
1a437f1 docs: smoothness/pacing/latency idea bank
a97678f perf(os): scheduling honesty (Win11 opt-outs, precise sleep)
1a8d188 feat(video): closed WAN loop (link-fitted pacer, BWE, AIMD, waves)
d100022 feat(video): auto-bitrate reserved to Game; per-layer bandwidth logs
86615bd build: hold versioning at 0.2.46 — versions are Chris's to cut
f5f1f21 docs: this integration report
b992776 perf(kernels): AVX2 + non-temporal NV12→RGBA; memchr start-code
        scan — the viewer convert 3.5→1.8 ms avg @1440p (byte-exact
        pinned by test); pacer's Annex-B walk memchr-anchored
```

Per-file diffstat vs `78b1c76`: see `git diff --stat 78b1c76..HEAD`
(41 files, +15,535/−873; the large four are the new FFI rungs
`nvenc/nvdec/d3d11va` and `video.rs`).

## 7 · Verification status

- **Unit + integration:** 165 passing in the node crate (0 failing;
  audio module skipped on the dev box — environmental access
  violation, pre-existing), 140 passing across the shared crates
  (protocol/session/mobile-core round-trips include the new fields
  both directions).
- **Hardware-proven on the dev box (RTX, Ampere):** NVENC→NVDEC HEVC
  lossless round trip 60/60 byte-exact; NVENC→**D3D11VA** 60/60
  byte-exact at 1280×718 (conformance-window crop live) and 30/30
  byte-exact through real pacer chunking; GDR produces zero IDR walls
  and decodes clean; `precise_sleep` worst overshoot 435 µs.
- **Benches:** NVDEC 4.24 ms vs D3D11VA 5.76 ms avg decode+copy at
  1440p (ladder order is measured); preset grid (game P2 5.7 ms);
  encode-path A/B/C columns in `docs/fork/ENCODER-PASS-2026-07.md` (fork-internal); the
  NV12→RGBA viewer kernel 3.5 → 1.8 ms avg @1440p after the AVX2 +
  non-temporal-store pass (`b992776`, byte-exact against the scalar
  reference by test).
- **Field-pending (2-machine rig):** BWE estimate accuracy vs imposed
  rates, freeze-seconds A/B for the closed loop (Game posture),
  pace-gap fidelity on a stock Win11 box, the D3D11VA rung on the
  incoming Radeon 9060 XT (probe: `probe_d3d11va_hevc_configs`), and
  the monitor-refuses-to-connect log capture.

## 8 · Suggested review order

1. `crates/allmystuff-protocol/src/app.rs` — the only wire-visible
   surface (30 additive lines).
2. `node/src/mesh.rs` — `send_video_paced` + `note_video_arrival` +
   `send_video_feedback`: everything that touches the daemon boundary.
3. `node/src/video.rs` — postures, ladder, AIMD gate
   (`rate_adapt_mode`), wave chooser.
4. `node/src/nvenc.rs` / `nvdec.rs` / `d3d11va.rs` — self-contained
   FFI rungs, each with its proving tests at the bottom.
5. `node/src/os_perf.rs` — the OS levers, all best-effort.
6. `docs/fork/SMOOTHNESS-IDEAS-2026-07.md` (fork-internal) — where this is headed next and
   what was explicitly rejected (the frozen-daemon rule, encoded).
