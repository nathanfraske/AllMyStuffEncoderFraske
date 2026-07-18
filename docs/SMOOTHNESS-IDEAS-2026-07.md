# Smoothness, pacing, and end-to-end latency — engineering brainstorm (2026-07)

_Scope: WAN + LAN smoothness/pacing/latency for the encode→pace→daemon→decode→paint
pipeline. Hard constraint honored throughout: **MyOwnMesh is frozen** — every idea
here changes only what bytes we hand the daemon, when, and in what sample framing,
plus receiver-side behavior and our own control messages. Nothing touches the
daemon's RTP packetization, its pacing, or the signaling layer (which carries zero
media, ever). Postures referenced: **Game** (GDR, latency-first), **Balanced**,
**Studio** (lossy, quality-first), **Studio·Lossless** (bit-exact HEVC)._

---

## Executive summary — top 5 by (expected win ÷ effort)

1. **Link-fitted pacer drain model** (T2.1) — replace the fixed 800 Mbps constant and 6/10 ms budget caps in `send_video_paced` with a link-class/measured rate. *Why now:* the current math bursts a ~190 KB keyframe at ~5–20× a 40 Mbps WAN path's line rate — a designed-in 2-frame queue spike, fixable in ~30 lines at one seam.
2. **Closed-loop bitrate (AIMD) on existing feedback** (T1.2) — wire `note_feedback` → `NvencH264::set_bitrate`. *Why now:* both ends already ship (live no-IDR reconfigure + 2 s viewer reports); Game-over-WAN is open-loop today by admission, and this is the single biggest WAN smoothness lever in every shipping competitor.
3. **Windows-11 scheduling honesty bundle** (T3.1+T3.2) — MMCSS on media threads, process-wide power-throttling + timer-resolution opt-outs, high-res waitable timers for the pacer. *Why now:* Win11 can silently ignore our `timeBeginPeriod(1)` for windowless background processes — every pacing constant in the codebase currently means "±15.6 ms" on a stock field box.
4. **Chunk-train bandwidth estimation** (T1.1) — the pacer already emits timed ~24 KB packet trains; timestamp their arrivals in `handle_video_inbound` and you get a per-second bottleneck estimate for free. *Why now:* it upgrades idea 2 from guessing to knowing, with zero wire changes.
5. **LTR-anchored loss recovery** (T1.4) — long-term reference + "last-good" feedback instead of IDR walls on loss. *Why now:* commit `e382997` proved `invalidate_ref` recovery trips strict decoders on the frame_num gap; LTR is the conformant sibling that reuses the exact same `lost_ts_us`/`input_ts` plumbing that already ships.

---

## Tier 1 — industry-standard techniques we haven't adopted yet

### T1.1 · Receiver-side bandwidth estimation from the pacer's own chunk trains (GCC/transport-cc, transposed app-side)

- **Postures:** Game and Balanced on WAN first; Studio-over-WAN honesty second.
- **What/why:** WebRTC's congestion controller (GCC + transport-cc) works from two
  signals: per-packet arrival-time deltas vs send-time deltas (delay gradient → queue
  is growing) and loss. We can't see RTP packets — but we don't need to. The app-side
  pacer already sends every large AU as a **train of ≤`PACE_SLICE_BYTES` (24 KB)
  samples with known departure spacing** (`send_video_paced`: `bytes/100 µs` gaps),
  and the daemon delivers each chunk as its own sample to `handle_video_inbound`
  with a shared `rtp_timestamp`. That is a textbook **packet-train probe**: for any
  AU with ≥3 chunks, `spread_bytes / arrival_spread` at the receiver is a direct
  bottleneck-capacity sample (arrival spacing can only stretch to line rate — the
  same math as pathload/packet-pair, and the same dispersion filter GCC's arrival
  model uses). Separately, per-AU one-way-delay *trend* (arrival wall-clock minus the
  RTP timestamp, slope over a 1–2 s window — no clock sync needed for a slope)
  detects standing-queue growth before loss does.
- **Where it lands:** viewer: `mesh.rs handle_video_inbound` (stamp `Instant::now()`
  per sample; group by `rtp_timestamp`; maintain a min-filtered capacity EWMA and a
  delay-trendline slope). Feedback: extend `RouteControl::VideoFeedback` with
  `#[serde(default)]` fields (`est_kbps`, `delay_trend_us_per_s`) — old senders
  deserialize-and-ignore, old viewers simply never send them. Sender:
  `video.rs note_feedback` stores them in `RecvFeedback` for T1.2/T2.1 to read.
- **Expected win:** the missing WAN closed loop. Concretely: lets Game/Studio ride at
  the *measured* rate instead of the blind 40 Mbps WAN cap (`h264_bitrate_for`), and
  lets the pacer spread bursts at the *measured* drain instead of 800 Mbps (T2.1).
  Literature and WebRTC field data put delay-gradient control at **2–10× lower p99
  queuing delay** than loss-based control at equal throughput.
- **Risk/effort:** medium (≈1 week with tests). Noise sources: the daemon's own
  internal queuing jitters arrival stamps (measure first — M3 below); small AUs give
  no train (fine — capacity only updates on bursts, which is when it matters); ICE
  path migration must reset the filter (`LinkClass` change is the hook).
- **Validate:** M3's log line first; then A/B under `clumsy` (rate-limit 20/40/80
  Mbps, ±20 ms jitter): estimate must track the imposed rate within ~15%, and
  freeze-count/queue_depth must drop vs today at equal settings.

### T1.2 · Closed-loop bitrate adaptation on the existing live-reconfigure seam (AIMD now, GCC target later)

- **Postures:** Game, Balanced, Studio (lossy). Explicitly not Studio·Lossless
  (`set_bitrate` already returns `false` on constQP-0 — correct).
- **What/why:** Every shipping remote-play stack (Parsec, Moonlight/Sunshine, Steam
  Remote Play, GFN/xCloud) closes a rate loop; we currently adapt only IDR cadence
  (`adaptive_idr_ms` 2 s↔8 s) and, opt-in, resolution (`AutoAdapt`, default OFF).
  The far better first dial is **bitrate**: `NvencH264::set_bitrate` already re-aims
  mean/peak/VBV **in place, no reset, no IDR**, and re-applies the posture's VBV
  shape (game single-frame VBV survives — the red-team fix is already in). Start
  with AIMD fed by today's signals (queue_depth > 8 or recv_fps sag → multiplicative
  cut to ~0.7×; clean 2 s reports → additive +5% up to the posture lane), and swap
  the target to T1.1's estimate when it lands. Fast-down/slow-up mirrors the proven
  `AutoAdapt` streak/hold discipline — reuse those constants' logic, not new tuning.
