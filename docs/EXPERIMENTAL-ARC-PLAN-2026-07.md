# The Experimental arc — a gated Labs tier for latency/smoothness field trials (plan, 2026-07)

_Planning document only — no source was modified for this plan. Repo state
at writing: `main` @ `b992776` (AVX2+NT NV12→RGBA, memchr scan). Companion
docs: `HANDOFF.md` (current state), `docs/SMOOTHNESS-IDEAS-2026-07.md` (the
graded idea bank this arc draws from — T-numbers below refer to it),
`docs/INTEGRATION-REPORT-2026-07.md` (blast-radius + wire-compat rules)._

## 0 · Inherited hard constraints (violating any disqualifies an item)

1. **MyOwnMesh is frozen; signaling carries zero media/metadata.** Every new
   byte in this arc rides the ICE-negotiated datapath on surfaces that exist
   today: the media track lanes' sample framing (`send_video_track` /
   `send_video_paced`), bytes **inside** the AU itself (SEI NALs — the
   bitstream is ours to compose), or additive `#[serde(default)]` fields on
   existing `CHANNEL_CONTROL` messages (`RouteControl::VideoFeedback`,
   `RouteControl::Tune`, `RouteControl::VideoLane`). No new channels, no new
   daemon requests, nothing on signaling. The explicitly-rejected list in
   the idea bank (packet FEC, transport swaps, raw UDP side-channels,
   B-frames, SVC, `REALTIME_PRIORITY_CLASS`…) stays rejected here.
2. **Versioning is Chris's.** Every Labs prototype manifests at his 0.2.46;
   artifacts are `AllMyStuff-PROTOTYPE-<commit>_0.2.46_x64-setup.exe`. Labs
   is **runtime-gated, never build-gated** — one artifact demos everything,
   and no feature ever justifies a version bump.
3. **Fail soft to shipped behavior.** Every feature: off unless the
   Experimental tier is on, carries its own kill switch, and every failure
   path lands on today's exact behavior (the same discipline as
   `ALLMYSTUFF_NVENC=0` → MF rung, `ALLMYSTUFF_PACED_SLICES=0` → single
   write, the D3D11VA soft-fail → bridge re-key).
4. **Wire compatibility, four pairings per feature.** old×old (trivially
   today), old-host×new-viewer, new-host×old-viewer, new×new — each must
   degrade to today's behavior. Each feature section below carries its
   old-peer story explicitly.

---

## 1 · Product shape of "Experimental"

### 1.1 The gate model

One master switch, per-feature dials layered under it, all runtime:

```
ALLMYSTUFF_EXPERIMENTAL=1        # the tier. Unset/0 = everything below is dead code.
ALLMYSTUFF_X_DAMAGE=0|1|strict   # T2.9  damage-metadata grouping (strict = verify mode, §2.1)
ALLMYSTUFF_X_PAINT_PACE=0|1      # T2.6  capture-clock paint pacing (GUI reads it via labs state)
ALLMYSTUFF_X_SUBFRAME=0|1        # T2.2  sub-frame slice streaming
ALLMYSTUFF_X_QPMAP=0|1           # T2.5a damage-QP emphasis
ALLMYSTUFF_X_GRAIN=0|1           # T2.5b adaptive slice grain
ALLMYSTUFF_X_WAVE_STRETCH=0|1    # T2.7  steady-state wave-period stretch
ALLMYSTUFF_X_PRESENT=0|1|fs      # T2.8  zero-copy present (fs = fullscreen-only, the safe stage)
ALLMYSTUFF_X_LTR=0|1             # T1.4  LTR-anchored recovery
ALLMYSTUFF_X_GAP_NACK=0|1        # T2.3  arrival-side loss inference
ALLMYSTUFF_X_RESCUE=0|1          # T2.4  rescue layer
ALLMYSTUFF_X_ENC_ASYNC=0|1       # fence/async encode submit chain
```

Precedence (tri-state, mirrors `ALLMYSTUFF_RATE_ADAPT`'s proven pattern):

- `ALLMYSTUFF_EXPERIMENTAL` unset/0 → **all `X_*` dials ignored**, features
  compiled in but inert. This is the tier's outer wall: a field box with a
  stray `X_` var set behaves exactly like today's build.
- Tier on + `X_FOO` unset → the feature's **curated default** (per-phase:
  a feature graduates to default-on-under-tier only after its phase's exit
  criteria pass; before that, tier-on still requires the explicit dial).
- Tier on + `X_FOO=0` → hard off (the per-feature kill switch).
- Tier on + `X_FOO=1` → on regardless of curated default (A/B).

Implementation seam: a new small module `node/src/labs.rs` (the `os_perf.rs`
pattern: tiny, env-driven, everything best-effort, one `LazyLock` per dial)
exposing `labs::tier() -> bool` and `labs::feature(Feature) -> FeatureState`.
Every call site gates as `if labs::on(Feature::Damage) { … } else { /*
today's path, unchanged */ }` — the else-branch is always the shipped code,
never a re-implementation. One INFO line at serve start enumerates the
resolved state (see §1.5) so every log is self-describing.

GUI face of the same gate: the sidecar env comes from the launcher today;
for demo-friendliness the Labs sheet (§1.2) also toggles features live via a
new GUI-internal `node_control` op `labs_set { feature, on }` (same plumbing
class as `video_feedback` — never wire-visible). `labs_set` writes the same
atomics `labs.rs` owns; env sets the boot state, the sheet overrides at
runtime. Kill semantics: `labs_set(feature, false)` must take effect within
one frame (all consumers read atomics per-frame, the same discipline as
`RouteRate.target`).

### 1.2 The GUI "Labs" affordance

Where it lives: a **`Labs` chip next to `ModePill` in both hosts of the
shared Mode control** — the console strip (`Console.svelte`) and the popout
bar (`gui/src/ui/VideoPopout.svelte`), exactly the `bcd06f8` shared-control
discipline: one new component `gui/src/ui/LabsPill.svelte`, mounted beside
`ModePill` in both bars, **rendered only when the backend reports the tier
is on** (a `labs` field on the existing status plumbing — GUI-internal). A
stock user never sees it; a demo box shows a small flask chip.

Clicking opens a sheet (the `LayersSheet.svelte` visual family):

- One row per feature: name, on/off toggle, and a **live one-line counter**
  fed by a 1 Hz `labs_stats` poll (GUI-internal op): e.g.
  `Damage grouping — dirty 3.8% · convert 0.14 ms · IPC 41 → 3.2 MB/s`,
  `Paint pacing — headroom 3.1 ms · interval σ 9.8 → 1.9 ms`,
  `Sub-frame slices — first-byte 1.1 ms after encode start (was 5.9)`.
  The counters ARE the demo: flip a toggle mid-stream and watch the number
  move. (The user wants to show these off — the sheet is built for
  screen-sharing it next to a live popout.)
- A `copy diagnostics` button that copies the last minute of `labs`-tagged
  log lines (§1.5) — the "send me what happened" affordance for field
  testers.
- Features that are posture-gated (§1.3) render dimmed with the reason
  ("Game only", "needs HEVC route") rather than hidden — discoverability
  without foot-guns.

Demo choreography note: everything in Phase E1 (§4) is **solo-demoable on
one box** — a loopback route (this host's screen → its own popout on a
second monitor, the setup the conformance-crop tests already use). The
sheet's counters make single-box demos legible: dirty-% while typing,
paint-σ while dragging a window, IPC MB/s collapsing when T2.9 flips on.

### 1.3 Posture × feature engagement

Features never override a posture's identity — they refine it. Defaults
when tier + dial are on:

