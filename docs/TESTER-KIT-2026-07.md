# Tester kit — verifying the encoder line before the upstream PR

_For Chris's review + security push. Everything here runs against fork
`main`. The companion document that travels with the PR is
[`INTEGRATION-REPORT-2026-07.md`](INTEGRATION-REPORT-2026-07.md) (what
changed, blast radius, wire compat). Where it goes next lives in the
**fork-internal** docs (on the fork, not in the PR — see §5):
`docs/fork/SMOOTHNESS-IDEAS-2026-07.md`,
`docs/fork/EXPERIMENTAL-ARC-PLAN-2026-07.md`, and the how-to-interact
reference `docs/fork/PIPELINE.md`; nothing experimental is on by default._

## 0 · The artifact

Prototype installer (versioning untouched — manifests sit at your
0.2.46; the commit hash in the name is the identity):

```
AllMyStuff-PROTOTYPE-<commit>_0.2.46_x64-setup.exe
```

It bundles: the GUI, the `allmystuff-serve` node (all changes live
here), and **your pinned MyOwnMesh v0.3.1, byte-identical** — verify
with 7-Zip: `7z l <setup.exe>` shows `myownmesh.exe` (2026-07-17
timestamp) and `allmystuff-serve.exe`.

## 1 · Static verification (any box, ~10 minutes)

```powershell
git fetch origin && git switch main
# The whole fork delta:
git diff --stat 78b1c76..HEAD          # 78b1c76 = your 0.2.46 release
# The only wire-visible surface (30 additive serde-default lines):
git diff 78b1c76..HEAD -- crates/allmystuff-protocol/src/app.rs
# The daemon boundary (every daemon call is a pre-existing API):
git diff 78b1c76..HEAD -- node/src/mesh.rs
# Confirm the daemon pin is untouched:
git diff 78b1c76..HEAD -- .myownmesh-rev   # empty
```

Security-review shortlist (the surfaces worth your push, in order):
1. `crates/allmystuff-protocol/src/app.rs` — new fields arrive from
   remote peers: `est_kbps: u32`, `delay_trend_us_per_s: i32`,
   `lost_ts_us: Option<u64>`, `Tune.game/mode`. All plain data; the
   consumers clamp/gate (AIMD clamps to [8 Mbps, posture ceiling];
   feedback only accepted from the route's own active peer — the
   pre-existing `is_active() && peer == from` gate in
   `crates/allmystuff-session/src/lib.rs`).
2. `node/src/d3d11va.rs` — an in-house HEVC bitstream parser consuming
   REMOTE data. Scoped and defensive by design: every read
   bounds-checked (`BitReader` errors on truncation, never panics),
   `rbsp` capped, out-of-scope streams (10-bit, scaling lists,
   long-term refs, B slices, oversized RPS/tile grids, implausible
   geometry) rejected with named errors, and every failure soft-resets
   the session (bridge re-keys). Fuzzing this parser is a worthwhile
   security-push target; the entry point is `D3d11vaHevc::decode`.
3. `node/src/nvenc.rs` / `nvdec.rs` — FFI structs, size-asserted;
   inputs are local (our own encoder/driver), not remote.
4. `node/src/mesh.rs` `note_video_arrival` — remote-driven state
   (per-route arrival map): bounded (one entry per route, windows
   trimmed by time), no allocation proportional to remote input sizes
   beyond the per-minute sample vec.
5. `node/src/os_perf.rs` — process-local OS levers, all best-effort,
   nothing remote-reachable.

## 2 · Build + unit gate (dev box, ~10 minutes warm)

```powershell
cd node
cargo fmt --check
cargo clippy --release --all-targets
cargo check --no-default-features       # the capture-less/viewer build
cargo test --release -- --skip audio::  # 166 passed / 0 failed expected
cd ../gui ; npm run check               # 0 errors (1 known a11y warning)
cd .. ; cargo test --workspace          # shared crates: ~140 tests
```