- **Where it lands:** `video.rs note_feedback` (compute new target) → a per-route
  atomic the encode threads read next frame → `GpuCodec::set_bitrate` (NVENC full
  fidelity; MF rung partial — documented; openh264 has its own setter). Floor at
  8 Mbps (the existing budget floor), ceiling at the posture lane (`tuned_bitrate`).
  Log every step on the `video out` stats line.
- **Expected win:** on any WAN path narrower than the configured rate, this converts
  "periodic multi-frame stalls + dumped decode queues + re-keys" into a soft quality
  dip. It is the difference between the stream *having* a bad second and the stream
  *being* unusable. LAN: no-op (never triggers).
- **Risk/effort:** low-medium (2–3 days + soak). Main risk is oscillation —
  mitigated by the streak/hold pattern and by stepping bitrate (cheap, invisible)
  long before resolution (visible). Interaction to watch: the adaptive IDR relax
  reads `queue_depth <= 8` too; a bitrate cut should not race a cadence tighten
  (both are correct responses; just log both).
- **Validate:** `clumsy` ladder (40→20→10 Mbps steps every 30 s): recv_fps must
  stay ≥ 0.9× target through every step; compare freeze seconds/minute vs today.

### T1.3 · App-layer erasure parity across a frame's paced slices (the Moonlight/Sunshine-FEC analog at sample granularity)

- **Postures:** Game-over-WAN primarily; Balanced/Studio WAN keyframes second.
- **What/why:** Every serious UDP game-streamer ships FEC (Sunshine sends ~20%
  Reed-Solomon parity per frame) because retransmission costs an RTT and keyframes
  are all-or-nothing. We can't add packet-level FEC (daemon frozen) — but our loss
  unit at the app is the **chunk-sample** (a lost/short RTP packet kills one ≤24 KB
  sample, or corrupts the AU — M5 characterizes which). So send, per multi-chunk AU
  (keyframes: 8 slices lossy, 32 lossless; GDR wave frames), **one trailing parity
  sample**: `{magic, ts, chunk_count, per-chunk len+CRC32, XOR payload}`. The viewer
  CRC-checks received chunks, identifies the single missing/corrupt one, and
  reconstructs it before `video_decode.feed`. XOR-of-8 = 12.5% overhead **on
  keyframes only** (≈0 steady-state in GDR game mode, where wave frames are
  near-normal size); recovers any single-sample loss without a round trip.
- **Where it lands:** sender: `video.rs` beside `split_annexb_paced` (build parity
  over the chunk ranges) + `mesh.rs send_video_paced` (ship it as one more track
  send, `duration_us = 0` before the final real chunk so the marker stays on real
  data). Viewer: `mesh.rs handle_video_inbound` — parity samples start with a magic
  that `sniff_codec` can never match (no start code); **must be gated on the
  planned viewer decode-capability handshake**, because an old viewer would feed
  the blob to its decoder and glitch-storm. That handshake is already the
  recommended next build item — this rides it.
- **Expected win:** at 1% random packet loss, a 160-packet keyframe wall dies with
  p≈80% today (one lost packet anywhere) → a 2 s-worst-case re-key or a wave; with
  single-erasure parity per 8-chunk group, group death needs 2 losses in ~20
  packets (p≈1.6%) — roughly **10–50× fewer keyframe deaths** at low loss rates.
- **Risk/effort:** medium-high (wire framing discipline, handshake dependency,
  characterization first). Bandwidth overhead concentrated exactly at the worst
  moment (the wall) — must ride *inside* the pacer's spread, not after it.
- **Validate:** M5 first (what the daemon delivers under loss). Then `clumsy` 0.5–2%
  loss: count re-keys/min and freeze time with parity on/off; assert byte-exact
  reconstruction in a unit test over `split_annexb_paced` output.

### T1.4 · LTR-anchored recovery: reference selection on loss, the conformant way

- **Postures:** Balanced and Studio (lossy H.264) — the postures whose only heal
  today is an IDR wall; Game keeps GDR waves (already better).
- **What/why:** GameStream-class recovery: keep a **long-term reference** the viewer
  is known to have; on a loss report, encode the next frame predicting from that LTR
  — a normal-sized P-frame, no wall, and (unlike `invalidate_ref`'s aftermath,
  commit `e382997`) **frame_num-continuous and fully conformant**, so openh264 and
  WebCodecs viewers accept it. NVENC exposes this directly: `enableLTR` +
  `ltrNumFrames` at init; per-picture `ltrMarkFrame`/`ltrUseFrames` in the H.264
  pic-params — the same hand-transcription discipline as
  `H264_PIC_FORCE_INTRA_REFRESH_IDX`. Mark an LTR every ~1 s; the viewer's periodic
  feedback adds `last_good_ts_us` (highest cleanly-decoded AU ts — one field on the
  existing report); on `lost_ts_us`, the sender re-anchors to the newest LTR ≤
  last_good instead of forcing an IDR.