| Feature | Balanced | Game | Studio | Studio·LL | Why |
|---|---|---|---|---|---|
| T2.9 damage grouping | ✔ | ✔ | ✔ | ✔ **(first)** | Pure viewer-side compute cut; lossless is provably exact (§2.1), lossy runs the drift rules |
| T2.6 paint pacing | ✔ | ✖ (slam-immediate is Game's identity) | ✔ | ✔ | Smoothness-for-headroom trade fits quality postures |
| T2.2 sub-frame slices | ✔ | ✔ **(first)** | ✔ | ✖ (32-slice IDRs fine; constQP frames huge — probe later) | Latency overlap; biggest on Game-WAN and Studio walls |
| T2.5a damage-QP | ✔ | ✖ (single-frame VBV already flattens; revisit) | ✔ **(first)** | ✖ (constQP — no rate control to steer) | Quality-per-bit on desktop content |
| T2.5b adaptive grain | ✔ | ✔ | ✔ | ✔ | Burst shape matched to burst size |
| T2.7 wave stretch | — | ✔ (GDR only) | — | — | The wave exists only on Game |
| T2.8 present | ✔ | ✔ **(fullscreen first)** | ✔ | ✔ **(HEVC path leads)** | D3D11VA rung owns a device already |
| T1.4 LTR | ✔ | ✖ (GDR wave is better) | ✔ | ✖ (no loss story on LAN lossless) | The IDR-wall postures |
| T2.3 gap-NACK + matrix | ✔ | ✔ | ✔ | ✔ | Detection is free everywhere |
| T2.4 rescue layer | ✖ | ✔ (WAN, opt-in) | ✖ | ✖ | Game's freeze-vs-blur trade only |
| enc fence/async | ✔ | ✔ | ✔ | ✔ | Host-side, posture-blind |

### 1.4 How peers learn (the experimental handshake, without new messages)

Two additive `#[serde(default)]` fields carry everything:

- **Viewer → host:** `RouteControl::VideoFeedback` gains `caps: u32`
  (default 0 = a peer that predates it). Bits: `1<<0` damage-SEI-aware
  (viewer parses/strips the SEI), `1<<1` rescue-lane-aware, `1<<2`
  rides-frame_num-gaps (the `e382997` invalidate story), `1<<3` LTR-ack
  (`last_good_ts_us` meaningful). Feedback already flows every ~2 s on the
  existing CHANNEL_CONTROL message; a host simply never engages a
  viewer-cooperative feature until it has seen the bit. `VideoFeedback`
  also gains `last_good_ts_us: Option<u64>` for T1.4.
- **Viewer → host intent:** `RouteControl::Tune` gains `labs: u32`
  (default 0) — the viewer's Labs sheet requesting host-side features on
  its route (mirrors how `game`/`mode` already ride Tune additively).

Both are field-additions to messages that already round-trip-test in the
protocol crate — the exact `est_kbps`/`delay_trend_us_per_s` precedent. Old
peer receiving new fields: serde ignores. New peer receiving old messages:
defaults to 0 = everything off. **No new enum variants** (unknown variants
are not tolerated by serde the way unknown fields are — `InputAction` had a
catch-all; `RouteControl` is not assumed to).

### 1.5 Telemetry — making an experimental session diagnosable after the fact

The existing field-log spine stays the source of truth: `video out`
(sender: raw→wire Mbps, scale/encode ms + p95), `pace gaps` (M2 fidelity),
`video in` (M3 chunk-train dispersion + est + delay trend), `video decode`
(wire→nv12→rgba Mbps, ms/frame), `video rate` (every AIMD step), 1 Hz
telemetry (CPU/GPU-engine/VRAM). The arc adds, all under the same logger:

1. **`labs state` one-shot at serve start and on every `labs_set`:**
   `labs state: tier=on damage=on(strict) paint-pace=default subframe=1 …`
   — the first line a post-mortem reads.
2. **Per-feature columns appended to existing lines** (never reformatting
   existing fields — downstream `grep`s keep working):
   - `video out … · dmg 4.2% (8 rects p95) · sei 78 B` when T2.9 emits.
   - `video decode … · partial 93% · full 7% (idr/refine/reset)` viewer-side.
   - `video out … · slices out 8 · first-byte +1.1 ms` under T2.2.
   - `video rate` gains the wave-shape steps of T2.7:
     `wave period 30 → 60 frames (clean 30 s)`.
3. **`labs probe` one-shots** — every driver-dependent feature runs its
   probe at first engage and logs a verdict line
   (`labs probe subframe: reportSliceOffsets=ok subFrameWrite=ok n_slices=8`),
   the `probe_nvenc_av1_lossless` discipline. A probe failure logs the
   named reason and the feature self-kills for the session (fail-soft rule).
4. **`labs guard` events** — every automatic disengage says why:
   `labs guard: damage → full-frame (decoder re-entry)`,
   `labs guard: present → canvas (occlusion)`. Guards are the hiccup
   telemetry: a session that "felt off" reconstructs from guard lines.
5. The Labs sheet's `copy diagnostics` collects exactly lines 1–4 plus the
   spine lines for the last minute.

---

## 2 · Per-feature implementation plans, ordered by (expected win ÷ risk)

Order: **T2.9 → T2.6 → T2.2 → T2.5 → T2.7 → T2.8 → T1.4+T2.3 → enc-fence →
T2.4.** The rig-gated tail (T1.4/T2.3 validation, T2.4) can slide without
blocking the rest.

---

### 2.1 · T2.9 — Damage-metadata pixel grouping (the arc's centerpiece)

**Principle (nathanfraske):** don't beat O(n) by a better loop — group
pixels by a classifier that costs **zero pixel reads** and give whole
groups zero compute. The classifier already exists: DXGI duplication hands
the sender compositor-exact dirty/move rects. Ship them as a few bytes of
app-layer metadata riding the AU itself; the viewer partitions clean/dirty
for free and runs convert/IPC/paint only on the dirty union. Typing,
scrolling, cursor and terminal workloads dirty ~1–5% of the frame — the
effective n of three viewer-side passes drops 20–100× on exactly the
content Studio postures exist for. Composes with `b992776` (the AVX2
kernel now converts dirty bands only), with T2.5 (same rects steer encoder
QP), and with T2.8 (the presenter scissors to the same rects).

**Expected win (math in §3.3):** viewer convert+IPC+paint cost scales with
damage, not resolution — at 1440p/5% dirty, ~7.8 ms of per-frame viewer
work becomes ~0.5 ms, and the ~885 MB/s IPC stream becomes ~45 MB/s.

#### Seams

| Where | What |
|---|---|
| `node/src/win_capture.rs` — `GpuDup::next_gpu_frame` | The frame-held window between `AcquireNextFrame` and `ReleaseFrame` (today reads only pointer metadata). Add: when `info.TotalMetadataBufferSize > 0`, `GetFrameMoveRects` then `GetFrameDirtyRects` into a reusable buffer. Move rects fold into damage as src∪dst. Cursor-only emits already know their damage exactly: `patch_cursor_on_clean` returns the patched rect and `restore_clean(rect)` names the previous one — a moving cursor is a 2-rect frame. |
| `node/src/win_capture.rs` — `GpuFrame` | Grows `damage: Option<DamageMeta>` (`rects: ArrayVec<[Rect; 8]>`, `coverage_pm: u16` per-mille). Coalesce >8 rects or coverage >60% → `None` = full frame (the encoder path is unchanged either way — rects are metadata, never pixels; the blt still converts the whole texture). Rects are scaled through the same `fit_within_even` math the lane uses (src→out), **rounded outward** to even coordinates (4:2:0 chroma pairs). |
| `node/src/video.rs` — `run_gpu_lane` | After `encode_texture`, when the outcome consumed a damaged frame and the encoder did a plain P encode: compose the damage SEI and splice it into the AU **after the last parameter-set NAL, before the first VCL NAL** (position matters — see wire notes). Force `damage = full` on: `force_idr`, quiet-path convergence IDRs, the `REFINE_PASSES` re-encodes (they deliberately re-spend on the *whole* frame — a partial paint would freeze the sharpening out), any frame while a GDR wave is armed/in flight, the first frames after `set_bitrate` reconfigure, and lane rebuilds. |
| `node/src/video_decode.rs` — `run_decode` | Pre-decode: walk the AU's NALs (the `split_annexb_paced` memchr walk, reused), extract our UUID'd SEI, feed the AU onward. Post-decode: partial convert + delta-IPC build (below). |
| `node/src/mesh.rs` — `enqueue_decoded` | Freshest-wins today is `queue.clear()`. Delta packets change the rule: an undrained **delta** packet may not be silently cleared (its rects would never paint and the canvas would hold stale pixels there). New rule: queue up to 4 delta packets; a 5th forces the next packet full-frame. Full packets still clear the queue (they carry everything). |
| `gui/src/tauri.ts` + `VideoPopout.svelte` / `Console.svelte` | `parseVideoPacket` learns kind 4 (delta): header + `n` rect entries `{x,y,w,h}` + concatenated RGBA tiles. Paint slot: `ctx.putImageData(tile, dx, dy)` per rect — the canvas **is** the retained buffer (2D canvases keep their contents), so the GUI needs no back buffer at all. `presentFsr` unchanged — it re-reads the composited base canvas. |

#### Wire & data changes, and the old-peer story

Carrier: **an in-band SEI NAL** — H.264 NAL type 6, `payloadType 5`
(user_data_unregistered, 16-byte UUID + payload), HEVC prefix-SEI NAL type
39 (first byte `0x4E`). Payload ≈ `magic/ver(2) + flags(1) + n(1) +
n×{x,y,w,h as u16} (≤64)` → ~84 bytes at 8 rects, ~20 at 1. This is the
only carrier that is *atomic with its frame*, needs **zero wire-format
changes**, and is **self-gating**: every decoder skips unknown SEIs by
spec.

- Placement is load-bearing: `sniff_codec` judges the AU's **first** NAL
  byte. SEI **after** parameter sets, **before** the first VCL: key AUs
  still lead SPS/VPS (sniff unchanged ✔), delta AUs lead SEI whose byte
  sniffs `None` — exactly what delta AUs sniff today ✔. `split_annexb_paced`
  already glues "parameter-set/SEI runs to the slice that follows" ✔.
- **old-viewer × new-host:** openh264 skips SEI type 6; NVDEC's parser
  eats SEIs natively; WebCodecs ignores them. The one audited risk is our
  own in-house D3D11VA parser (`d3d11va.rs` — scoped to SPS/PPS/slice):
  its NAL walk must *skip* type-39, not error. Audit + a unit test
  (SEI-bearing lossless stream through the rung) is step 1; if the rung
  objects, the host simply doesn't attach SEIs on HEVC routes until the
  viewer advertises `caps & DAMAGE_AWARE` (which also means "I strip it").
  Fail-soft either way.
- **new-viewer × old-host:** no SEI arrives → extractor finds nothing →
  full-frame path, byte-for-byte today's behavior.
- **Bitrate cost:** ~84 B × 60 fps = **40 kbps** ≈ 0.11% of a 35 Mbps
  game stream, 0.03% of Studio. Nothing.

The byte-exact HEVC round-trip tests compare *decoded pixels*, which an SEI
cannot change — they stay green unmodified; a new variant pins that an
SEI-bearing stream still decodes 60/60 byte-exact through both rungs.

#### The correctness core (read this before implementing)

The decoder always decodes **full frames** (the bitstream is full-frame —
T2.9 optimizes convert/IPC/paint, never decode). Partial paint is exact iff
*decoded* pixels outside the rects equal the previous *decoded* frame's.
Three regimes:

1. **Studio·Lossless — exact by construction.** constQP-0 +
   transquant-bypass decodes to the encoder's input bytes (proven 60/60 on
   both rungs). Unchanged capture pixels → identical decoded pixels. The
   partial path can assert equality in tests and **ship first here**.