(The `--skip audio::` is a dev-box environmental crash, pre-existing.)

## 3 · Hardware proofs (NVIDIA box, ~5 minutes)

The claims and the tests that pin them — all skip clean (passing)
without the silicon:

| Claim | Command | Expected |
|---|---|---|
| Lossless is bit-exact, NVENC→NVDEC | `cargo test --release nvdec_hevc_lossless_round_trip -- --nocapture` | `60/60 byte-exact` |
| Lossless is bit-exact through the VENDOR-NEUTRAL rung | `cargo test --release d3d11va_hevc_lossless_round_trip -- --nocapture` | `60/60 byte-exact @1280×718 (crop live)` |
| …and survives real pacer chunking | `cargo test --release d3d11va_survives_pacer_chunking -- --nocapture` | `30/30 byte-exact` |
| SIMD convert is byte-equal to scalar | `cargo test --release simd_lane_matches -- --nocapture` | pass (skips without AVX2) |
| GDR = no IDR walls, decodes clean | `cargo test --release nvenc_intra_refresh -- --nocapture` | pass |
| Sub-quantum pacing sleeps | `cargo test --release precise_sleep -- --nocapture` | worst overshoot ≪ 2 ms (dev box: 435–635 µs) |
| Decode rung ladder numbers | `cargo test --release -- --ignored bench_nvdec --nocapture --test-threads=1` and `bench_d3d11va` | @1440p: decode+copy ≈4.2 vs ≈5.4 ms · nv12→rgba ≈1.8 ms |
| New-box decode field kit | `cargo test --release -- --ignored probe_d3d11va --nocapture` | lists HEVC configs (`ConfigBitstreamRaw=1`) |

## 4 · Field smoke (two machines, ~20 minutes)

1. Install the prototype on both. Start a console session; confirm the
   Mode control cycles **Balanced → Game → Studio → Studio·LL** (one
   bandwidth warning on first Studio entry).
2. Logs live at `%LOCALAPPDATA%\AllMyStuff\logs\allmystuff-serve.log`.
   The lines that narrate a session end to end (all on by default,
   1 Hz/5 s/60 s cadences):
   - `video out …: raw N Mbps → wire N Mbps · age N ms (p95) · scale · encode …`
     — sender layers + M1 capture-age span.
   - `pace gaps: … requested avg → actual avg · worst · >1 ms err % · daemon write avg µs/chunk`
     — pacing honesty + the daemon-pipe await split.
   - `video in …: chunk-trains n · implied p5/p50 Mbps · est · delay trend`
     — the viewer's bandwidth estimate from your own paced bursts.
   - `video decode …: fps · ms/frame · wire → nv12 → rgba Mbps` — viewer layers.
   - `video rate …: X → Y Mbps (ceiling Z)` — every closed-loop step
     (Game posture only by default).
   - `HEVC decoder for …: NVDEC (…)` or `D3D11VA (… vendor-neutral)` —
     which glass Studio·LL crossed.
   - 1 Hz telemetry: CPU/thread split, per-engine GPU busy, VRAM.
3. Per-posture checks:
   - **Game**: no periodic keyframe pulses on the wire (GDR); pull the
     network cable for 2 s → picture heals in a wave, log shows the
     wave restart (3-frame short heal if you do it twice in 10 s).
   - **Studio·LL** on an NVIDIA pair: decoder line says NVDEC. Set
     `ALLMYSTUFF_HEVC_DECODER=d3d11va` on the viewer and restart it:
     same picture, decoder line says D3D11VA — that's the cross-vendor
     path exercised on NVIDIA glass.
   - **WAN dial-down rehearsal** (single site): `clumsy` at 20–40 Mbps
     ±20 ms on the viewer; in Game the `video rate` line should step
     down within ~5 s and climb back after ~30 s clean.