- **Where it lands:** `nvenc.rs` (config + pic-params fields, a probe test in the
  `probe_nvenc_av1_lossless` style — several SDK generations document LTR and
  intra-refresh as mutually exclusive, so probe, don't assume), `video.rs`
  `route_wave_or_refresh` (third rung between wave and `force_idr`),
  `VideoFeedback` (+`last_good_ts_us`, `#[serde(default)]`).
- **Expected win:** loss recovery cost drops from a ~190 KB IDR (which itself risks
  re-loss — the all-or-nothing wall) to a ~1× P-frame; recovery latency from
  "next 2 s IDR or explicit re-key round trip" to one frame after the report.
  This is also the deep-DPB (`max_num_ref_frames = 8`) finally earning its keep.
- **Risk/effort:** medium (FFI + probe + feedback field; ~3–4 days). Risks: driver
  LTR quirks per generation (probe catches), and stale-LTR drift (bound LTR age;
  fall back to IDR past ~2 s — exactly today's behavior).
- **Validate:** two-machine lossy link (the same field test #22 is gated on):
  inject loss, assert log shows `LTR re-anchor` with no IDR emitted, viewer
  picture heals within ≤2 frames, and openh264/WebCodecs viewers show zero
  decode-fail feedback after heal.

### T1.5 · Lossy-HEVC posture behind the decode-capability handshake (bits → smoothness)

- **Postures:** Studio-over-WAN, Game-over-WAN on capable pairs.
- **What/why:** xCloud/GFN ship HEVC/AV1 to capable clients because **compression
  efficiency is a latency feature on a constrained link**: HEVC ≈ 30–40% fewer bits
  at equal quality, which at a fixed WAN capacity means proportionally smaller
  bursts, shallower queues, and fewer drops. All the pieces exist: the NVENC HEVC
  arm (currently gated to lossless constQP-0) needs only its VBR branch un-gated
  (`nvenc.rs` rate-control block already computes `burst_bounds` for H.264; the
  HEVC path just never takes it), the pacer already speaks HEVC slice framing
  (`split_annexb_paced` cuts HEVC VCL NALs), the viewer's hardware ladder
  (NVDEC → D3D11VA) already decodes HEVC cross-vendor, and the bridge already
  re-sniffs codec morphs mid-route. The one true prerequisite is the **viewer
  decode-capability handshake** (HANDOFF's recommended next item) so HEVC is only
  offered where hardware decode exists — WebCodecs/software viewers keep H.264.
- **Where it lands:** `nvenc.rs` (HEVC VBR config: same `burst_bounds`, GDR via
  HEVC intra-refresh fields for a Game variant later), `video.rs run_gpu_lane`
  posture selection (prefer HEVC when the handshake says the viewer decodes it and
  the link is WAN), handshake carrier in route negotiation (already planned).
- **Expected win:** at the 40 Mbps WAN cap, HEVC ≈ H.264-at-60 Mbps quality; or
  hold quality and cut the rate ~35% → smaller keyframe walls (~120 KB vs 190 KB),
  ~35% shallower worst-case queues. Compounds with T1.2/T2.1.
- **Risk/effort:** low-medium *after* the handshake (which is justified
  independently): the encode config is a day; the risk is spreading QA across two
  codecs per posture (pin with the existing byte-exact/round-trip test pattern).
- **Validate:** `bench_nvenc_preset_grid`-style quality/latency grid for HEVC VBR;
  A/B at fixed 20 Mbps `clumsy` rate: HEVC vs H.264 SSIM + freeze count.

---

## Tier 2 — novel / experimental (codec-transport co-design on our seams)

### T2.1 · Link-fitted, then measured, drain-rate model for the slice pacer

- **Postures:** all; the *correctness* fix is WAN, the *tail-latency* fix is LAN.
- **What/why:** `send_video_paced` spreads chunks with `gap = bytes/100 µs` (an
  800 Mbps drain model) clamped to [100 µs, 1 ms game / 1.5 ms other] per gap and a
  **6 ms / 10 ms total budget** per AU. Do the arithmetic for WAN: a ~190 KB
  keyframe = 8 chunks = 7 gaps ≈ 1.7 ms total spread ≈ **890 Mbps instantaneous
  into a path whose steady rate we cap at 40 Mbps** — 190 KB is 38 ms of line time
  there, so we hand the bottleneck a ~36 ms standing queue (>2 frames at 60 fps) or
  force tail drops, every wall. Even at the full budget caps it's 152–253 Mbps.
  The pacer was tuned for LAN shallow-buffer shaping and is honest about it — but
  WAN inherits LAN's constants. Fix in three stages: **(a)** link-class defaults
  now: on `LinkClass::Wan`, drain = the route's actual bitrate × ~1.5 (peaks exist)
  and budget ≈ one frame interval (16 ms at 60) — spreading a wall across its own
  frame slot adds zero pipeline latency by definition, since the next frame isn't
  due sooner; **(b)** feed the sender's own observable: the awaited
  `send_video_track` completion time per chunk (if the daemon pipe ever
  backpressures, that IS a drain signal — currently unread); **(c)** T1.1's
  measured capacity as the model. Also fix the silent quantizer: the gaps are
  `tokio::time::sleep` at 100–1000 µs, which the runtime rounds to ≥1 ms — the
  *actual* spread today is unmeasured and probably ~7 ms/wall in game mode (see
  M2); make the gap engine honest (coarse waitable-timer sleep + QPC finish, T3.2)
  before tuning constants on top of it.
- **Where it lands:** `mesh.rs send_video_paced` (drain/budget from a per-route
  source instead of literals), `video.rs` (plumb `LinkClass` + bitrate into the
  route's pacing state — `route_game` already shows the pattern).
- **Expected win:** WAN: removes a designed-in 2-frame queue spike per wall (and
  every Balanced/Studio IDR is a wall every 2–8 s). LAN Studio·Lossless: none
  (already correct — that's what the 800 Mbps model was built for). Game GDR:
  small steady-state effect (waves are near-normal frames) but joins/rescue IDRs
  stop stabbing the link.
- **Risk/effort:** low (the staged (a) is ~30 lines); risk is over-spreading on LAN
  — gate strictly on `LinkClass` and keep env dials for A/B.
- **Validate:** M2 + M3 before/after; `clumsy` at 40 Mbps: per-AU arrival spread at
  the viewer should match line rate, decode-queue depth p95 should drop by
  ~2 frames, no regression in LAN `video out` fps.

### T2.2 · Sub-frame slice streaming: send slice 0 while slice 7 is still encoding

- **Postures:** Game first (latency budget), Studio 4K second (biggest encode times).
- **What/why:** today the pipeline is strictly frame-granular: `encode_texture`
  returns the whole AU (5.7 ms P2@1440p, 12.9 ms studio presets, more at 4K), and
  only then does the pacer start spreading it. NVENC supports **sub-frame readback**
  (`enableSubFrameWrite` + per-slice offsets in `NV_ENC_LOCK_BITSTREAM`) —
  the GFN-class low-latency mode where slices become available as encoded. Since
  the pacer *already* ships slices as independent samples and the D3D11VA rung
  *already* assembles pictures from per-slice samples (close on first-slice/ts
  change/learned count), the receive side needs nothing: we'd simply be overlapping
  the encode tail with the wire time instead of serializing them.
- **Where it lands:** `nvenc.rs` (init flag + a slice-offset poll on the lock; probe
  test first — support varies by driver/codec), `video.rs run_gpu_lane` (emit
  units per-slice instead of per-AU; `packetize_units` grows a "same-ts
  continuation" form — non-final slices with `duration_us = 0`, exactly the framing
  `send_video_paced` already emits for chunks).
- **Expected win:** up to ~(encode time − first-slice time) off glass-to-glass:
  ~3–5 ms at 1440p game, ~8–10 ms at 4K studio. This is one of the few remaining
  *structural* latency cuts on the host.
- **Risk/effort:** high-medium: FFI surface + the pump's outcome seam changes shape;
  sync-mode interplay must be probed. Keep it env-gated (`ALLMYSTUFF_SUBFRAME=1`)
  through soak, like `ALLMYSTUFF_PACED_SLICES` was.
- **Validate:** e2e stamp (M1): encode-start→first-byte-to-daemon should drop from
  ≈encode-time to ≈first-slice-time; byte-exactness pinned by the existing
  round-trip tests (concatenated slices must equal the whole-AU encode).

### T2.3 · The recovery matrix + instant gap-NACK (finishing what `invalidate_ref` started)

- **Postures:** all lossy; per-pair behavior chosen by decoder identity.
- **What/why:** we now have four heal mechanisms of very different cost: GDR wave
  (game, shipped), LTR re-anchor (T1.4), `invalidate_ref` (shipped mechanism,
  gated on viewers that ride frame_num gaps — NVDEC does natively; our own
  D3D11VA/H.264 rung could be taught to, and the capability handshake can say so),
  and IDR (the floor). Make the choice explicit per route:
  `wave if GDR else LTR if probe-ok else invalidate if viewer-rides-gaps else IDR`
  — one function beside `route_wave_or_refresh`. Second half: **detection latency**.
  Today a loss is noticed when a decode *fails* — but the viewer can see it earlier:
  in `handle_video_inbound`, an `rtp_timestamp` discontinuity (a whole missing AU)
  or a short chunk-train (missing sample within an AU, once M5 characterizes
  framing) is knowable **on arrival of the next sample**, one frame before any
  decoder touches it. Fire `send_video_feedback(lost_ts_us = inferred)` immediately
  — the sender's wave/LTR heal starts an RTT earlier, which at WAN RTTs is the
  difference between a 1-frame and a 5-frame artifact window.
- **Where it lands:** viewer `mesh.rs handle_video_inbound` (ts-continuity check —
  ~15 lines); sender: the matrix function in `video.rs`; handshake carries
  "rides-gaps" and "LTR-ok" bits.
- **Expected win:** heal-start latency cut by ~1 RTT (25–80 ms WAN); IDR walls
  reserved for genuine resets. Together with T1.3 this is the full loss story:
  reconstruct if you can, re-anchor if you can't, wall only if all else fails.
- **Risk/effort:** low for gap-NACK (do it regardless); the matrix is glue once
  T1.4 exists. False-positive NACKs on reordering: the daemon reorders within its
  assembler, so app-level samples arrive ordered per lane — verify in M5, else
  debounce by one sample.
- **Validate:** loss-injection A/B measuring artifact-visible milliseconds per loss
  event (frame-health logs already name the AU; add the heal-mechanism tag).

### T2.4 · Speculative rescue layer: a tiny always-on second encode for WAN game mode

- **Postures:** Game-over-WAN, opt-in.
- **What/why:** Salsify-style insurance without SVC: run a second NVENC session
  (sessions are cheap on the encode engine — telemetry shows it far from saturated)
  at ~480p/2 Mbps, all-intra or 15-IDR, encoding the same NV12 textures. Ship it as
  a second lane the viewer only *paints* when the main stream is in a heal window
  (post-loss, pre-wave-completion). The user sees a soft picture for 100–300 ms
  instead of a frozen or smeared one — which is what Parsec/GFN's "resilience"
  feel actually is: never freeze, degrade.
- **Where it lands:** `video.rs run_gpu_lane` (second `GpuCodec` fed from the same
  retained textures — the NV12 ring's depth-2 retirement already guarantees
  liveness), `mesh.rs` lane allocation (`video_lane` pool grows a rescue slot per
  game route — pool budget is the real constraint to check), viewer route→lane map
  + paint arbitration in the decode bridge (paint rescue only while
  `waiting_key`/heal is active). Handshake-gated like T1.3.
- **Expected win:** perceived: freezes become blur dips. Measured: artifact-visible
  ms per loss event collapses to ≈ decode time of one rescue frame. Cost: ~2 Mbps
  + one encode session (~1–2 ms GPU at 480p — measure on the encode engine line).
- **Risk/effort:** high — this is the most speculative item here (lane budget,
  paint arbitration, two streams' worth of failure modes). Prototype behind an env
  dial on the 2-machine rig before any productization.
- **Validate:** loss soak: freeze-seconds/min and subjective side-by-side capture;
  telemetry `enc %` and VRAM before/after.

### T2.5 · Damage-driven encode emphasis and pacing grain (content-adaptive bits and bursts)

- **Postures:** Balanced/Studio for the emphasis map; all for adaptive grain.
- **What/why:** two uses of information we throw away today. (1) **Damage rects:**
  DXGI duplication offers `GetFrameDirtyRects`/`GetFrameMoveRects`; `win_capture`
  currently reads only frame presence + cursor. NVENC accepts a per-picture
  **QP-delta map** (`qpDeltaMap`, macroblock-granular): bias bits *into* dirty
  rects (where the eye is) and *out of* static regions — typing/scroll workloads
  get visibly crisper text at the same rate, and the post-quiesce refinement passes
  (`REFINE_PASSES`) get a map instead of re-spending on the whole frame.
  (2) **Adaptive grain:** the pacer's chunk cap is a constant 24 KB; on the same
  evidence (AU size vs steady-state mean, damage area), scale the *slice count*
  per-frame — NVENC's pic-params carry per-picture slice control — so a scene-cut
  frame leaves as 16 slices (finer spread under T2.1's WAN budget) while a
  10 KB delta stays single-slice (no CABAC-reset tax when there's nothing to
  shape). Slice-count-vs-quality cost is already characterized in the code
  (~1–3% for 32 slices lossless).
- **Where it lands:** `win_capture.rs` (fetch rects, pass a coarse mask through
  `GpuFrame`), `nvenc.rs encode_texture` (qpDeltaMap upload + per-pic slice
  fields — probe both; emphasis-map mode is CBR-gated on some drivers, the plain
  delta map is not), `video.rs` (grain decision beside `split_annexb_paced`).
- **Expected win:** emphasis: subjective sharpness at equal bitrate (hard to
  number honestly; the win is quality-per-bit, which becomes smoothness on WAN via
  smaller AUs). Grain: burst shape matched to burst size — mostly a WAN-tail win.
- **Risk/effort:** medium; per-driver probes required; the rect plumbing must not
  add a CPU touch per frame (keep it to rect metadata, never pixels).
- **Validate:** SSIM-on-text corpus at fixed rate (emphasis on/off); M3 arrival
  spread on scene-cut frames (grain on/off).

### T2.6 · Capture-clock-recovery paint scheduling at the viewer (smooth without a jitter buffer)

- **Postures:** Studio/Balanced (smoothness-first); Game keeps slam-immediate.
- **What/why:** the viewer paints freshest-wins on arrival (`enqueue_decoded`
  clears the queue; the webview paints on the eval poke) — minimum latency, but
  frame *display* cadence inherits network+decode jitter, which reads as micro-
  stutter on smooth motion (scrolling, panning) even when no frame is lost. The
  standard fix is a jitter buffer (adds a fixed fee); the better fit for us is
  **clock recovery without a queue**: PLL the sender's RTP timestamp against local
  monotonic (offset + drift), schedule each decoded frame's paint at
  `recovered_capture_time + headroom` where headroom auto-tunes to p95 jitter
  (~2–8 ms typical), and drop late frames (freshest-wins preserved). One frame is
  held at most a few ms; mean added latency ≈ headroom, but displayed cadence
  becomes the *capture* cadence. Parsec's "smoothest video" toggle is exactly this
  trade, and our `packetize_units` already emits honest wall-clock durations to
  drive it.
- **Where it lands:** GUI: `Console.svelte`/`VideoPopout.svelte` paint slot (the
  WebCodecs path holds `pendingFrame` already — schedule instead of rAF-slam;
  native path: schedule on the `ts_us` carried in the IPC header). No host change.
- **Expected win:** displayed-frame-interval stddev collapses toward capture
  stddev (measure: M4's paint-interval histogram) at ≤ +5 ms mean latency;
  subjectively this is the largest *smoothness* (as opposed to latency) item on
  the list for Studio scrolling/panning content.
- **Risk/effort:** low-medium, all in TS; the PLL must handle sender fps changes
  (damage-driven quiet spells) — headroom decays during quiet, re-learns on
  motion.
- **Validate:** paint-interval histogram before/after at 60 fps scroll over
  `clumsy` ±10 ms jitter; A/B blind eyeball on the popout.

### T2.7 · GDR wave shaping from loss telemetry (close the loop on the wave itself)

- **Postures:** Game.
- **What/why:** the wave is fixed: period `fps/2` (min 15), spread over
  `period/5` ≥ 3 frames (≈6 frames = 100 ms at 60 fps), and the per-picture force
  field already takes a frame count (`H264_PIC_FORCE_INTRA_REFRESH_IDX`) — so wave
  *length is already a per-call knob*, we just never vary it. Feed it the loss
  telemetry we already collect: repeated `lost_ts_us` within a window → shorten the
  healing wave to 3 frames (heal in 50 ms, pay a fatter per-frame intra share —
  which the single-frame VBV then smooths by *slightly* raising those frames'
  latency: the honest trade, bounded); clean links → stretch waves to 8–10 frames
  (thinner per-frame cost, better steady-state quality). Also coalesce: the wave
  flag is a bool (idempotent) — good — but a *second* loss report mid-wave should
  restart-with-short, not be absorbed silently.
- **Where it lands:** `nvenc.rs arm_wave`/`encode_texture` (parameterize the count),
  `video.rs` wave-flag seam (`wave_flags()` carries a small struct instead of a
  bool: `{restart: bool, frames: u8}`), loss-rate window beside `note_feedback`.
- **Expected win:** artifact-visible window per loss event 100 ms → ~50 ms on lossy
  links; steady-state intra tax reduced ~40% on clean links. Small, cheap, pure
  tuning of a mechanism that already works.
- **Risk/effort:** low (a day). Risk: thrash between shapes — hysteresis on the
  loss-rate window, mirroring `adaptive_idr_ms`'s conservative style.
- **Validate:** the existing `nvenc_intra_refresh_replaces_idr_walls` pilot
  extended with per-shape byte/frame profiles; field: frame-health log timestamps
  from loss report to clean recovery-point SEI.

### T2.8 · Viewer zero-copy present: decode-to-swapchain, skipping RGBA/IPC/canvas

- **Postures:** all, on native-decode viewers; biggest for Studio·Lossless 1440p+.
- **What/why:** the viewer's post-decode path today: NVDEC/D3D11VA → (staging copy
  + CPU `Map`) → threaded NV12→RGBA (2.8 ms @1440p) → IPC packet (14.7 MB per
  1440p frame — **~885 MB/s** at 60 fps through the webview boundary) → canvas
  `putImageData` (another copy + GPU upload). Every byte of that exists only
  because the presenter is a webview canvas. A D3D11 child-window swapchain over
  the console's video rect (Tauri exposes the HWND), fed the decoded NV12 texture
  through the same `ID3D11VideoProcessor` we already drive on the host
  (NV12→RGBA on GPU, zero CPU touches), with
  `DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT` + `SetMaximumFrameLatency(1)`
  + `Present(1)`/allow-tearing per posture — deletes the staging Map, the CPU
  convert, the IPC copy, and the canvas upload in one move. Moonlight's D3D11
  renderer is the proof this shape is the endgame for Windows viewers.
- **Where it lands:** new presenter module beside `d3d11va.rs` (which already owns
  a D3D11 device + decoded textures); `video_decode.rs` grows a "present in
  place" sink next to `on_frame`; GUI hosts the child HWND and keeps the canvas
  path for occlusion/screenshots/older paths. `nvdec.rs` output would need a
  CUDA→D3D11 interop surface (or keep NVDEC→staging for now and let the D3D11VA
  rung lead — it's already vendor-neutral).
- **Expected win:** −3 to −8 ms mean glass latency and a large jitter cut at the
  viewer (M4 will show the current spread), plus ~1 GB/s of memory traffic and a
  webview-thread stall source gone. Probably the single largest *viewer-side* item.
- **Risk/effort:** high: window layering/DPI/popout compositing, occlusion, and a
  second presentation path to keep correct. Stage it: popout window first (it owns
  a simple rect), console later.
- **Validate:** M4 decode→glass histogram before/after; photodiode-style LED test
  (capture host flashes a rect, viewer camera) for absolute glass-to-glass.

### T2.9 · Damage-metadata pixel grouping — skip work classified by metadata that costs zero pixel reads (nathanfraske, 2026-07-18)

- **Postures:** Balanced/Studio/Studio·Lossless (desktop content); Game benefits
  at menus/HUD-quiet moments.
- **What/why:** the principle: to beat O(n) in practice you don't beat linear —
  you *group pixels by a factor that requires no reads* and give whole groups
  zero compute. The classifier already exists: DXGI duplication hands the SENDER
  compositor-exact dirty rects without touching pixels. Ship them as a few bytes
  of app-layer metadata per frame (datapath — rides the existing sample framing,
  never signaling), and the VIEWER partitions clean/dirty for free: convert
  (NV12→RGBA) and repaint only the dirty union; re-blit the clean remainder from
  the previous frame's buffer. Typing/scroll/cursor workloads dirty 1–5% of a
  frame → effective n drops 20–100× on exactly Studio's content. Composes with
  the AVX2 kernel (`b992776` — convert only dirty bands), with T2.5 (same rects
  steer encoder QP), and with T2.8 (a GPU presenter can scissor to the same
  rects). Sibling forms of the same principle: the GPU dispatch itself
  ("compute all groups simultaneously" — O(n) work, ~O(1) depth) and the
  endgame, NV12 hardware overlay planes (MPO/DirectComposition), where the
  display controller's fixed-function CSC converts at scanout — zero compute
  in any engine we own.
- **Where it lands:** `win_capture.rs` (rects already available at
  `AcquireNextFrame` — currently unread), sample metadata beside the AU
  (app-layer framing, old viewers ignore), `video_decode.rs`/GUI paint slot
  (partial convert + partial `putImageData` — the canvas API takes dirty
  rects natively).
- **Expected win:** viewer convert+paint cost scales with damage, not
  resolution: a 1440p typing session's ~2 ms convert + ~3 ms paint collapses
  to ~0.1 ms steady-state. Also cuts the ~885 MB/s IPC stream proportionally
  (ship only dirty tiles + coordinates).
- **Risk/effort:** medium — the decoder must still decode full frames (the
  bitstream is full-frame), so this optimizes convert/IPC/paint, not decode;
  correctness needs care at IDRs/waves (full repaint on any decode re-entry)
  and encoder-side motion search can dirty more than the compositor reported
  (trust the encoder's OWN skip decisions? no — trust the capture rects, they
  bound what the encoder saw change).
- **Validate:** byte-exact invariant — partial-path output must equal
  full-convert output every frame (the same test discipline as the SIMD lane);
  M4 paint-interval + IPC-bandwidth columns before/after on a typing workload.

---

## Tier 3 — kernel/OS-level (Windows) hyper-optimizations

_Baseline already shipped in `os_perf.rs`: `timeBeginPeriod(1)` guard while
streaming, `THREAD_PRIORITY_ABOVE_NORMAL`, per-thread EcoQoS opt-out
(`ThreadPowerThrottling`), P-core CPU-set preference on hybrid parts; plus the
GPU-side `ClockKeeper` (23.7→14.7 ms — the biggest OS-adjacent win to date)._

### T3.1 · MMCSS for the media threads (`AvSetMmThreadCharacteristicsW`)

- **Mechanism:** register capture/encode/decode/present threads with the Multimedia
  Class Scheduler ("Games" or "Capture" class; "Pro Audio" for the tightest —
  runs threads in the 16–26 priority band under a scheduler-managed quota, far
  above `ABOVE_NORMAL` (9), without the starvation risk of raw
  `REALTIME_PRIORITY_CLASS`).
- **Magnitude (honest):** zero when the box is idle; under real contention (a game
  pegging all cores — exactly Game posture's environment) scheduling-latency tails
  drop from multi-ms to sub-ms. Expect p99 frame-time improvement, not mean.
- **Where:** `os_perf.rs boost_media_thread` (add the MMCSS join, keep the current
  levers as fallback — MMCSS can be disabled by policy). One caveat to **document
  for field boxes**: the classic MMCSS `NetworkThrottlingIndex` (default 10
  packets/ms ≈ ~120 Mbps) throttles network DPCs while MMCSS tasks run — right at
  Studio rates. Field-box doc: set `0xFFFFFFFF` (registry, reboot) when running
  Studio/Studio·LL. That interaction is why MMCSS ships behind a dial.
- **Risk/effort:** low (a day incl. A/B); revert is trivial.
- **Validate:** M1 p99 while a CPU-burner runs; telemetry per-thread CPU line
  confirms the threads still get their time.

### T3.2 · Windows 11 background-process honesty: process-wide power/timer opt-outs + high-res waitable timers

- **Mechanism:** three documented calls. (1)
  `SetProcessInformation(ProcessPowerThrottling)` with
  `PROCESS_POWER_THROTTLING_EXECUTION_SPEED` masked off — the process-wide EcoQoS
  opt-out. Today only *named media threads* opt out; **the pacer's µs gaps run on
  tokio worker threads, which are still throttle-eligible**. (2) The same API with
  `PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION` — since Windows 11, the OS
  ignores `timeBeginPeriod` for processes it classifies background/occluded;
  `allmystuff-serve` is a **windowless sidecar**, the exact shape at risk. Without
  this bit, our 1 ms guard may be a no-op on stock Win11 and every sleep in the
  pipeline quantizes at up to 15.6 ms. (3) `CreateWaitableTimerExW(…,
  CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, …)` for the pacer/pump waits — ~0.5 ms
  precision independent of the global resolution; for the pacer's 100–500 µs gaps,
  a hybrid (waitable sleep to within ~1 ms, then a bounded QPC spin) makes the
  requested gap real for the first time (see M2 — today's `tokio::time::sleep`
  almost certainly rounds every sub-ms gap to ≥1 ms).
- **Magnitude:** on boxes where the timer raise is being ignored, this is the
  difference between designed pacing and ±15 ms chaos — potentially the largest
  single item in this tier. Where it isn't ignored: the waitable-timer hybrid
  still tightens gap error from ~1 ms to ~50–100 µs (µs-class, matters only for
  the drain model's fidelity).
- **Where:** `os_perf.rs` (process-level call at serve start, beside the timer
  guard); a small `precise_sleep(µs)` helper the pacer uses via a
  `spawn_blocking`-style dedicated sender step or a per-route sender thread (the
  send path is per-route serialized already, so moving the gap engine off the
  async runtime is contained).
- **Risk/effort:** low-medium; the QPC spin burns one core for ≤1 ms per wall —
  bounded and only on multi-chunk AUs.
- **Validate:** extend `bench_sleep_granularity` to run with/without each bit,
  foreground vs headless; M2 gap histograms.

### T3.3 · GPU scheduling priority + HAGS posture (contention-proofing the blt and encode submits)

- **Mechanism:** the encode *engine* is separate silicon, but our per-frame
  `VideoProcessorBlt` and the submission path ride WDDM queues that a foreground
  game can flood. `D3DKMTSetProcessSchedulingPriorityClass(…, High)` raises our
  process's GPU scheduling class (Realtime needs privilege — don't); on the same
  theme, document a **HAGS on/off A/B** for field boxes (hardware scheduling
  changes submission latency characteristics per driver generation; evidence in
  the wild is genuinely mixed — measure, don't believe).
- **Magnitude:** zero uncontended; **ms-class under a GPU-saturated game** —
  telemetry's per-engine busy line is precisely the tool to catch the "3d 99%,
  convert stalling" signature (M4/M1 correlation).
- **Where:** `os_perf.rs` (one call at media start, Windows-only,
  `d3dkmthk` FFI); HAGS is documentation only (`docs/`, field-box checklist).
- **Risk/effort:** low code risk; the API is semi-documented — feature-gate and
  fail soft like every other lever in that file.
- **Validate:** run Game posture while a synthetic 3D load (furmark-class) runs;
  compare M1 convert/encode spans and dropped-frame counts with/without.

### T3.4 · Event-driven waits where we currently poll

- **Mechanism:** two known 1 ms-class sleep-polls: the MF async drain
  (`ASYNC_OUTPUT_GRACE` 50 ms window polled at 1 ms sleeps in
  `mediafoundation.rs`) → use `IMFMediaEventGenerator::BeginGetEvent` callbacks
  with an event; and any future fence needs (`ID3D11Fence` +
  `SetEventOnCompletion`) for GPU completion instead of implicit blocking Maps
  (the D3D11VA staging `Map` today blocks the decode thread until the copy
  drains — a fence + deferred map would let picture N's readback overlap N+1's
  submit).
- **Magnitude:** MF rung: up to ~1 ms mean, more jitter than mean (only matters
  when NVENC is absent — the MF rung is the fallback). Decode overlap: ~1–2 ms at
  1440p lossless on the D3D11VA rung.
- **Where:** `mediafoundation.rs` (event model), `d3d11va.rs` (double-buffered
  staging + fence).
- **Risk/effort:** medium for MF (async COM callbacks are fiddly); low for the
  double-buffered staging.
- **Validate:** the existing per-rung decode+copy benchmarks (4.24 / 5.76 ms
  lines) re-run; MF rung e2e latency bench.

### T3.5 · Memory priority and working-set pinning for the media plane

- **Mechanism:** `SetThreadInformation(ThreadMemoryPriority)` to 7 for media
  threads (their pages resist trimming), `VirtualLock` on the recycled convert
  buffers (CPU lane) and the decode staging buffers, plus a modest
  `SetProcessWorkingSetSizeEx` floor. The buffer-recycling work already proved
  demand-zero page churn was a real cost — this is its tail-risk sibling
  (page-outs under memory pressure on loaded field boxes).
- **Magnitude (honest):** ~0 in steady state on a healthy box; prevents
  100 ms-class page-in stalls only on memory-pressured hosts. Insurance, not
  speed.
- **Where:** `os_perf.rs boost_media_thread` + the buffer pool allocation sites in
  `video.rs`.
- **Risk/effort:** trivial code; `VirtualLock` quotas need the working-set floor
  call first, and every call stays best-effort.
- **Validate:** synthetic memory-pressure soak (a hog allocating to ~90% RAM)
  while streaming; count M1 outliers.

### T3.6 · Field-box network + power documentation (app-observable, zero daemon involvement)

- **Mechanism (document, don't code):** a checklist for host/viewer boxes, each
  item with its observable in *our* telemetry/logs:
  - **Wi-Fi background scan / roaming aggressiveness** on viewer laptops — the
    periodic 100–300 ms latency spikes it causes are the single worst "mystery
    stutter" in the field; observable as periodic queue_depth spikes at a fixed
    cadence in the feedback log. (Prefer wired; else set roaming aggressiveness
    lowest, disconnect-time scanning off.)
  - **NIC interrupt moderation / ITR** low or adaptive-off on the viewer (batching
    adds 50–200 µs per burst and lumps our paced chunk trains back together —
    directly visible in M3's arrival-spacing histogram).
  - **RSS on** (spread receive DPCs off core 0, where our decode thread may sit).
  - **Power plan High/Ultimate + USB selective suspend off** (input path) on
    hosts; note the existing `ClockKeeper` already covers the GPU P-state half.
  - **Windows QoS Policy (gpedit) DSCP EF tagging** for the daemon's UDP port —
    OS policy, not daemon code, so it stays inside the frozen-daemon rule; only
    honored on networks that respect DSCP (most home gear ignores it — say so).
  - **`NetworkThrottlingIndex`** if/when T3.1 lands (see there).
- **Magnitude:** individually µs-to-ms; the Wi-Fi item alone is worth the page.
- **Where:** a new section in the field-run checklist doc; telemetry lines already
  provide the before/after evidence.
- **Risk/effort:** an afternoon of writing; zero code.

---

## Measure first — the 3–5 numbers that pick among all of the above

The telemetry seams exist (`StreamStats` 5 s line: scale/encode ms + p95;
`video decode` line: ms/frame; 1 Hz telemetry: per-engine GPU busy, per-thread
CPU). What's missing is the **frame waterfall** — where a frame's 33 ms actually
goes, per posture, per link class:

1. **M1 — end-to-end per-frame waterfall.** Add three spans we don't time today:
   (a) capture-age: duplication `LastPresentTime` → convert start; (b) pace+write:
   total time inside `send_video_paced` per AU, split gap-sleep vs
   `send_video_track` await; (c) viewer glass: `ts_us` → paint (needs only a rough
   clock offset — one control-channel ping/pong at route start, or report deltas
   and trends, which need no sync). Emit p50/p95/p99 on the existing 5 s lines.
   *Decides:* T2.2 (is encode serialization material?), T2.8 (how big is the
   viewer tail?), T3.1/T3.3 (are the tails scheduling-shaped?).
2. **M2 — pacer gap fidelity.** Histogram requested-vs-actual inter-chunk gaps in
   `send_video_paced` (one log line per minute). Hypothesis to kill: tokio rounds
   every 100–500 µs gap to ≥1 ms, so today's real spread is ~3–7 ms per wall, not
   the modeled 1.7 ms. *Decides:* whether T3.2's precise-sleep engine is a
   prerequisite for T2.1's constants or a refinement after them.
3. **M3 — chunk-train arrival spacing at the viewer.** In `handle_video_inbound`,
   per multi-chunk AU: bytes, arrival spread, implied Mbps; log p5/p50 per minute.
   This is simultaneously the validation probe for T1.1 (does dispersion track a
   `clumsy`-imposed rate?) and the T2.1 before/after metric — build it first, it's
   ~20 lines and zero wire change.
4. **M4 — viewer decode→glass split.** We have decode ms/frame; add
   feed-queue-wait (arrival → decoder pickup, exposes `MAX_PENDING` backlog
   behavior), and paint delta (`on_frame`/poke → webview `putImageData` — needs a
   timestamp echo from the GUI, one IPC field). Also paint-interval stddev — the
   smoothness number T2.6 moves. *Decides:* T2.6 vs T2.8 priority.
5. **M5 — the daemon's loss surface (characterization, not code).** With `clumsy`
   loss on the 2-machine rig: when an RTP packet dies, does the app see a missing
   sample, a short sample, or a corrupt AU (and does `rtp_timestamp` continuity
   hold)? One evening of logging at `handle_video_inbound` + the frame-health
   lines. *Decides:* T1.3's parity design (erasure vs error model) and T2.3's
   gap-NACK trigger.

---

## Explicitly rejected — do not re-litigate without new facts

- **Any RTP/RTCP-level mechanism** — packet-level FEC (FlexFEC/RED), RTX/NACK
  retransmission, transport-cc header extensions, RTCP XR, abs-send-time: all live
  inside MyOwnMesh's transport, which is frozen. The app-side analogs are T1.3
  (sample-level parity), T2.3 (feedback-channel NACK), T1.1 (train-based BWE).
- **Transport swap** (QUIC/MoQ, SRT, RIST, WebTransport): same freeze. MoQ's
  relay/fan-out ideas only become relevant if a broadcast/spectate mode ever
  bypasses the daemon — it doesn't today.
- **LL-HLS / LL-DASH shapes** (segment/chunk HTTP delivery): wrong topology —
  they optimize CDN fan-out at 0.5–2 s latencies; we are a 1:1 interactive stream
  two orders of magnitude below that.
- **Daemon socket-buffer / pacing tuning** (`SO_SNDBUF`, sendmsg batching): inside
  the frozen process. The only app-observable proxy is `send_video_track` await
  time — which T2.1(b) uses, legitimately, from our side of the pipe.
- **A raw UDP side-channel** next to the daemon (for probes, FEC, or rescue
  bytes): violates the transport rule's letter (all media on the ICE-negotiated
  path) and its spirit (one path, one NAT story, one security review). Signaling
  stays zero-media, always.
- **B-frames / lookahead / frame-level parallel encode**: `frame_interval_p = 1`
  is load-bearing for latency, LTR, and invalidation; every one of these buys
  compression by holding frames. Off the table in every posture, including Studio
  (its fidelity comes from rate, not reordering).
- **Temporal SVC layers**: pays constant bitrate overhead so a *network element*
  can drop the enhancement layer — but the frozen daemon drops nothing
  selectively, and our source-side freshest-wins already drops at frame
  granularity for free. Rejected until some relay in the path can act on layers.
- **`REALTIME_PRIORITY_CLASS` / hard core affinity**: starves DWM, audio, and
  driver worker threads the pipeline itself depends on (the capture path *is* DWM
  downstream); CPU-set preference + MMCSS (T3.1) capture ~90% of the benefit with
  none of the deadlock-adjacent risk. (Same verdict for `SetThreadAffinityMask`
  pinning — the scheduler wins this argument on hybrid parts.)
- **Undocumented global timer hacks** (`NtSetTimerResolution` to 0.5 ms):
  process-global effects on the whole box, undocumented, and Windows 11's
  coalescing policy ignores it for background processes anyway — T3.2's documented
  opt-out + waitable timers is the supported path to the same place.
- **Viewer-side frame extrapolation/interpolation** (timewarp-style): desktop
  content is text and hard edges — hallucinated in-between frames read as
  smearing precisely where this product promises fidelity. Revisit only ever for
  Game posture, only with real motion hints, probably never.
- **MJPEG/still-image hybrid switching for static content**: the quiesce IDR +
  refinement passes already converge static screens to near-lossless; codec
  flapping would re-key on every transition — strictly worse than what ships.