2. **Lossy P-frames — near-exact, verify then trust.** Static macroblocks
   are normally skip-coded (bit-identical to the reference), but rate
   control *may* re-quantize them. Mitigations, layered: (a) the sender's
   full-flags above remove every *systematic* re-spend (IDR, refine passes,
   waves, reconfigures); (b) `ALLMYSTUFF_X_DAMAGE=strict` runs the full
   convert in parallel and logs the mismatch rate outside rects
   (`labs strict: dmg mismatch 0.00%/frame p95 0.02%`) — the field
   evidence that graduates lossy from strict to on; (c) any decoder
   re-entry (`waiting_key`, codec morph, dumped queue) forces full until
   the next key.
3. **Dropped-paint accumulation — solved structurally.** Damage is
   relative to the *previous delivered frame*; if a delta packet is never
   painted its rects must not vanish. The `enqueue_decoded` rule above
   (bounded delta queue, overflow → forced full) keeps the chain sound
   without rect-set algebra.

#### Implementation steps

1. D3D11VA parser SEI-tolerance audit + test (the one cross-feature
   pre-req). ½ day.
2. Capture-side rects: fetch, coalesce, scale, thread through `GpuFrame`.
   Rect metadata only — zero added pixel touches (the T2.5 risk note in
   the idea bank). 1 day.
3. Sender SEI composer + splice in `run_gpu_lane` + full-flag rules; log
   column. 1 day.
4. Viewer extractor + **stage 1a: partial convert into a persistent
   per-route RGBA buffer, full-size IPC** (the buffer is always complete —
   no accumulation hazard; win = convert only). Byte-exact test: partial
   path output == full-convert output on synthetic damage sequences —
   the `simd_lane_matches_scalar_byte_for_byte` discipline. 2 days.
5. **Stage 1b: delta IPC (kind 4) + `putImageData`-per-rect paint** +
   the bounded-delta-queue rule in `enqueue_decoded` + GUI compositing.
   The invariant test moves up a level: canvas-model (a test harness that
   replays packets into a software canvas) == full-frame reference every
   frame, including forced drops. 2–3 days.
6. `strict` mode + counters + Labs sheet row. 1 day.
7. Lossless default-on under tier; lossy graduates on strict-mode field
   evidence (Phase E1 exit, §4).

#### Hiccups → detection → kill

| Hiccup | Detection | Response |
|---|---|---|
| Encoder-vs-compositor divergence (lossy drift outside rects) | `strict` mismatch counter; user report "stale smudge" | Auto: full-flag rules already exclude systematic cases; guard escalates route to full-frame for 10 s on any decode-fail. Kill: `X_DAMAGE=0` (per-route full-frame is one atomic read away) |
| D3D11VA parser rejects SEI | `labs probe damage-hevc: parser=reject` at first HEVC engage | Host omits SEI on HEVC routes (caps-gated re-enable) |
| Rect storm (games, video playback) | coverage >60% or >8 rects | Coalesce to full — the path is self-defeating there by design, cost is the 84 B |
| Stale pixels after resolution change / rotation | lane rebuild event | Rebuild forces full (step 3 rule); test pins it |
| GUI paints delta before any full frame | first-packet state machine | Node never emits kind 4 before a kind 3 full has been built for the route |

#### Validation

- Unit: SEI splice/extract round-trip; rect scaling (odd sizes, edges);
  partial==full byte-exactness (stage 1a); canvas-model==reference
  (stage 1b); SEI-bearing lossless byte-exact through NVDEC **and** D3D11VA.
- Bench: extend the decode bench to a typing-trace corpus (recorded damage
  sequences); assert convert ms scales ~linearly with dirty fraction.
- Field: `video decode` line's new `partial %` + rgba-Mbps column
  before/after on a typing workload; M4 paint-interval histogram; the Labs
  sheet counter is the demo.

---

### 2.2 · T2.6 — Capture-clock paint pacing at the viewer

**What/win:** the popout paints on arrival (`watchVideo` poke → immediate
`putImageData`), so displayed cadence inherits network+decode jitter —
micro-stutter on smooth motion even with zero loss. PLL the sender's
capture clock against local time and schedule each frame's paint at
`recovered_capture_time + headroom`; headroom auto-tunes to p95 jitter
(~2–8 ms), late frames drop (freshest-wins preserved). Displayed-interval
stddev collapses toward capture stddev for ≤ +5 ms mean latency. The
largest pure-*smoothness* item for Studio scrolling/panning.

**Seams:** entirely TypeScript — no host change, no wire change, no node
change. The timestamp already crosses the IPC boundary: `raw_ipc_packet`
writes the AU's `ts_us` into the 28-byte header at offset 20, which
`parseVideoPacket` (`gui/src/tauri.ts`) already surfaces as `f.seq`. The
scheduler lives in the paint slot of `VideoPopout.svelte` (first) and
`Console.svelte` (after): hold at most **one** pending frame
(`pendingFrame` + its target time), paint via a rAF-aligned `setTimeout`,
supersede on newer arrivals.