4. Kill switches if anything misbehaves (all env, viewer/host as noted):
   `ALLMYSTUFF_RATE_ADAPT=0` (host) · `ALLMYSTUFF_PACED_SLICES=0`
   (host) · `ALLMYSTUFF_NVENC=0` (host, pins MF) ·
   `ALLMYSTUFF_GPU_LANE=0` (host) · `ALLMYSTUFF_HEVC_DECODER=nvdec`
   (viewer). Opt-ins that default OFF: `ALLMYSTUFF_MMCSS=1`,
   `ALLMYSTUFF_GPU_SCHED=1`, `ALLMYSTUFF_AUTO_ADAPT=1`,
   `ALLMYSTUFF_RATE_ADAPT=1` (all-postures form). Full dial table:
   integration report §5.

## 5 · The PR

**Keep fork-internal docs out of the PR.** The fork's own pipeline docs
(`docs/fork/**`) and the engineering handoff (`HANDOFF.md`) live on the
fork and should not travel upstream — the PR is code + the review
dossier (`INTEGRATION-REPORT` + this kit) only. A GitHub PR is a
whole-branch diff, so the way to exclude them is to open the PR from a
**curated branch** that strips those paths, not from `main` directly:

```powershell
# from an up-to-date fork main, cut the upstream branch and strip fork-only docs
git switch -c for-upstream main
git rm -r docs/fork
git rm HANDOFF.md
git commit -m "chore: strip fork-internal docs for the upstream PR"
git push -u origin for-upstream
# docs/fork/ and HANDOFF.md stay on fork main; the PR never carries them.
# Re-cut this branch (delete + redo) whenever main advances before the PR merges.
```

Suggested shape: one PR, `for-upstream` → upstream `main`, title
`encoder/decoder line: GPU zero-copy pipeline, four postures, cross-vendor
lossless, closed WAN loop` — the 53-commit history is the review unit
(each commit message carries its reasoning; the integration report is
the map). Draft body:

> This is the encoder/decoder line built on the fork, as discussed —
> computer-engineering layer only, strict prototypes until your
> signoff. Versioning untouched (manifests at 0.2.46; you cut versions).
>
> **What it is:** GPU zero-copy capture→encode (D3D11 VideoProcessor +
> texture-fed NVENC/MF ladder), four postures (Balanced / Game with GDR
> / Studio / Studio·Lossless bit-exact HEVC), hardware decode with a
> vendor-neutral D3D11VA rung (NVIDIA host → AMD/Intel viewer works),
> an app-side link-fitted slice pacer with a closed loop (chunk-train
> BWE → AIMD bitrate, Game-only by default), Win11 scheduling honesty,
> AVX2 kernels, and per-layer bandwidth observability in the standard
> log.
>
> **What it is NOT:** zero MyOwnMesh changes (your v0.3.1 pin is
> byte-identical in the bundle), zero signaling-path changes, zero new
> daemon surfaces — everything rides existing track lanes and
> CHANNEL_CONTROL with additive serde-default fields (old↔new peers
> degrade to today's behavior in all pairings).
>
> Review map (in this PR): `docs/INTEGRATION-REPORT-2026-07.md` (blast
> radius, suggested review order) and `docs/TESTER-KIT-2026-07.md` (this
> verification). The full engineering narrative (`HANDOFF.md`) and the
> pipeline how-to docs live on the fork under `docs/fork/` if you want
> deeper context — they're intentionally kept out of this PR. For the
> security push, the remote-input surfaces are the protocol fields and
> the `d3d11va.rs` parser — see the kit's §1 shortlist.

Open it with:

```powershell
gh pr create --repo mrjeeves/AllMyStuff --base main --head nathanfraske:for-upstream `
  --title "encoder/decoder line: GPU zero-copy pipeline, four postures, cross-vendor lossless, closed WAN loop" `
  --body-file docs/pr-body.md   # paste the draft above
```