PLL: `offset = min-filtered (arrival_ms − seq/1000)` over a 2 s window
(min-filter absorbs decode/IPC spikes; drift term from the window's slope);
`headroom = clamp(p95(arrival − predicted), 2, 12) ms`, decaying during
damage-driven quiet spells and re-learned on motion (the idea bank's
explicit caveat — a quiet sender must not leave a stale 12 ms fee on the
first motion frame). Sender fps changes need no signaling: the PLL sees
only timestamps.

**Posture gating:** engaged for Balanced/Studio/Studio·LL; Game keeps
slam-immediate (its identity). The GUI knows the route's mode already
(the ModePill state) — no new plumbing.

**Old-peer story:** none — GUI-local. Four pairings trivially degrade
(feature off = today's paint).

**Steps (2–3 days):** (1) PLL + scheduler behind `X_PAINT_PACE` with the
one-frame hold; (2) paint-interval histogram counter (the M4 number) in
the 1 s fps timer that already exists; (3) Labs row + σ counter; (4)
Console after popout proves out.

**Hiccups → detection → kill:** mislock on quiet streams (headroom decay +
"paint immediately if queue empty and frame older than headroom" floor);
double-buffering against the 16 ms poll interval (the poke path in
`watchVideo` already beats the timer; scheduler works on arrival stamps,
not poll stamps); tab-throttled `setTimeout` in occluded windows (the
existing poke keeps draining; scheduler clamps lateness to paint-now).
Kill: toggle → paint-on-arrival, one atomic.

**Validation:** M4 paint-interval σ before/after at 60 fps scroll under
`clumsy` ±10 ms jitter (2-box), and solo with a synthetic
`setTimeout`-jittered local route; blind A/B on the popout (the sheet's σ
counter makes it visible). Mean-latency cost asserted ≤ headroom+2 ms.

---

### 2.3 · T2.2 — Sub-frame slice streaming (send slice 0 while slice 7 encodes)

**What/win:** the pipeline is frame-granular: `encode_texture` returns the
whole AU (measured: P2 ≈ 5.7 ms @1440p, studio P5 ≈ 12.9 ms, ~2.25× at
4K), and only then does `send_video_paced` start spreading. NVENC's
sub-frame readback streams slices as they complete. Since lossy ≥1080p
sessions already encode **every frame** as 8 count-mode slices
(`slice_mode=3`, `slice_mode_data=8` — set at init for the pacer's cut
points), and both the pacer framing and the receive side already treat an
AU as several same-timestamp samples, the receive side needs **nothing**:
we overlap the encode tail with wire time instead of serializing them.

Overlap win = `min(E, S) × (1 − 1/n)` (E encode, S pace/wire spread, n
slices): Game-WAN steady state (E 5.7, S ≈ 11 ms, n 8) ≈ **−5.0 ms every
frame**; LAN game ≈ −1.1 ms; Studio-WAN keyframes ≈ −5 ms; 4K studio walls
up to ~−11 ms. §3.3 shows the arithmetic.

#### Seams

- `node/src/nvenc.rs`:
  - `InitializeParams.flags` — the transcription already reserves the
    packed bitfield word after `enable_ptd`; per the staged ffnvcodec
    n12.0.16.0 header its bit 0 is `reportSliceOffsets`, bit 1
    `enableSubFrameWrite`. Setting both is the init half. **Probe first**
    (`probe_nvenc_subframe`, `#[ignore]`d like `probe_nvenc_av1_lossless`):
    drivers vary; the probe verifies init acceptance, that
    `LockBitstream.slice_offsets` (already transcribed, currently null)
    fills when pointed at a `[u32; 32]`, and measures slice-availability
    timing vs whole-AU lock on this box.
  - `encode_texture` grows a sibling `encode_texture_streamed(nv12,
    force_idr, sink: impl FnMut(SliceChunk))`: submit `encode_picture`,
    then poll `lock_bitstream` in incremental mode (lock with
    `slice_offsets` set; emit `data[prev_off..off_i]` as slices land; the
    final lock closes the AU exactly as today). Poll cadence: the
    `precise_sleep(200 µs)` engine — the encode thread has nothing else to
    do in that window today (it blocks inside the same lock).
- `node/src/video.rs` — `run_gpu_lane`/`packetize_units`: emit units
  per-slice with a "same-ts continuation" form — non-final slices
  `duration_us = 0`, final slice carries the AU's duration — **exactly the
  framing `send_video_paced` already writes for chunks** (verified against
  the daemon's `H264AuAssembler`: emit-per-marker, contiguity anchor spans
  split writes). The pacer path collapses: a slice ≤24 KB ships whole; the
  drain-model gap runs between slice sends instead of chunk sends
  (`route_pace`'s `(game, wan, rate_bps)` unchanged).
- Byte-exactness invariant: concatenated streamed slices **must equal** the
  whole-AU encode byte-for-byte (same session, same inputs). This is
  T2.9-grade discipline and is directly testable: run the same texture
  sequence through `encode_texture` and `encode_texture_streamed` on two
  sessions with identical config and compare — plus the cheaper in-session
  invariant: `Σ slice lens == bitstream_size_in_bytes` every frame.

#### Old-peer story

The wire shape is indistinguishable from today's paced chunks (same-ts
samples, marker on final). Old viewer: reassembles per marker → decodes the
same AU. New viewer: the D3D11VA rung's picture assembly (first-slice flag |
ts change | learned-count ratchet) was *built* for per-slice samples;
openh264 accepts per-slice feeds (it already receives pacer chunks). Old
host: nothing changes. **No handshake needed.**

#### Steps

1. Probe test + verdict line (½ day; run on this box, later the Radeon —
   AMF has no sub-frame analog, so the ladder's MF/AMF rungs simply never
   engage the dial: `labs::on(Subframe) && matches!(enc, GpuCodec::Nvenc)`).
2. `encode_texture_streamed` + slice-offset poll loop + invariant checks
   (2 days — the pump's outcome seam is the risky edit; `EncodeOutcome`
   stays the fallback shape and streaming is a parallel arm, so the
   fail-soft path is "stop streaming, next frame whole-AU").
3. `run_gpu_lane` emission + pacer interplay + `first-byte` log column
   (1 day).
4. Soak behind `X_SUBFRAME` (the `ALLMYSTUFF_PACED_SLICES` history repeats:
   env-gated until soaked, then default-on-under-tier).

#### Hiccups → detection → kill

| Hiccup | Detection | Response |
|---|---|---|
| Driver reports offsets late/never (readback granularity is driver-internal) | probe timing; runtime watchdog: first slice not available within E_est → fall back | Session flag flips to whole-AU mode; `labs guard: subframe → whole-AU (driver)` |
| Torn AU on mid-stream error (encoder error after k slices sent) | `encode_picture`/lock status | Already survivable: a partial AU on the wire decodes as a partial picture — "no worse than the same bytes lost on the wire" (`send_video_paced`'s documented mid-unit failure semantics); the heal path (`gpu_heal` → MF rung) also drops streaming |
| Poll loop burns CPU | per-thread CPU telemetry line | Poll at 200 µs only while a frame is in flight (≤ encode-time per frame); it replaces a blocking wait, net-zero |
| Interaction with T2.5b per-frame slice counts | invariant test matrix | Grain sets slice count per-picture *before* submit; streamed lock reads `num_slices` fresh per frame |

#### Validation

M1's new span is the acceptance number: encode-start→first-byte-to-daemon
drops from ≈E to ≈E/n (log column). Existing chunk-decode and round-trip
tests pin decodability; the new equality test pins byte-exactness; the M3
`video in` line shows the WAN overlap (train spread starts earlier relative
to ts). 2-box rig confirms the −5 ms on a `clumsy` 40 Mbps path.

---

### 2.4 · T2.5 — Damage-driven QP emphasis (a) + adaptive slicing grain (b)

**What/win:** two consumers of T2.9's rects on the **encoder** side.
(a) NVENC accepts a per-picture macroblock QP-delta map: bias bits into
dirty rects (where the eye is), out of static regions — crisper text at
equal rate; on WAN, equal quality at fewer bits = smaller AUs = shallower
queues (quality-per-bit *is* smoothness there). (b) The slice count is a
per-session constant today (8/32/16/4 by size/lossless); NVENC's per-pic
params can override per frame: a scene-cut frame leaves as 16 slices
(finer pacer grain under the WAN budget, more T2.2 overlap), a 10 KB delta
stays 1 slice (no CABAC-reset tax when there's nothing to shape — the
measured cost reference: ~1–3% for 32 slices on lossless).

#### Seams (all `node/src/nvenc.rs` + `run_gpu_lane` glue)

- **FFI honesty first — this is the feature's real risk.** `PicParams`
  transcribes the pre-union layout exactly and deliberately oversizes the
  codec union + tail as zeroed filler ("the driver reads zeros at its own
  offsets inside our larger buffer"). `qpDeltaMap`/`qpDeltaMapSize` live in
  the **post-union tail** — using them requires transcribing the true
  union size and the tail fields up to `qpDeltaMapSize` exactly, with the
  same `const _: () = assert!(size_of…)` discipline, from the staged
  header. Until that's done the map cannot be set safely. Budget a
  half-day of transcription + a `probe_nvenc_qpmap`: (i) all-zero map ⇒
  `NV_ENC_SUCCESS` **and byte-identical output to no-map** (the invariant
  that proves the offsets are right); (ii) a ±4 checker map ⇒
  `LockBitstream.frame_avg_qp` (already transcribed) moves the right way.
  `RcParams` needs `qpMapMode = NV_ENC_QP_MAP_DELTA` (delta mode works on
  every RC mode; the *emphasis* enum is CBR-gated on some drivers — use
  delta, sidestep the gate).
- Map build: 1440p = 160×90 MBs = 14.4 KB `i8`/frame, CPU-written from the
  ≤8 rects (memset base, fill rects) — µs-class, allocation-free (reused
  buffer). Dirty MBs −2, static +2 (start conservative; A/B ±4). Quiet
  frames (no damage meta) send no map.
- Grain: per-pic `slice_mode/slice_mode_data` live in the **codec union**
  at header-order u32 offsets (the `H264_PIC_FORCE_INTRA_REFRESH_IDX = 4`
  precedent — same union, different indices, pinned by the probe, with the
  `sliceModeDataUpdate` bit set in the pic-flags word). Decision function
  beside `split_annexb_paced`'s call site: predicted-large frame (damage
  coverage > 40% or force_idr) → 16; tiny delta (coverage < 2%) → 1;
  else session default 8.

**Old-peer story:** none on the wire — both are encoder-side; the output
is conformant H.264 either way. Slice-count variation is already something
every receive path handles (the D3D11VA learned-count is a **max-only
ratchet** — important interplay: it learns 16 and then closes pictures on
first-slice/ts-change for shorter frames, which is exactly its documented
close logic; the grain test must cover the rung).

**Hiccups → detection → kill:** wrong tail offsets = driver reads garbage
(caught by probe (i) before any live use — the feature never engages
without the probe's `labs probe qpmap: ok`); per-driver map rejection
(probe); QP oscillation shimmer on rect edges (bound deltas ±4, one MB of
feathering); grain thrash (hysteresis: change slice count at most 1×/s).
Kill: `X_QPMAP=0` / `X_GRAIN=0`, both read per-frame.

**Validation:** SSIM-on-text corpus at fixed rate, map on/off (the honest
subjective-win check the idea bank asks for); `video out` wire-Mbps at
equal SSIM; M3 arrival spread on scene-cut frames grain on/off; the probe
byte-identity test; rung round-trips at 1/8/16 slices.

---

### 2.5 · T2.7 — Steady-state wave-period stretch via in-place reconfigure

**What/win:** the loss-aware wave **length** shipped (`arm_wave(frames)`,
3-frame heal on a lossy spell). The wave **period** is still the init
constant `intra_refresh_period = (fps/2).max(15)` — a clean link pays the
intra tax every 30 frames forever. Close the other half of the loop:
sustained-clean links stretch the period (30 → 60 → 120 frames, i.e. waves
every 0.5 → 1 → 2 s), any loss report snaps back to 30 and re-arms the
3-frame heal. Steady-state intra overhead drops ~4× on clean links
(≈ −5% effective bitrate at equal quality, §3.3); lossy links keep today's
shape exactly.

**Seams:** `nvenc.rs` grows `set_wave_shape(period: u32, cnt: u32) ->
bool` — the `set_bitrate` twin: mutate `ConfigH264.intra_refresh_period/
intra_refresh_cnt` in `self.config`, `reconfigure_encoder` in place. The
chooser lives beside the loss window `route_wave_or_refresh` already
maintains (`loss_marks`, 10 s window): 30 s clean → step up; any mark →
snap down. Plumb like the rate seam: a per-route atomic in `RouteRate`
(one more field) the encode thread applies next frame.

**Probe first:** `probe_nvenc_wave_reconfig` — some driver generations
reset the GDR phase or demand an IDR on intra-refresh reconfigure. The
probe encodes across a reconfigure and asserts: no IDR emitted
(`LockBitstream.picture_type`), stream stays decodable (openh264), wave
SEIs keep arriving (recovery-point SEI presence — frame health already
parses these). Reject ⇒ feature self-kills with a verdict line.

**Old-peer story:** none — sender-side only; the stream stays conformant
GDR H.264. Old viewers see fewer waves, which they never counted anyway.

**Hiccups:** reconfigure races the AIMD `set_bitrate` (both mutate
`self.config` — serialize through the encode thread, which already owns
both applications; never reconfigure twice in one frame); stretch-then-
loss leaves a long dirty window (snap-down also `arm_wave(3)` immediately
— heal now, not at the next long period). Kill: `X_WAVE_STRETCH=0` pins
today's constants.

**Validation:** extend `nvenc_intra_refresh_replaces_idr_walls` with
per-shape byte/frame profiles at periods {30, 60, 120}; field: `video rate`
wave-shape lines + frame-health timestamps loss→recovery-SEI stay ≤ the
3-frame heal after snap-down. Solo-demoable (bitrate delta visible in
`video out` raw→wire on a static screen).

---

### 2.6 · T2.8 — Viewer zero-copy present (staged; the honest compositor story)

**What/win:** the native viewer's post-decode path exists only because the
presenter is a webview canvas: decoded NV12 → staging `Map` (the D3D11VA
rung's readback at `d3d11va.rs:1364–1397`) → CPU NV12→RGBA (1.8 ms avg
@1440p after `b992776`) → 14.7 MB IPC packet through the webview boundary
(≈ 885 MB/s at 60 fps) → canvas `putImageData` (another copy + GPU
upload). A D3D11 swapchain fed the decoded texture through the same
`ID3D11VideoProcessor` we already drive host-side deletes all four in one
move — **the pass-deletion endgame the CPU-kernel line named**: the AVX2
kernel's true successor is not a faster kernel but no kernel.
Expected −3…−8 ms mean glass latency and the largest jitter cut on the
viewer (M4 will show today's spread), ~1.9 GB/s of memory traffic gone.

**The process-boundary fact that shapes the design:** decode lives in
`allmystuff-serve` (sidecar); windows live in the Tauri GUI process.
Cross-process `SetParent` child HWNDs are a documented minefield —
**rejected**. The right shape: **decode stays in serve, present moves to
the GUI process, pixels cross as a shared texture handle, not bytes.**

- Stage A (popout, the first target): `d3d11va.rs`'s device copies the
  decoded frame into a small ring of `D3D11_RESOURCE_MISC_SHARED_NTHANDLE`
  NV12 textures (keyed-mutex sync). The handle set is passed once over the
  existing GUI-internal control plumbing (`node_control` — never the wire).
  `gui/src-tauri` (Rust, in-process with the window) opens the handles on
  its own D3D11 device, creates a child HWND over the popout's video rect
  (the window handle is Tauri-native), and runs
  `CreateSwapChainForHwnd` + `VideoProcessorBlt` NV12→RGBA →
  `Present`. `DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT` +
  `SetMaximumFrameLatency(1)`; `Present(0)`+allow-tearing under Game,
  `Present(1)` otherwise (posture-appropriate).
- The canvas path **stays fully wired** as the fallback and for
  occlusion/screenshots — the presenter is a sink beside `on_frame` in
  `video_decode.rs` (a "present in place" arm), not a replacement; the IPC
  stream can idle at 1 fps thumbnails while presenting (keeps `hasFrame`,
  `norm`'s content-box math, and the FSR overlay decision honest).
- **Overlay controls are the real compositor fight, named honestly:** a
  child HWND swapchain sits **above** the WebView2 surface, so the
  popout's hover controls (ModePill, pills, fullscreen button) would
  vanish over the video. Two staged answers: **A-fs** (`X_PRESENT=fs`,
  the safe demo): engage only in fullscreen, where controls are hidden
  anyway and the win is largest (fullscreen game latency) — zero UI
  compromise, ships first. **A-windowed**: Tauri window `transparent:
  true` + WebView2 `DefaultBackgroundColor` alpha 0, swapchain HWND placed
  *bottom* of the window's child z-order, webview above it rendering
  transparent where the stage is — HTML controls composite over GPU video.
  This is a known-working WebView2 pattern but per-version fragile; it is
  the stage's *experiment*, behind its own sub-dial, with the guard
  falling back to canvas on any compositional anomaly we can detect
  (window style changes, DPI change events, presentation stats stalls).
- **NVDEC routes:** stage A leads with the D3D11VA rung (it already owns a
  D3D11 device and its surfaces — `ALLMYSTUFF_HEVC_DECODER=d3d11va` pins
  it for presenter demos). NVDEC→D3D11 interop (CUDA
  `cuGraphicsD3D11RegisterResource` into the shared ring) is a follow-up;
  H.264/openh264 routes get a cheap upload path (CPU I420→shared NV12
  write — still deletes IPC+canvas, keeps one convert) or simply stay on
  canvas in stage A.
- **Stage B (endgame, documented not scheduled): NV12 hardware overlay
  planes (MPO).** `CreateSwapChainForComposition` with an NV12 swapchain +
  `IDXGIOutput3::CheckOverlaySupport` → the display controller's
  fixed-function CSC converts at scanout — zero compute in any engine we
  own (T2.9's sibling principle taken to silicon). Honest constraints:
  windowed MPO needs a DirectComposition visual tree we own; Tauri/wry
  uses WebView2 **windowed** hosting (the webview owns its rendering),
  so true DComp composition requires either wry's visual-hosting mode
  (`CoreWebView2CompositionController` — not exposed today) or a
  dedicated fullscreen native window; MPO availability is
  driver/panel-dependent and NVIDIA has shipped MPO-off periods. Stage B
  is therefore: measure `CheckOverlaySupport` on field boxes via a probe
  line now, build only if stage A's numbers justify it.

**Old-peer story:** none on the wire — presentation is viewer-local.
Four pairings unaffected.

**Hiccups → detection → kill:** z-order/occlusion anomalies (guard:
`DXGI_PRESENT` stats + window events → canvas fallback within one frame,
`labs guard: present → canvas (reason)`); DPI change mid-stream (recreate
swapchain on `WM_DPICHANGED`-equivalent Tauri event; content-box math
reuses `norm`'s fit — already inset-exact); fullscreen transitions (the
popout already refits via ResizeObserver — presenter subscribes to the
same signal); keyed-mutex deadlock on process death (serve watchdog:
mutex acquire with timeout → drop presenter, canvas resumes); GPU device
mismatch (multi-adapter viewer: open shared handle on the adapter that
owns the output; else fall back). Kill: `X_PRESENT=0` and the automatic
guard. Blast radius contained to the popout window in stage A.

**Validation:** M4's decode→glass split before/after (the histogram is
the acceptance artifact); presentation-stats jitter
(`DXGI_FRAME_STATISTICS`) vs canvas path; the photodiode/LED
glass-to-glass spot-check the idea bank names; soak across fullscreen
toggles, DPI changes, monitor hops, occlusion. Solo-demoable (popout on
the second monitor).

---

### 2.7 · T1.4 + T2.3 — LTR-anchored recovery, the recovery matrix, and gap-NACK

Grouped: one is the mechanism, the other the chooser + earlier trigger.
**Both need the 2-machine rig for validation** (loss injection), and the
matrix's short-train trigger needs **M5's loss characterization first**
(what the daemon delivers under RTP loss: missing sample vs short sample
vs corrupt AU). Buildable solo; graduation is rig-gated.

**T1.4 LTR (Balanced/Studio — the IDR-wall postures):**

- Win: loss recovery cost drops from a ~190 KB IDR wall (which risks
  re-loss — all-or-nothing) to a ~1× P-frame referencing a known-good
  anchor; recovery latency from "next 2 s cadence or explicit re-key RTT"
  to one frame after the report. The deep DPB (`max_num_ref_frames = 8`)
  finally earns its keep; frame_num stays continuous so openh264/WebCodecs
  accept it (the `e382997` finding's conformant sibling).
- Seams: `nvenc.rs` config — `ConfigH264.ltr_num_frames` (=2) +
  `ltr_trust_mode` are **already transcribed**; per-picture the H.264
  codec-union words carry `ltrMarkFrame`/`ltrUseFrames` bits and
  `ltrMarkFrameIdx`/`ltrUseFrameBitmap` at header-order offsets (same
  union as `H264_PIC_FORCE_INTRA_REFRESH_IDX` — pin with
  `probe_nvenc_ltr`, which must also answer the documented hazard:
  several SDK generations make **LTR and intra-refresh mutually
  exclusive** — fine, LTR is for the non-GDR postures anyway).
  `LockBitstream.ltr_frame_idx/ltr_frame_bitmap` (transcribed) confirm
  marks landed. Mark cadence ~1/s from `run_gpu_lane`; on
  `lost_ts_us` + `last_good_ts_us` (the new serde-default feedback field,
  §1.4), encode the next frame with `ltrUseFrames` against the newest
  anchor ≤ last_good. Anchor older than 2 s → today's IDR (exactly the
  shipped floor).
- Old-peer story: old viewer never sends `last_good_ts_us` (default
  `None`) → LTR never engages → today's IDR path. Old host ignores the
  field. The *stream* is conformant H.264 either way.
- Validation: rig — inject loss, assert log `LTR re-anchor (age 0.4 s)`
  with no IDR emitted, viewer heals ≤2 frames, zero decode-fail feedback
  after heal on openh264 **and** WebCodecs viewers; unit: probe +
  mark/use round-trip on this box's silicon.

**T2.3 recovery matrix + gap-NACK:**

- The chooser, one function beside `route_wave_or_refresh`:
  `wave if GDR else LTR if (probe-ok && last_good fresh) else
  invalidate_ref if (caps & RIDES_GAPS) else force_idr` — making the four
  shipped/planned heal mechanisms an explicit per-route policy
  (`invalidate_ref` + `EncodeOutcome.input_ts` already ship; NVDEC rides
  frame_num gaps natively, and the caps bit lets a future D3D11VA/H.264
  arm advertise it).
- Gap-NACK: today a loss is noticed when decode fails. The viewer can
  know earlier — but honestly: with a damage-driven sender, RTP-timestamp
  arithmetic can't distinguish "no frame existed" from "frame lost", so
  the **whole-AU** trigger is unreliable *except* where structure exists:
  (a) a **short chunk-train** (train opened with N−1 chunks then a new ts
  — M5 must confirm short trains are what loss produces), and (b) the
  D3D11VA rung's `learned_slices` closing a picture short (the rung
  already knows its expected slice count — the precedent to copy). Fire
  `send_video_feedback(lost_ts_us = inferred)` on either, one AU early ≈
  one RTT of heal latency saved (25–80 ms on WAN). Debounce by one sample
  (the daemon's assembler delivers ordered samples per lane; M5
  verifies).
- Old-peer story: `lost_ts_us` already ships (serde-default); an old host
  receiving an early report treats it exactly like today's decode-fail
  report. No new surface.
- Steps: matrix function + caps bits (1 day, solo); gap-NACK triggers
  behind `X_GAP_NACK` (1 day) — **enabled only after M5**; rig A/B
  measuring artifact-visible ms per loss event with the heal-mechanism
  tag added to frame-health lines.

---

### 2.8 · NVENC input-fence-chained submit (probe-first; honest small win)

**What:** today's convert→encode handoff relies on D3D11 same-device
implicit ordering: the capture thread queues `VideoProcessorBlt`
(`GpuConvert::convert`), the encode thread later submits
`encode_picture` on the same multithread-protected device, and the driver
serializes. That's *correct*; the questions are (a) whether the implicit
path costs CPU time under contention (the `ID3D11Multithread` critical
section + driver-internal flushes when a game floods WDDM queues), and
(b) whether the sync-mode `lock_bitstream` block leaves submit-side
overlap on the table. The NVENC-API fence field
(`RegisterResource.p_input_fence_point`, transcribed and null today) is
**D3D12-typed in SDK 12.0** — so for our D3D11 sessions the honest
mechanism is D3D11's own: `ID3D11Device5::CreateFence` +
`ID3D11DeviceContext4::Signal` after the blt, `Wait` before the encode
submit, making the dependency explicit and letting the encode thread
submit without touching the shared immediate-context lock window.

**Expected win (stated plainly):** ~0 mean uncontended; **0.3–1 ms p95**
under a GPU-saturated game (the M1/M3.3 "3d 99%, convert stalling"
signature) — a tail item, not a headline. Bundled here because T2.2's
streamed lock loop wants the same restructuring anyway.

**Plan:** (1) measure first — M1 spans (encode-call p95 while a
furmark-class load runs) with/without an explicit `ctx.Flush()` after the
blt: if flush-after-blt alone kills the tail, ship *that* one line and
stop; (2) fence Signal/Wait chain behind `X_ENC_ASYNC` with device-lost
guards (fence wait failure → drop to implicit path); (3) optional third
step, only if T2.2 lands: double bitstream buffers + submit N+1 before
locking N (needs `enable_encode_async=0` kept, purely reordered CPU
calls), which hides the ~0.2–0.5 ms CPU gap between lock and next submit.
Fail-soft at every step to today's exact call order. No wire, no
old-peer story. Validation: M1 p95 columns under synthetic load, and the
existing e2e tests (the chain must be behavior-invisible).

---

### 2.9 · T2.4 — Speculative rescue layer (Game-over-WAN, opt-in; most speculative, planned last)

**What/win:** a second tiny NVENC session (~480p, 2 Mbps, IDR every 15) fed
from the same retained NV12 textures, shipped on a second track lane; the
viewer paints it **only** during a heal window (post-loss, pre-wave-
completion). Freezes become blur dips; artifact-visible ms per loss event
collapses to ≈ one rescue frame's decode+paint (~3–5 ms). This is
Parsec/GFN's "never freeze, degrade" feel without SVC.

**Seams + budget realities (the honest part):**

- Encode: `run_gpu_lane` opens a second `GpuCodec::Nvenc` at 480p on the
  same device; the depth-2 retirement already guarantees texture liveness.
  Cost: one more session (GeForce cap is 8 system-wide), ~1–2 ms GPU at
  480p (telemetry's enc-engine line verifies headroom), +2 Mbps.
- **Lane budget is the real constraint:** `assign_video_lane` pins the
  lowest free lane per peer from `effective_video_lanes` (= the daemon's
  pool size, `daemon_lanes`). A rescue lane per game route halves the
  usable stream count on small pools. Rule: rescue takes a lane **only if
  ≥2 lanes would remain free** (multi-monitor setups keep their lanes;
  the pool arithmetic runs at the same pin-lock site).
- Wire/gating: the lane binding rides the existing
  `RouteControl::VideoLane` message + a serde-default `purpose:
  Option<String>` ("rescue"). **Hard-gated on the viewer's
  `caps & RESCUE_AWARE`** — an old viewer would bind the lane to the same
  route and feed 480p IDRs into the main decoder mid-stream (glitch
  storm). No caps bit seen ⇒ the host never opens the session. That makes
  all four pairings safe: old viewer never advertises; old host never
  opens; new×new negotiates.
- Viewer: a second `DecodeBridge` route key (`route#rescue`), paint
  arbitration in the GUI paint slot: rescue frames paint only while the
  main stream is `waiting_key`/healing (the bridge already knows;
  surface it in the IPC header's spare flag byte), always superseded by
  the first clean main frame.
- Steps: prototype entirely behind `X_RESCUE` on the rig; measure
  freeze-seconds/min and subjective side-by-side capture before any
  productization talk. **Rig-gated end to end** — there is no solo demo
  that means anything here.
- Hiccups: lane starvation (rule above; detection = lane-pin log),
  arbitration flapping (hysteresis: rescue paints for ≥100 ms once
  engaged), bandwidth in exactly the congested moment (+2 Mbps *inside*
  the pacer's spread — rescue AUs ride `send_video_paced` at the same
  drain model, never after it), session-cap exhaustion (open failure ⇒
  feature self-kills for the route). Kill: `X_RESCUE=0` closes the
  session and frees the lane (RAII like `RateReg`/`WaveReg`).

---

## 3 · Calculated speedups — the arithmetic

### 3.1 Assumptions and sources

Measured on this box (dev, RTX Ampere, from the repo's own docs/benches —
marked **[M]**); derived/estimated values marked **[E]** with the scaling
rule stated. M1/M4 spans (Phase E0) exist to replace every [E] with a
measurement; the honest-case columns below only bank wins whose mechanism
is proven on this box today.

- Encode: NVENC P2 game **5.7 ms @1440p [M]**; studio P5 **12.9 ms [M]**;
  scale ∝ pixels: ×0.56 @1080p, ×2.25 @4K [E]. (ClockKeeper context:
  boost-clock state assumed held — it is, while streaming [M].)
- Decode+copy @1440p HEVC: NVDEC **4.24 ms [M]**, D3D11VA **5.76 ms [M]**;
  openh264 H.264 @1440p **~7 ms [E]** (unmeasured — M4 pins it).
- NV12→RGBA convert @1440p: **1.8 ms avg / 2.2 p95 [M]** (AVX2+NT);
  openh264's I420→RGBA `write_rgba8` **~3 ms [E]**.
- IPC: 2560×1440×4 = **14.7 MB/frame [M-arith]** ≈ 885 MB/s @60; transfer+
  parse cost **~2–4 ms [E]** (M4's job); paint `putImageData` @1440p
  **~2–4 ms [E]**.
- Pacer: `PACE_SLICE_BYTES` 24 KB; LAN drain 800 Mbps, gap ≈ 240 µs/chunk;
  WAN drain = rate×1.5, budget 16 ms; gaps real to ~±100 µs
  (`precise_sleep`, worst overshoot **435 µs [M]**).
- Rates (`tuned_bitrate`/`h264_bitrate_for`, 0.16 bpp): 1080p60 ≈ 19.9,
  1440p60 ≈ 35.4, 4K60 ≈ 79.6 (LAN cap 80) Mbps; WAN cap 40; Studio floor
  150 Mbps; Game single-frame VBV ⇒ steady frame ≈ bitrate/fps.
- Frame sizes: game 1440p steady ≈ 35.4 Mb/60 = **73.7 KB**; studio ≈
  312 KB; lossy keyframe wall ≈ **190 KB [M-ref]**; lossless IDR ≈
  **1.4 MB [M-ref]**.
- LAN line rate 1 Gbps; WAN RTT 25–80 ms.

### 3.2 Today's per-frame waterfalls (host-add + viewer-add, native-decode popout path)

"Glass-add" = capture-acquire → photons, excluding capture-age (damage
arrival is content-driven) and display vsync (constant across variants).

**Game · H.264 · 1440p60:**

| Stage | LAN | WAN | Source |
|---|---|---|---|
| convert (blt queue) | 0.5 | 0.5 | [E] |
| encode P2 | 5.7 | 5.7 | [M] |
| pace spread + line (73.7 KB) | 1.3 | **10.9** (drain 53 Mbps) | [M-arith] |
| network | 0.3 | 12–40 (½RTT) | [E] |
| decode openh264 | 7 | 7 | [E] |
| I420→RGBA | 3 | 3 | [E] |
| IPC+parse | 3 | 3 | [E] |
| paint | 3 | 3 | [E] |
| **glass-add** | **≈ 23.8 ms** | **≈ 45–73 ms** | |

**Studio·LL · HEVC · 1440p60 (NVDEC viewer):** 0.5 + ~10 [E, lossless P3
unbenched] + pace (deltas small; IDR walls 1.4 MB spread ≤10 ms) + 4.24 +
1.8 + 3 + 3 ≈ **22.5 ms** steady. D3D11VA viewer: +1.5.

**Scaling rows (steady state, LAN, game-shape):** 1080p ≈ 0.5+3.2+0.9+
(openh264 ~4)+(convert 1.7)+(IPC 1.7)+(paint 1.7) ≈ **13.7 ms**;
4K ≈ 0.5+12.8+2.6+(~16)+(~6.8)+(~6.8)+(~6.8) ≈ **52 ms** — the 4K numbers
are exactly why T2.8+T2.9 matter: the viewer tail (~36 ms) dwarfs the
encode.

### 3.3 Per-feature deltas, with the math shown

**T2.9 (dirty-fraction parameterized, 1440p, viewer side).** Viewer work =
fixed + d·(convert + IPC + paint). Convert 1.8 (HEVC path; 3 for H.264),
IPC 3, paint 3; per-rect overhead ~0.05 ms ×≤8 = 0.4 ms worst:

| dirty d | convert | IPC ms | paint | viewer Δ vs full | IPC bandwidth @60 |
|---|---|---|---|---|---|
| 1% (cursor/typing) | 0.02 | 0.03 | 0.03 + 0.4 | **−7.3 ms** | 885 → **8.9 MB/s** |
| 5% (editing/terminal) | 0.09 | 0.15 | 0.15 + 0.4 | **−7.0 ms** | 885 → **44 MB/s** |
| 25% (window drag) | 0.45 | 0.75 | 0.75 + 0.4 | **−5.5 ms** | 885 → **221 MB/s** |
| 100% (video/game) | 1.8 | 3 | 3 | ±0 (self-coalesces to full) | 885 MB/s + 40 kbps wire |

Wire cost: ~84 B/frame SEI = 40 kbps = 0.11% of 35 Mbps [M-arith].
Viewer memory traffic: full path moves ≈ (read 5.5 MB NV12 + write/read/
write ~3×14.7 MB) ≈ **3 GB/s @60**; at d=5% ≈ **0.15 GB/s** — the laptop-
viewer battery/thermal story in one number.

**T2.6.** Displayed-interval σ: today ≈ network+decode jitter (M4 to
measure; `clumsy` ±10 ms ⇒ σ ≈ 6–10 ms); after: σ → capture σ (≈1–2 ms)
at mean cost = headroom ≈ p95 jitter (2–8 ms). Not a latency win — a
smoothness purchase with a bounded, displayed price.

**T2.2.** Win = `min(E, S)·(1−1/n)`, n = slices (8):

| Case | E | S | win |
|---|---|---|---|
| Game LAN steady | 5.7 | ~1.3 | 1.3×7/8 ≈ **−1.1 ms** |
| Game WAN steady (S = 73.7 KB @53 Mbps ≈ 10.9) | 5.7 | 10.9 | 5.7×7/8 ≈ **−5.0 ms/frame** |
| Studio WAN wall (190 KB, budget 16) | 12.9 | 16 | **−11.3 ms/wall** |
| 4K studio wall | ~29 | 16 | 16×7/8 ≈ **−14 ms/wall** |

As a function of slice count: win(n) = min(E,S)·(1−1/n) — 4 slices banks
75% of the gain, 16 banks 94%; T2.5b's grain (16 slices on big frames)
compounds here.

**T2.5a.** No honest latency claim — the claim is bits: screen-content
QP-emphasis literature + NVENC field practice put −10…−25% bitrate at
equal text SSIM [E, corpus to verify]. On WAN that is S shrinking
proportionally ⇒ tail-latency and loss-exposure follow (a −20% AU is a
−20% shorter train).

**T2.7.** Steady-state intra tax on a clean Game link: one full frame of
intra spread per period P. Extra bits/frame ≈ (I−P̄)/P with I ≈ 2.5×P̄
[E]: P=30 ⇒ +5%; P=120 ⇒ +1.25% — **≈ −4% effective bitrate** or, held
at rate, that margin returned to motion quality. Loss behavior unchanged
(snap-down + 3-frame heal).

**T2.8 (stage A, HEVC/D3D11VA route @1440p).** Deletes: staging copy
(~1.0 inside the 5.76 [E-split]), convert 1.8, IPC 3, paint 3; adds: blt
+ present ≈ 0.5–1 [E]. **Δ ≈ −7…−8 ms mean** and the jitter of three
CPU passes + webview scheduling collapses to swapchain timing; with
waitable-object latency 1, worst-case viewer-side queueing ≤ 1 frame.
At 4K the same deletion is ≈ −18 ms [E, ∝ pixels].

**T1.4.** Recovery cost: 190 KB IDR → ~74 KB P-frame (−61% burst); heal
start: next-cadence (≤2000 ms) or re-key RTT → one frame after report
(~17 ms + ½RTT). Artifact window on Balanced/Studio loss: from
"seconds-class" to "~2 frames + RTT".

**T2.3 gap-NACK:** heal start −1 RTT (25–80 ms WAN) when triggered
pre-decode-fail.

**Fence/async submit:** −0.3…−1 ms **p95 only under GPU contention**;
~0 mean [E — measure-first is the plan].

**T2.4:** artifact-visible ms per loss event → ≈ rescue decode+paint
(~3–5 ms perceptual blur) for the heal window's duration; +2 Mbps, +1
session.

### 3.4 Cumulative — best case vs honest case

**1440p60 typing/desktop (Studio·LL, NVDEC viewer, LAN):**

| | glass-add | note |
|---|---|---|
| today | ≈ 22.5 ms | §3.2 |
| + T2.9 (d=5%) | ≈ 15.4 ms | −7.0 viewer |
| + T2.8 instead of T2.9's IPC/paint half | ≈ **13.7 ms** best-case | T2.8 supersedes IPC+paint deletion; T2.9 still scissors the blt + keeps the canvas path cheap |
| honest case | **≈ 16–18 ms** | [E] spans (IPC/paint) may be smaller than est.; count only measured mechanisms until M4 |

**1440p60 Game WAN:** today ≈ 45–73; + T2.2 (−5.0) + T2.7 (rate margin)
+ T2.9 at HUD-quiet (−7 when quiet) ⇒ steady ≈ **40–68 best**,
**43–70 honest** (T2.9 counts only in quiet spells; the RTT term
dominates and no feature here touches it — stated plainly).

**4K60 Studio LAN:** today ≈ 52; + T2.8 (−18) + T2.2 walls (−14/wall) ⇒
**≈ 34 ms steady best-case** — 4K60 native-decode becomes genuinely
holdable; honest case ≈ 38–42 pending M4's [E] replacements.

Rule for reading the table: **best-case sums independent mechanisms;
honest-case only sums measured-on-this-box mechanisms and marks the rest
pending M1/M4** — the phase gates (§4) exist to move numbers from the
second column to the first.

### 3.5 Bandwidth deltas summary

| Feature | Wire | IPC (viewer-internal) |
|---|---|---|
| T2.9 | +40 kbps SEI | 885 → 9–221 MB/s (∝ d) |
| T2.5a | −10…−25% at equal SSIM [E] | ∝ |
| T2.7 | −4% effective (clean links) | — |
| T2.2 | 0 (same bytes, earlier) | — |
| T2.4 | +2 Mbps during engagement | +rescue frames (480p ≈ 55 MB/s while healing) |
| T2.8 | 0 | → ~0 (handle passing) |

---

## 4 · Phased rollout

**Phase E0 — Labs plumbing + truth (solo; ~1 week).**
Build: `labs.rs` gates + `labs state`/`labs probe`/`labs guard` log
discipline; LabsPill + sheet + `labs_set`/`labs_stats` ops; the M1 spans
(capture-age, pace+write split, first-byte) and M4 spans (feed-queue wait,
paint delta via one echoed IPC stamp, paint-interval σ) — the arc's
before/after camera. Also the two cross-feature audits: D3D11VA SEI
tolerance; `Flush()`-after-blt A/B.
*Entry:* none. *Exit:* labs lines in a field log; M4 histogram populated on
a loopback route; suite green (157+ node, `--no-default-features` check,
svelte-check 0 errors); all [E] numbers in §3.2 replaced or confirmed.
**Demoable:** the sheet itself.

**Phase E1 — viewer-side wins (solo-demoable; ~2 weeks).**
T2.9 stage 1a → 1b (lossless first, then lossy behind `strict`), T2.6,
T2.7.
*Entry:* E0 exit. *Exit:* T2.9 byte-exact + canvas-model tests green;
`video decode` shows `partial ≥90%` at ≤5% dirty with rgba-Mbps ∝ d;
strict-mode lossy mismatch ≤0.05%/frame over a 30-min soak; T2.6 σ halved
under injected jitter at ≤ +8 ms mean; T2.7 probe verdict ok + wave-shape
lines stepping. **Demoable:** the arc's headline solo demo — typing into
the popout with the sheet's counters live.

**Phase E2 — encoder structure (solo build, rig-confirmed; ~2 weeks).**
T2.2 (probe → streamed encode → emission), T2.5a/b (tail transcription →
probes → emphasis + grain), fence/async measure-first.
*Entry:* E1 T2.9 shipped (rects exist). *Exit:* subframe probe verdict +
first-byte column ≈ E/n; slice-concat byte-equality test green; qpmap
zero-map byte-identity green; SSIM text corpus ≥ +2 dB at equal rate or
−15% rate at equal SSIM (else T2.5a parks); M1 p95 fence columns.
**Demoable solo:** first-byte column + SSIM corpus; the −5 ms WAN claim
waits for the rig.

**Phase E3 — the loss story (2-machine rig + `clumsy`; ~2 weeks,
schedulable around rig availability).**
M5 characterization evening → T2.3 gap-NACK arming → T1.4 LTR → T2.4
rescue prototype. Also the E2 WAN confirmations and the shipped-loop
field items (BWE accuracy, freeze-seconds A/B) ride the same rig time.
*Entry:* rig assembled; M5 logged. *Exit:* artifact-visible ms/loss event
measured per heal mechanism (matrix tag in frame-health lines); LTR heals
≤2 frames with zero post-heal decode-fails on openh264 + WebCodecs;
gap-NACK false-positive rate <1/hour on a clean link; rescue judged
keep/park on freeze-seconds + side-by-side capture. **Not solo-demoable**
(that's the point of the rig).

**Phase E4 — the presenter (solo; ~2–3 weeks, can overlap E3).**
T2.8 stage A-fs (fullscreen popout, D3D11VA route) → A-windowed
(transparent-webview experiment) → stage-B probes only
(`CheckOverlaySupport` field data).
*Entry:* E0's M4 numbers (the before). *Exit:* M4 decode→glass Δ ≥ 5 ms
@1440p with jitter σ down ≥2×; guard-driven canvas fallback proven by
fault injection (kill the swapchain, stream survives); DPI/fullscreen/
occlusion checklist green; NVDEC-route interop decision made with
numbers. **Demoable solo** — and the single most impressive demo next to
T2.9 (fullscreen 4K popout, telemetry line showing the deleted GB/s).

Ordering rationale: E1 before E2 because T2.5 consumes T2.9's rects and
the demo story compounds; E4 independent of E2/E3 so presenter work can
absorb rig downtime.

---

## 5 · Risk register (top 10 across the arc)

| # | Hiccup | Likelihood | Blast radius | Detection signal | Rollback |
|---|---|---|---|---|---|
| 1 | **T2.8 compositor fights** — WebView2 z-order/transparency/DPI vs child swapchain; controls invisible or video black | High (windowed), Low (fullscreen-only) | Popout window only (canvas path intact) | `labs guard: present → canvas`; presentation-stats stall; user sees controls vanish | Automatic guard to canvas ≤1 frame; `X_PRESENT=0`; stage-fs default |
| 2 | **T2.9 damage divergence** — lossy re-quant outside compositor rects → stale smudges | Medium (lossy), ~0 (lossless) | One route's picture quality; no crash path | `strict` mismatch %, decode-fail uptick, `labs guard: damage → full` | Full-flag rules; per-route full-frame escalation; `X_DAMAGE=0`; lossless-only default until strict data |
| 3 | **Sub-frame driver variance** — offsets late/absent/coarse per driver gen | Medium | Latency win absent; never correctness (final lock is today's path) | `labs probe subframe` verdict; first-byte watchdog | Session self-kill to whole-AU; `X_SUBFRAME=0` |
| 4 | **PicParams tail transcription error (T2.5a)** — wrong qpDeltaMap offset = driver reads garbage | Medium at first write | Corrupt encode for the probe session only (feature never engages live pre-probe) | Probe (i) byte-identity fails | Feature is probe-gated by construction; fix offsets against staged header |
| 5 | **WC-buffer/fence pitfalls** — NT-store partial convert without `sfence`/alignment discipline; keyed-mutex deadlock; fence wait on removed device | Medium | Torn pixels (T2.9) / hung presenter (T2.8) / stalled encode (fence) | Byte-exact tests catch tearing in CI; mutex timeout; device-removed HRESULT | Partial convert reuses the proven kernel's `sfence` discipline + falls back to scalar on unaligned rects; all waits time-bounded → canvas/implicit paths |
| 6 | **T2.4 lane budget** — rescue lane starves a second monitor's stream to MJPEG | Medium | Multi-route setups | lane-pin log at assign; MJPEG fallback line | ≥2-free-lanes rule; `X_RESCUE=0` frees via RAII |
| 7 | **Reconfigure side effects (T2.7/AIMD interplay)** — wave-shape reconfigure emits IDR or resets GDR phase on some drivers; races `set_bitrate` | Medium | Game route smoothness (IDR walls return) | probe; `video out` keyframe count; frame-health SEI cadence | Probe-gated; serialize both reconfigures on the encode thread; `X_WAVE_STRETCH=0` |
| 8 | **SEI carrier tolerance** — our D3D11VA parser (or an exotic viewer) chokes on the damage SEI | Low-medium | HEVC routes decode-fail → re-key loop | E0 audit test; decode-fail feedback spike on engage | Caps-gated SEI on HEVC; strip-at-viewer option; `X_DAMAGE=0` |
| 9 | **Paint-PLL mislock (T2.6)** — damage-driven quiet spells decay into a stale headroom or a hold on the first motion frame | Medium | One viewer window's feel | paint-interval σ counter worsens; late-drop counter | Headroom decay + paint-now floor; toggle back to slam |
| 10 | **Gate leakage / dial precedence bugs** — an X_ dial acting without the tier, or a kill switch not reaching a hot loop | Low (single gate module) | Contract with the field ("off means off") | `labs state` line names resolved state; soak with tier off diffing logs vs baseline | One `labs.rs` choke point, per-frame atomic reads, and a CI test asserting tier-off ⇒ zero labs lines |

Cross-cutting mitigation: every feature's *else*-branch is the shipped
code path untouched (§1.1), so the worst field day is "turn it off and
you have yesterday's build" — which the versioning rule guarantees is
literally the same 0.2.46 prototype artifact.

---

## 6 · Appendix

**Dial inventory added by the arc:** `ALLMYSTUFF_EXPERIMENTAL`,
`ALLMYSTUFF_X_{DAMAGE, PAINT_PACE, SUBFRAME, QPMAP, GRAIN, WAVE_STRETCH,
PRESENT, LTR, GAP_NACK, RESCUE, ENC_ASYNC}` (§1.1 semantics). Existing
dials untouched; `ALLMYSTUFF_HEVC_DECODER=d3d11va` doubles as the
presenter-demo pin.

**New log lines:** `labs state`, `labs probe <feature>: …`,
`labs guard: <feature> → <fallback> (<reason>)`, `labs strict: …`; new
columns on `video out` (`dmg`, `sei`, `slices out`, `first-byte`) and
`video decode` (`partial/full`); wave-shape steps on `video rate`.

**New tests (by invariant):** partial-convert == full-convert (byte);
canvas-model == full-frame reference across forced drops; SEI-bearing
streams byte-exact through NVDEC + D3D11VA; slice-concat == whole-AU
encode; zero-qp-map == no-map (byte); `probe_nvenc_{subframe, qpmap, ltr,
wave_reconfig}` verdict tests (`#[ignore]`, hardware-gated, `SKIP:` clean
like every hardware test); tier-off ⇒ log-silence CI check.

**What this arc deliberately does not touch:** MyOwnMesh, signaling, RTP
semantics, the MJPEG floor, audio, versioning, `frame_interval_p = 1`,
and everything on the idea bank's explicitly-rejected list.
