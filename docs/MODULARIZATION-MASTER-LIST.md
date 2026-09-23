# Modularization, optimization and MyOwnMesh master list

Original source inventory: `e6340b31daa6d9c2058c4ee345564d3e0c0ecebf`, reviewed on
2026-09-22. The completed storage, terminal and Unix-terminal work is included
in that revision. Audio extraction and cumulative macOS CI were pending at
that checkpoint; their later evidence is recorded below. This document changes
no behavior, feature default, dependency pin or authority check.

This is the index for the audited work and remaining opportunities. The linked
reports retain exact commits, commands, failures and platform qualifications.
Historical reports use older file locations; optimization and migration symbol
anchors still refer to the original inventory unless explicitly marked otherwise.

**Current evidence, 2026-09-22:** native macOS
[run 35763211936](https://github.com/nathanfraske/AllMyStuffEncoderFraske/actions/runs/35763211936)
passed all 38 selected suites / 338 executions on each of Intel and ARM64 at
`b86c9fb31baed34a2feeb55243a90120ec669088`, including the extracted audio
fixtures. Both host compile checks, four unchanged lock hashes, tracked-source
checks and normal private-process/directory cleanup passed on each architecture.
The [Mac evidence report](reviews/modular-foundation/macos-validation.md)
records the exact selections, tools, native linkage and exclusions. This is
676 selected executions, with deliberate repeats, not 676 distinct definitions.

The [audio report](reviews/modular-foundation/audio-library-extraction.md)
records focused Windows package results of 14 default, 33 codec and 52 I/O
executions: 99 feature executions covering 52 distinct safe definitions. Local
final formatting/lint evidence uses
`ad5962d3db096e1ee42d3a1ebeae184c606cab7d`; its only difference from the Mac-tested
revision is three comment/local-lint-allow lines in a test adapter. It is not a
second Mac run. Local cleanup
`1f653544-c931-4925-9ba2-77c4c066ddf1` also passed: 6,374 regenerable files totaling
2,577,006,776 logical bytes (2.4000 GiB) were removed, with protected evidence,
source, four locks and native Opus records verified unchanged. This is logical
file size, not measured freed filesystem blocks. Neither validation result
qualifies live audio/video devices, GUI/mobile products, MSRV or
forced-cancellation recovery.

**Priority/state convention:** P1 is a correctness, lifecycle or qualification
gate; P2 is a useful bounded follow-up; P3 needs measurement before investment.
`Complete` means the named extraction or check is complete, not that every
platform/device is qualified. `Active` means assigned work is still underway.
`Proposed` and `Gated` items are not implementation authorization. Allocation,
copy and dependency observations are source evidence; faster execution,
smaller binaries and better latency remain hypotheses until measured.

## Library and caller locations

All workspace-inherited packages below currently use version `0.2.121`, edition
2021 and declared Rust `1.88.0`. The minimum compiler version is a declaration,
not a new test result. Root, node, desktop GUI and mobile have separate
workspaces and lockfiles. Root tests do not validate the other three products.

| ID / state | Reusable implementation and symbol anchors | Retained caller paths and feature boundary | Evidence |
| --- | --- | --- | --- |
| MOD-01 Complete | [frame timing](../crates/allmystuff-frame-timing/src/lib.rs): `FrameCadence`, `AssemblyClock`, `send_breakdown`, `periodic_sample` | [node timing shim](../node/src/video_frame_timing.rs); video reexports `timing`. Standard library only; caller owns clocks and waits. | [catalog and timing vectors](MODULAR-LIBRARIES.md) |
| MOD-02 Complete | [byte queues](../crates/allmystuff-byte-queues/src/lib.rs): `ByteQueues::{ensure,watch,unwatch,enqueue,poll}` | [node shim](../node/src/byte_queues.rs); terminal viewer uses the same implementation. Per-key payload bound, whole-chunk eviction and watcher ownership remain unchanged; `parking_lot`/`tracing`. | [queue policy and checks](MODULAR-LIBRARIES.md) |
| MOD-03 Complete | [video pacing](../crates/allmystuff-video-pacing/src/lib.rs): `PacePolicy`, `PaceRouteState`, `pace_policy`, `frame_policy` | [node shim](../node/src/video_pacing.rs), video `pacing` reexport; Mesh retains shared buckets, sleeps, overrides and bilateral negotiation. Standard library only. | [pacing vectors and limits](MODULAR-LIBRARIES.md) |
| MOD-04 Complete | [update policy](../crates/allmystuff-update-policy/src/lib.rs): `ApplyPolicy`, `compare_semver`, `policy_allows` | [updater policy shim](../crates/allmystuff-updater/src/policy.rs). Serde only in production; downloading, staging and installation remain outside. Permissive version parsing is preserved. | [policy compatibility](MODULAR-LIBRARIES.md) |
| MOD-05 Complete | [inventory model](../crates/allmystuff-inventory-model/src/lib.rs): inventory/device records and pure helpers | [scanner types shim](../crates/allmystuff-inventory/src/types.rs); [bridge](../crates/allmystuff-bridge/src/lib.rs) consumes canonical model directly. Serde; scanners remain in `allmystuff-inventory`. | [model/reexport validation](MODULAR-LIBRARIES.md) |
| MOD-06 Complete | [video metadata](../crates/allmystuff-video-metadata/src/lib.rs): `AuIdentity`, `AuRecovery`, `annexb_nals`, insert/peek/take marker helpers | [node wire shim](../node/src/video_wire.rs), video `metadata` reexport. `memchr`; exact marker bytes, permissive walk and first-match behavior retained. | [metadata differential evidence](MODULAR-LIBRARIES.md) |
| MOD-07 Complete | [node client](../crates/allmystuff-node-client/src/lib.rs): shared wire, `NodeClient`, terminal client, address/error modules | [node_control](../node/src/node_control.rs) reexports client/wire; [amst client shim](../crates/allmystuff-term/src/client.rs) selects terminal-compatible error handling. Tokio/interprocess/Serde/protocol; node retains bind permissions, dispatch and process ownership. | [IPC extraction report](reviews/modular-foundation/ipc-client-extraction.md), [package](../crates/allmystuff-node-client/README.md) |
| MOD-08 Complete | [video](../crates/allmystuff-video/src/lib.rs): `codec`, `framing`, `receive`, `ingress`, `handoff`; optional output/decoder/capture/backends | [Mesh receive adapter](../node/src/mesh.rs), [daemon ingress adapter](../node/src/control_client.rs), [decode output adapter](../node/src/video_decode.rs), [handoff packet adapter](../node/src/video_handoff.rs). Default features empty; `decode` adds native receive, `host` includes decode/capture/encode, `hwenc` includes host/FFmpeg. Node always enables decode and retains its default host mapping. | [video extraction report](reviews/modular-foundation/video-library-extraction.md), [package](../crates/allmystuff-video/README.md) |
| MOD-09 Complete | [storage plan](../crates/allmystuff-storage/src/plan.rs): records, prepared inputs, `PlanState`, validation, sanitization, transitions and digest | [StoragePlanStore](../node/src/storage_plan.rs) retains mutex, path/load/write and public record paths; [persist](../node/src/persist.rs) retains atomic writes. Serde/JSON core; synchronous persistence callback preserves consumed setter counters versus full merge rollback. Mesh retains sender/manager and local volume/capacity decisions. | [storage report](reviews/modular-foundation/storage-library-extraction.md), [package](../crates/allmystuff-storage/README.md) |
| MOD-10 Complete | [terminal](../crates/allmystuff-terminal/src/lib.rs): viewer and optional `host::TerminalHost<S>`, `TaskSpawner` | [node host shim](../node/src/terminal.rs) supplies `NodeSpawner` at original spawn sites; [node viewer shim](../node/src/stubs/terminal.rs) explicitly selects viewer behavior. Empty defaults; host adds xpty `0.3.6` (portable-pty alias), dirs, locks and task/timer support. Consent, routing and IPC remain in node. | [terminal report](reviews/modular-foundation/terminal-library-extraction.md), [package](../crates/allmystuff-terminal/README.md) |
| MOD-11 Complete focused extraction/validation | [audio](../crates/allmystuff-audio/src/lib.rs): [PCM helpers](../crates/allmystuff-audio/src/pcm.rs), [codec](../crates/allmystuff-audio/src/codec.rs) `OpusStream`/`OpusDecoder`, [I/O](../crates/allmystuff-audio/src/io.rs) `AudioBridge<S>`/`StatsPolicy`, [disabled implementation](../crates/allmystuff-audio/src/disabled.rs) | [audio shim](../node/src/audio.rs), [disabled shim](../node/src/stubs/audio.rs), [Mesh](../node/src/mesh.rs). Empty defaults; `codec` adds Opus/session, `audio-io` adds CPAL/Linux Pulse bridge. Node keeps codec always; its audio-io forwards only the I/O feature. Node supplies lazy shared statistics policy and retains queues/routes/auth. | [Audio extraction report](reviews/modular-foundation/audio-library-extraction.md), [package](../crates/allmystuff-audio/README.md), [Mac results](reviews/modular-foundation/macos-validation.md); device-opening retained case excluded. |

Video's platform implementations now live under
[`crates/allmystuff-video/src`](../crates/allmystuff-video/src): `video.rs`,
`camera_capture.rs`, `win_capture.rs`, `wayland_capture.rs`, `videotoolbox.rs`,
`nvenc.rs`, `nvdec.rs`, `d3d11va.rs`, `mediafoundation.rs`, `amf.rs`,
`gpu_pipeline.rs`, `hwenc.rs`, `os_perf.rs` and `wake.rs`. Corresponding node
modules are compatibility paths or narrow policy adapters. In particular,
`node/src/video.rs` and `node/src/win_capture.rs` supply `DesktopFollower`;
the decoder output adapter preserves the local IPC envelope. No route or
authorization ownership moved into these backends.

At the original source-review checkpoint, the audio candidate was committed as
`c1bce0fee51843cc73df2299f3ae9236112cc1ea` and independently checked against
`target/audio-extraction/source-ready.json` in C1's worktree: node audio shim
`2909dc5d016284e81bed6f808a32ff746a6f2480`, disabled shim
`da86d053fb04ceeea2d1c2c09f464580a56e065e`, Mesh
`e56745af8f75818baebd722a1e6a249de82683d0`. Exactly three Mesh substitutions
reverse to the entire original file: decoder type, constructor and the FEC
argument moved inside the wrapper. That receipt establishes source equivalence;
the later integrated fixture/compiler/runtime results are separately recorded
in the audio report and current-evidence annotation above.

Other existing packages and application boundaries must remain visible when
planning removals; these are not all new extractions:

| Location | Current responsibility and dependency boundary |
| --- | --- |
| [allmystuff-graph](../crates/allmystuff-graph/src/lib.rs) | Capability graph, sharing and compatibility rules; application policy survives a transport change. |
| [allmystuff-protocol](../crates/allmystuff-protocol/src/lib.rs) | Application messages plus the hand-maintained daemon control mirror and state-path discovery. `control.rs` is a migration boundary, not a new node IPC client. |
| [allmystuff-session](../crates/allmystuff-session/src/lib.rs) | Pure application route/presence/media state and shared wire records, including [AudioFrame](../crates/allmystuff-session/src/audio.rs); transport authority is separate. |
| [allmystuff-inventory](../crates/allmystuff-inventory/src/lib.rs), [bridge](../crates/allmystuff-bridge/src/lib.rs) | Platform probes remain in scanner; inventory-to-capability mapping remains in bridge. Model reuse does not qualify device probing. |
| [allmystuff-updater](../crates/allmystuff-updater/src/lib.rs), [service](../crates/allmystuff-service/src/lib.rs) | Host installation/update/service effects stay outside the pure update policy. These are actual dependencies of product entry points. |
| [allmystuff-cec-protocol](../crates/allmystuff-cec-protocol/src/lib.rs), [cec-consent](../crates/allmystuff-cec-consent/src/lib.rs) | Support wire and approval policy; node's [cec adapter](../node/src/cec.rs) applies them to application operations. |
| [allmystuff-mobile-core](../crates/allmystuff-mobile-core/src/lib.rs) | Phone capability/profile model and testable client specification. The shipped [mobile engine](../gui/mobile/src/engine.rs) still embeds node and MyOwnMesh; it is not replaced by those model tests. |
| [allmystuff-cli](../crates/allmystuff-cli/src), [allmystuff-term](../crates/allmystuff-term/src) | Product CLI/serve delegation and terminal UI. `allmystuff-term` is distinct from the extracted `allmystuff-terminal` library. |
| [node/pixels](../node/pixels/Cargo.toml) | Existing standard-library pixel hot-loop crate, version `0.1.0`; selective optimization profile is retained. It is not GUI code merely because video host depends on it. |
| [node](../node/Cargo.toml), [desktop](../gui/src-tauri/Cargo.toml), [mobile](../gui/mobile/Cargo.toml) | Application integration and separate lock/feature graphs. Desktop still links node for supervision/diagnostics; mobile defaults to embedded Mesh plus node without host and with audio-io. |

## Porting and validation locations

| ID / state | Source and recipe locations | Evidence and remaining boundary |
| --- | --- | --- |
| PORT-01 Complete bounded Windows preparation | [node feature gates](../node/src/lib.rs), [manifest](../node/Cargo.toml), `Mesh::advertised_capabilities`, [direct Serve help](../node/src/bin/serve.rs) | [Stage-one ledger](reviews/modular-foundation/stage-one-verification.md) records locked Windows feature/capability/help checks. Earlier dependency-review host leaks are historical findings corrected by that work; no claim of a fully media-free Serve. |
| PORT-02 Complete focused Windows extraction gates | MOD-01 through MOD-11 and their reports | Existing reports distinguish library tests, node integration checks and actual native PTYs. Storage reports 52 focused cases; terminal reports 67 distinct Windows definitions across 84 executions. Audio adds 52 distinct safe definitions across 99 feature executions. These are separate gates, not a summed whole-product score. |
| PORT-03 Complete Unix terminal qualification | [terminal host](../crates/allmystuff-terminal/src/host.rs), [lifecycle fixtures](../crates/allmystuff-terminal/tests/support/lifecycle.rs), terminal report's WSL section | Native WSL2 Ubuntu 24.04 passed retained 13 cases (11 PTY, 2 pure) and 9 lifecycle cases, with reviewed private home/tmp/target and PID-namespace cleanup. This does not qualify other Unix packages or macOS. |
| PORT-04 Experimental, failure retained | [vendored OpenH264 wrapper](../vendor/openh264-sys2-0.9.6/build.rs), [vendor verification](../vendor/verify_openh264.py), [RISC-V scripts](../scripts/riscv), [experiment](reviews/portability/openh264-riscv64.md), [Serve plan](reviews/portability/serve-riscv64-test-plan.md) | Two target-recognition additions retain OpenH264 `0.9.3`/sys2 `0.9.6`. No-default riscv64gc-musl Serve linked; QEMU ran 684 historical root/node cases and degraded Serve lifecycle. Unoptimized standalone codec proof still aborts on a shift check; diagnostic relink failed. Real Mesh, target child/self-exec, doctests, board/device and performance remain unqualified. |
| PORT-05 Complete bounded native Mac qualification | [workflow](../.github/workflows/modular-macos.yml), [runner](../scripts/ci/modular_macos.py), [process guard](../scripts/ci/macos_processes.py), [exact inventory](../scripts/ci/modular-macos-suites.json), [runner notes](../scripts/ci/README.md) | [Run report](reviews/modular-foundation/macos-validation.md): 338 executions / 38 suites per native Intel/ARM architecture, plus video/node host compile-only, at `b86c9fb`. Includes repeated terminal viewer and audio feature definitions; both routine cleanup receipts pass. Hardware audio/video, GUI/mobile, MSRV, omitted adapters and forced-cancellation recovery are not qualified. |
| PORT-06 Proposed additional qualification | [platform review](reviews/modular-foundation/headless-platform-review.md), [release workflow](../.github/workflows/release.yml), [mobile docs](MOBILE.md) | A configured release target is not inspected runtime evidence. Native Linux/ARM, Android/iOS, Windows ARM, hardware capture/encode/decode, service operation and Rust 1.88 need their own exact configurations and authorized fixtures. Historical review claims must be read with later Windows/WSL/RISC-V evidence above. |

The initial macOS source checkpoint identities were workflow `fdf1259d`, process guard
`ec23f2ae`, runner `ffbf32ae`, suite inventory `06686ae1`, README `a96b3f94`.
Those identities are source evidence, not pass evidence. Its 239 selections were: timing 5,
queues 4, pacing 10, update policy 6, model 7, metadata 5, IPC 16, storage core 28,
video core 42, terminal viewer 17, terminal host public/channel 18/30, retained
Unix terminal 13, lifecycle 9, software decode 7 and node storage 22.

The first [run 35760215630](https://github.com/nathanfraske/AllMyStuffEncoderFraske/actions/runs/35760215630)
was canceled: Intel completed 217/239 selected executions and ARM completed
239/239, but the overall jobs did not pass. Host compilation was interrupted;
the still-running supervisor overlapped the always-cleanup step. ARM cleanup
reported `ENOTEMPTY` in the private Cargo registry. The cancellation origin
and exact ARM writer PID remain unknown. The subsequent runner correction
serializes supervisor/cleanup ownership and stops further child launches;
the successful 338-selection run adds 99 audio feature executions to the
original inventory. Its normal cleanup exercised an uncontended lease with
no termination signals, so it does not prove forced-cancellation recovery.

GitHub documents both native labels used by the validated workflow:
`macos-15-intel` and `macos-15` (ARM64). Each job receives a fresh hosted VM;
that does not remove the need to identify and clean up test-owned PTY
descendants before deleting their private directories.
[Runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).

## Optimization opportunities

Every proposed optimization below keeps its baseline until separately
implemented and compared. None has a measured speedup in this inventory.

### OPT-01 — PCM and Opus buffering copies

**P2 / Proposed / allocation optimization.** Evidence:
[`OpusStream::push` and `AudioBridge::feed`](../node/src/audio.rs) extend/copy
into a remainder buffer, drain its consumed prefix, and downmix before playback
ring insertion. Non-48-kHz, nonzero input rates allocate a resample buffer;
48-kHz and zero-rate Opus input already borrow the input slice. Do not claim a
redundant pass-through allocation. Candidate destinations are MOD-11's `codec`
and `io`; dependency is pinned Opus `0.3.1`, with no newer Mesh prerequisite.

Bounded action: benchmark caller-owned scratch buffers/read offsets separately
from borrowed mono playback normalization. Expected benefit is fewer copies
and allocations; latency benefit is unknown. Risk: callback-panic residue,
encoder state/drain order, partial channel groups, zero-rate handling and strict
ring trimming. Validate paired old/new packets and PCM, remainder/panic cases,
and allocations/CPU/p95 latency at 20/30 ms, 24/44.1/48 kHz and multiple routes.

### OPT-02 — Per-packet decoder scratch allocation

**P2 / Proposed / allocation optimization.** Evidence:
[`Mesh::handle_audio_inbound`](../node/src/mesh.rs) allocates and zeros 5,760 i16
samples before locking the route decoder map, then truncates after decode.
MOD-11 preserves that ownership; the wrapper does not remove the allocation.
Dependency: Opus `0.3.1`/fixed 48-kHz mono contract, not a Mesh upgrade.

Bounded action: measure a per-route reusable output buffer. Expected benefit:
allocation reduction; throughput/latency unmeasured. Risk: error recovery,
teardown, buffer lifetime and locking move. Require stateful paired decoders,
empty-packet PLC, malformed then valid packets, output-size failures, route
isolation and allocation/latency measurements before adopting it.

### OPT-03 — Audio map-lock scope

**P2 / Proposed / contention measurement.** Evidence: the decoder-map lock in
[`Mesh::handle_audio_inbound`](../node/src/mesh.rs) spans decoding;
[`AudioBridge::feed`](../node/src/audio.rs) holds the playback map through
conversion, statistics and ring append. Dependency: existing node/audio
ownership, with no upstream prerequisite. Contention impact is a hypothesis.

Bounded action: profile one and many routes, then consider per-route handles
without combining it with extraction. Risk: late feed after stop, ordering,
auto-traits and stop/join races. Gate on lock wait and p95/p99 latency under
concurrent teardown, plus identical ordering, error and shutdown fixtures.

### OPT-04 — Statistics-label allocations

**P3 / Proposed / allocation optimization.** Evidence:
[`AudioBridge::feed`, `metered`, `LevelStats`](../node/src/audio.rs) clone route
labels per buffer although periodic statistics emit roughly every five
seconds. Dependency: current synchronous statistics closure and node's lazy
statistics dial. No measurable hot-path effect has been established.

Bounded action: borrow a label or defer formatting until emission. Risk:
changed lock lifetime, counters, log target/text or lazy policy evaluation.
Measure allocations and verify trace equivalence and the existing statistics
fixtures; do not introduce a new global dial or eager runtime capture.

### OPT-05 — Binary IPC body copy

**P2 / Proposed / copy-versus-write benchmark.** Evidence:
[`MediaTrackPipe::send_frame`](../node/src/control_client.rs) calls
[`encode_media_frame`](../crates/allmystuff-protocol/src/control.rs) before its
writer lock, allocating/copying a header-plus-payload body. Binary H.264/Opus
already works when `Status.media_pipes` is true; base64 is a compatibility
fallback. See the [independent follow-up](reviews/modular-foundation/myownmesh-followups.md#3-measure-one-outgoing-body-copy-before-changing-it).
Dependency: current v0.3.21 adapter, later reassess at MESH-03.

Bounded action: compare a separately built header and borrowed payload writes
under the same lock. Expected benefit: fewer copied bytes; extra writes may
cost more. Risk: framing, checked lengths, partial writes, producer interleave,
timeouts/reconnects. Compare exact bytes and allocation/elapsed-time results
for full AUs, paced slices, audio, empty and UTF-8 identifiers; preserve separate
audio/video pipes. This is not an end-to-end zero-copy claim.

### OPT-06 — Queue and admission budgets

**P2 / Proposed / resource measurement.** Evidence:
[`ByteQueues`](../crates/allmystuff-byte-queues/src/lib.rs) limits payload per
key, not key count, empty chunks or framing/allocation overhead;
[`Mesh::start_media`](../node/src/mesh.rs) uses an eight-item audio `try_send`
queue with intentional loss under pressure. Video ingress/handoff has separate
reference/recovery policy and cannot use arbitrary byte eviction.
Dependency: current app policy; upstream admission integration waits for MESH-07.

Bounded action: record occupancy, drops, age and total retained memory before
considering aggregate limits or injectable policies. Expected benefit:
predictable memory/latency, unmeasured today. Risk: a larger queue or blocking
send changes responsiveness and loss behavior. Validate slow/full/closed
consumers, audio alongside video, independent routes and exact notification
semantics. Preserve per-key watcher cleanup and local u32-LE packing.

### OPT-07 — Streaming resampler quality and duration

**P2 / Proposed explicit behavior change.** Evidence:
[`resample_linear`](../node/src/audio.rs) restarts phase and floors length for
each buffer; split fractional buffers can differ from concatenated input.
The audio baseline deliberately freezes that behavior. Dependency: MOD-11 PCM
module, no Mesh prerequisite. Improved duration/quality is a hypothesis.

Bounded action: evaluate a stateful resampler as its own feature/change, with
an explicit latency/history budget. Risk is changed samples and Opus packets,
not a transparent refactor. Validate duration drift, spectra, CPU, resets,
discontinuities and split/concatenated traces over representative device rates.

## Fat, duplication and dependency opportunities

### FAT-01 — Already removed duplication; keep compatibility surfaces

**Complete for MOD-01..10 / consolidation.** The libraries above now own their
implementations; node/updater/scanner/amst paths are compatibility shims.
The former independent local-node IPC clients now share MOD-07 while retaining
their different malformed-event handling. Benefit is one implementation and
independent consumers, not proven binary-size reduction. Dependency: current
package graph. Validation is in each extraction report.

Do not count these small shims, test-only frozen baselines/oracles or the
separate terminal viewer refusal surface as removable production duplication.
Deleting them would break compatibility or proof. Future shim retirement needs
an explicit public-path deprecation plan and consumer compile checks.

### FAT-02 — Optional receive codecs and selectable node capabilities

**P2 / Proposed / dependency split.** Evidence:
[`node/Cargo.toml`](../node/Cargo.toml) always enables video decode and currently
Opus; MOD-11 moves Opus to an always-enabled codec dependency. A no-default
node still includes application services. Node host still groups PTY, capture,
input and clipboard even though standalone terminal hosting is now selectable.
Dependency: current OpenH264 `0.9.3`, Opus `0.3.1`, terminal `xpty 0.3.6`; no
Mesh upgrade is required to design an internal seam.

Bounded action: choose a real consumer needing a smaller capability set, then
separate feature forwarding, state, dispatch and advertisements together.
Expected benefit: smaller selected build closure; size/startup gains unmeasured.
Risk: advertising unusable endpoints or breaking default/mobile audio behavior.
Validate default/no-host/audio-only/PTY-only graph and positive/refusal tests;
removing imports alone does not create a media-free Serve.

### FAT-03 — Desktop node dependency after IPC extraction

**P2 / Proposed / host-service boundary.** Evidence:
[`gui/src-tauri/src/main.rs`](../gui/src-tauri/src/main.rs) still uses node
`ensure_node_running`/`NodeChild`, daemon repair/discovery and diagnostics;
[`gui/src-tauri/Cargo.toml`](../gui/src-tauri/Cargo.toml) links node defaults.
MOD-07 only centralizes the IPC client. Dependency: existing supervision and
state/privilege ownership; migration is independent of a Mesh pin change.

Bounded action: inventory these remaining calls, then decide narrow host-service
library versus local RPC for one responsibility. Expected benefit: a thinner
GUI dependency graph; unmeasured build/binary gain. Risk: sidecar installation,
owned-child cleanup and privileged operations. Validate GUI startup/failure/
shutdown/repair with explicit owned helpers, and four-workspace locked graphs.

### FAT-04 — Application planes still live in node

**P2 / Proposed / modularity.** Evidence: [files](../node/src/files.rs),
[fleetfiles](../node/src/fleetfiles.rs), [drive mounts](../node/src/drive_mount.rs),
[sites](../node/src/sites.rs), [KVM media](../node/src/kvm_media.rs) and
[`Mesh`](../node/src/mesh.rs) still combine application-plane lifetime and
adapters. Node retains bundled SQLite, watching, HTTP/WebDAV/WebSocket and
updater/scanner closure. MOD-09 extracted only storage-plan decisions.
Dependency: existing plane contracts; transport replacement is gated on MESH-02.

Bounded action: select one consumer-backed pure model or provider boundary,
freeze its behavior and leave auth/I/O ownership explicit. Expected benefit:
reusable independent components; dependency reduction unmeasured. Risk:
filesystem semantics, reconciliation and remote authority. Validate wire and
failure compatibility in private roots, then product integration. Moving Mesh
methods to sibling files alone does not establish a reusable boundary.

### FAT-05 — Deliberately different framing walkers

**P3 / Proposed only after contract decision.** Evidence:
[`framing/host.rs`](../crates/allmystuff-video/src/framing/host.rs) and
[`framing/stub.rs`](../crates/allmystuff-video/src/framing/stub.rs) retain
different malformed-input walks; the video frozen fixtures preserve them.
Dependency: current coded-unit contract, unrelated to upstream RTP ownership.

Bounded action: measure maintenance cost and decide whether a future explicit
normalization is wanted. Benefit is less duplicated logic; no speedup evidence.
Risk: silently changing partitioning/recognition on malformed input. Keep the
separate implementations until differential vectors, truncation and byte-exact
concatenation prove the chosen behavior on both host and receive-only paths.

### FAT-06 — Separate workspaces, lock drift and build duplication

**P2 / Proposed audit / dependency hygiene.** Evidence: root excludes node/GUI;
four manifests/locks repeat patches. At the inventory base, desktop/mobile
still record older local node versions (`0.2.118`/`0.2.119`). Audio's reviewed
seed preserves all old root records and all existing consumer external
records; adding 36 native records to root is relocation, not a dependency bump
or proof they compile in default audio. Dependency: resolver 2 and current pins.

Bounded action: first audit/fix only stale local edges in a separate change and
measure repeated build/cache cost by target/features before workspace merging.
Benefit: reproducibility and potentially less repeated work; savings unknown.
Risk: feature unification enabling native/stub behavior or broad pin churn.
Require before/after four-workspace locked metadata/normal+build edge maps,
default/no-host tests and unchanged external identity/checksum records.

### FAT-07 — Native toolchain and compatibility settings

**P3 / Proposed measurement / build hygiene.** Evidence:
[`openh264-sys2/build.rs`](../vendor/openh264-sys2-0.9.6/build.rs) has no-ASM
fallback; native codec builds use the existing CMake policy floor in
[Cargo config](../.cargo/config.toml) and CI. NASM presence, compiler and target
can change the generated path. Dependency: pinned native packages, including
Opus/audiopus_sys and mozjpeg where selected. The pinned audiopus_sys build
can find installed static Opus through pkg-config on macOS before falling back
to bundled CMake; a Cargo lock does not fix the native library origin. Retain
actual link-library/search-path output when comparing builds.

Bounded action: record toolchain/ASM identity with benchmarks; separately assess
a reviewed native-package update before removing compatibility settings.
Benefit: predictable comparisons and possibly simpler builds, unmeasured
performance. Risk: output/platform regressions and lock churn. Validate locked
native builds plus codec roundtrips on both Mac architectures and existing
Windows/Unix configurations; do not silently install an assembler for a comparison.

## Correctness and lifecycle follow-ups

### BOUND-01 — Paced-video first and replacement fragment limit

**P1 / Completed and validated.**
[`Freshness::forward_paced`](../crates/allmystuff-video/src/ingress.rs) and
[`accept_paced_fragment`](../crates/allmystuff-video/src/receive.rs) now apply
the same inclusive 16 MiB payload ceiling to initial/replacement fragments as
to continuations. Rejection clears the affected pending unit, preventing a
later marker from emitting stale data, and retains each path's existing
discontinuity/recovery handling. Other peer/lanes and routes remain independent.
The [BOUND-01 evidence note](reviews/modular-foundation/paced-video-byte-bound.md)
records the explicit old/new behavior difference, source reviews and actual
execution results; the [original review](reviews/modular-foundation/myownmesh-followups.md#1-apply-the-existing-paced-au-limit-to-the-first-fragment)
and frozen extraction oracles remain historical evidence.

This corrects a per-AU retention gap; it does not cap aggregate memory or alter
unpaced forwarding, the separate 64 MiB media IPC frame limit, dependency pins,
feature defaults or application/Mesh authorization. No Mesh upgrade is needed.
The regression inventory covers exact-limit/plus-one payloads, stale markers,
Reset/Gradual recovery, sink pressure/closure and lane/route isolation. Windows
passed 206 executions covering 140 distinct test names, scoped formatting and
strict workspace/node Clippy. Native Mac passed 352 executions/39 suites per
architecture at `c75e7aa`; host gates there are compile-only. The evidence note
retains negative controls, the first shared-target failure and qualified
artifact-reuse diagnosis, and the Windows build-path failure/short-target fix.
No latency, crash-resilience or performance benefit is claimed.

### LIFE-01 — Terminal generations and descendant ownership

**P1 / Proposed lifecycle work.** Evidence:
[`TerminalHost::open_with`, `arm_idle_reaper`, `close`](../crates/allmystuff-terminal/src/host.rs)
initialize every generation to 1; recycled session IDs can therefore share a
generation. PTY close signalling is not proof that all shell descendants have
exited. Existing channel and native fixtures freeze actual behavior; external
test containment supplies the broader cleanup boundary.

Dependency: xpty `0.3.6` and caller `TaskSpawner`. Bounded action: independently
design generation ownership and descendant shutdown requirements, rather than
changing them during extraction. Benefit: stronger lifecycle fencing; no new
runtime defect reproduction is claimed here. Risk: idle/detach/close semantics
and blocking joins. Validate recycled IDs, stale reapers, cancellation/panic,
shell children and bounded shutdown on Windows and Unix using private fixtures.

### LIFE-02 — Audio start/stop race and lock ownership

**P1 / Proposed lifecycle measurement.** Evidence:
[`AudioBridge::{start_capture,start_playback,stop,stop_all}`](../node/src/audio.rs)
separate duplicate-start checks from insertion. `stop` removes workers before
off-lock joining; `stop_all` clears/joins while holding map locks. MOD-11
preserves these exact windows. This establishes ordering differences, not a
demonstrated deadlock. Dependency: current CPAL/thread ownership.

Bounded action: add controlled concurrent-start/stop/displacement evidence
before proposing per-route ownership state. Expected benefit: predictable
shutdown and less lock contention, not yet measured. Risk: callback lifetime,
join ordering and late frames. Validate deterministic fake workers and later
authorized device lifecycles, with bounded joins and unchanged ordinary behavior.

## MyOwnMesh: current contract and migration gates

The application pin [.myownmesh-rev](../.myownmesh-rev) is **v0.3.21**, also the
[latest published release checked on 2026-09-22](https://github.com/mrjeeves/MyOwnMesh/releases/tag/v0.3.21).
The newer V4 architecture is staged as **v1.0.0** in still-draft
[upstream PR #135](https://github.com/mrjeeves/MyOwnMesh/pull/135), reviewed at
`db7818e09fedd98899490347b86ac9bc9f97b59b` in
`nathanfraske/MyOwnMeshSecurityReview`. V4 is not a published `v4` tag.

Two distinct upstream sources matter: the exact
[Rust embedding API](https://github.com/nathanfraske/MyOwnMeshSecurityReview/blob/db7818e09fedd98899490347b86ac9bc9f97b59b/docs/APPLICATION-API.md)
(blob `0fc5cc0790fe8dd46fe987e0e5780119a2542cf4`) and
[daemon wire](https://github.com/nathanfraske/MyOwnMeshSecurityReview/blob/db7818e09fedd98899490347b86ac9bc9f97b59b/crates/myownmesh/src/control/wire.rs)
(blob `2e0690b9ddf7dfe39a29d30f63434e156467bb93`). The embedding facade uses
`Mesh::open_connector_capable*`, `MeshHandle` and `JoinedNetwork`; it explicitly
excludes conceptual names such as `request_session` and `watch_session` as
callable APIs. Peer selectors still occur in supported channel methods and
are not themselves authority. Daemon flow ownership has its own wire contract.

The existing [V4 transition proposal at 2349e6c](https://github.com/nathanfraske/AllMyStuffEncoderFraske/blob/2349e6c24a74a788c213f983100427b049177769/docs/MYOWNMESH-V4-TRANSITION.md)
(blob `d449d0f9c4573518451411d5cb7ca053c3b6438e`, app baseline `8fc90d85`)
supplies the AMS-V4-0..6 responsibility/order map. Its conceptual API names must
be re-anchored to the exact current sources above. It is a proposal, not proof
that those older method names exist. No production pin or transport change is
part of this inventory.

### MESH-01 — Explicit supported contract and legacy inventory

**P1 / Gated migration design; AMS-V4-0/1.** Evidence:
[`ensure_daemon_current`, `log_daemon_version`](../node/src/daemon_spawn.rs)
compare numeric versions; [`Mesh`](../node/src/mesh.rs) probes
`Status.media_pipes` and falls back to JSON/base64. Neither demonstrates
compatibility with a different daemon protocol. [Independent contract follow-up](reviews/modular-foundation/myownmesh-followups.md#2-distinguish-a-version-minimum-from-daemon-contract-support).

Bounded action: inventory every current request/caller and define an explicit
supported-contract/readiness matrix before selecting a new pin. Benefit:
understandable refusal instead of assuming a newer number is compatible.
Risk: unintended rejection of supported older daemons or authority fallback.
Validate fake-daemon legacy/missing/malformed/error/unsupported-new-contract
cases, followed by exact-version real sessions. Failed-helper Serve tests do
not establish Mesh compatibility; never restart a foreign daemon as a fallback.

### MESH-02 — Separate node IPC, daemon IPC and embedded session adaptation

**P1 / Gated on reviewed target; AMS-V4-1/2.** Current anchors:
[`protocol::control::Request`](../crates/allmystuff-protocol/src/control.rs),
[`ControlClient::{request,subscribe_events}`](../node/src/control_client.rs),
[`Mesh`](../node/src/mesh.rs), [mobile engine](../gui/mobile/src/engine.rs).
MOD-07's [node client](../crates/allmystuff-node-client/src/lib.rs) speaks local
application IPC; replacing it does not migrate the daemon contract.

Bounded action: introduce one narrow adapter for exact supported operations,
with typed lifetime/refusal outcomes; keep application route/consent policy
above it. Dependency: db781 target and a reviewed migration/version decision.
Benefit: transport/session changes stop leaking through product owners.
Risk: stale sessions, delivery semantics and local principal binding. Validate
disconnect/replacement/revocation and bounded message/RPC/stream operations,
including refusal before protected delivery. An engine acknowledgement is not
an application persistence receipt; retain necessary app-level acknowledgements.

### MESH-03 — General realtime flow ownership

**P1 / Gated on MESH-01/02; AMS-V4-3.** Current anchors:
[`MediaPipe`, `MediaTrackPipe`, `subscribe_media_source`](../node/src/control_client.rs),
`Mesh::start_media`/audio and video lane maps, and protocol `MediaLane*`,
`MediaTrackPipe`, `MediaSourcePipe`, `VideoSend`, `AudioSend`.
The reviewed target defines `RealtimeFlowOpen`, `RealtimeFlowClose` and
`RealtimePipe`; outbound flow capability and client binding differ from the
current lane/peer selection. [Exact wire](https://github.com/nathanfraske/MyOwnMeshSecurityReview/blob/db7818e09fedd98899490347b86ac9bc9f97b59b/crates/myownmesh/src/control/wire.rs#L495).

Bounded action: migrate one audio/video flow through an owned adapter, preserving
application codecs/AU metadata and separate pressure policy. Benefit: explicit
session/flow lifetime, not promised throughput. Risk: stale-handle sends or
closes touching a successor, send refusal leaking an opened flow, media
starvation. Validate send/close after replacement, label reuse, cancellation,
refusal, multi-monitor plus audio, direct/TURN and mixed supported versions.
Remove legacy sends only after the replacement and compatibility gates pass.

### MESH-04 — Presence, route intent, fleet and claims

**P1 / Gated on session adapter; AMS-V4-4.** Current anchors:
[`allmystuff-session`](../crates/allmystuff-session/src/lib.rs) profile/route
state, [`OwnedRoster`](../crates/allmystuff-protocol/src/app.rs), Mesh's
`desired_routes`, local-claim and fleet handling, and
[`ownership`](../node/src/ownership.rs). These carry application metadata and
intent alongside legacy authority integration.

Bounded action: distinguish profile/reachability observations, desired app
routes and authenticated membership; migrate one governance/claim boundary at
a time against the exact target. Benefit: fewer competing authority stores.
Risk: accidental admission/revocation or lost offline intent. Validate missing/
lagged observations, reconnect, LAN-only claim constraints, replayed gossip,
foreign contexts and explicit Closed governance; a claim code or cached profile
must not become authorization. Do not delete metadata needed by the product.

### MESH-05 — CEC support consent binding

**P1 / Gated on session adapter; AMS-V4-5.** Current anchors:
[`cec-consent`](../crates/allmystuff-cec-consent/src/lib.rs),
[`cec-protocol`](../crates/allmystuff-cec-protocol/src/lib.rs),
[`node::cec`](../node/src/cec.rs). The transition proposal keeps support
discovery separate from approval. Dependency: exact target/session adapter;
discovery metadata is not a grant.

Bounded action: bind existing Once/ThreeHours/Forever application policy to the
reviewed authenticated peer/principal/session lifetime. Benefit: precise revoke
and expiry behavior; no assertion that the current implementation is bypassed.
Risk: granting support from a beacon, stale identity or transport presence.
Validate deny/expire/revoke/reconnect and session-scoped Once behavior; preserve
CEC-specific identity/store separation until an explicit migration decides it.

### MESH-06 — Eventual deletion of legacy authority/transport adapters

**P2 / Gated on all preceding migration gates; AMS-V4-6.** Current anchors:
protocol's legacy request mirror, node control/media pipes and Mesh fallbacks.
The existing transition proposal suggests a temporary explicit legacy namespace
and feature; neither is implemented at the inventory base.

Bounded action: after supported consumer/peer migration, remove one proven-unused
legacy surface and its now-unreferenced dependencies. Benefit: less maintenance
and duplicate contract state, not measured binary savings. Risk: removing a
working old-daemon/base64 fallback prematurely. Require no first-party legacy
imports, desktop/headless/mobile and mixed-version gates, then locked dependency
proof. API deprecation/removal needs an explicit compatibility decision.

### MESH-07 — Resource policy, registration and capability meaning

**P2 / Gated integration; related to OPT-06.** Current anchors:
[`ByteQueues`](../crates/allmystuff-byte-queues/src/lib.rs),
`Mesh::advertised_capabilities`, [bridge](../crates/allmystuff-bridge/src/lib.rs),
[inventory model](../crates/allmystuff-inventory-model/src/lib.rs).
Dependency: exact target's resource/admission and advertisement surfaces.

Bounded action: map application limits and deployment requirements to the
reviewed public provider/policy surface; document which registration/approval
requirements remain application-owned. Benefit: coherent accounting and honest
capabilities. Risk: treating an inventory record, advertisement or capacity
example as authority, or inventing an upstream app registry. Validate bounded
pressure/refusal, feature-disabled endpoints and revoke/lifetime behavior.
Do not copy sample budgets or broaden identity grants.

### MESH-08 — Application timing, pacing and coded-unit metadata

**P2 / Gated integration measurements.** Current anchors: MOD-01/03/06,
video ingress/receive, Mesh pacing/sender adapter. Dependency: chosen target
connector metrics and flow contract, after MESH-03. The app's local IPC u32
packing and H.264/HEVC identity markers are not Mesh packetizer specifications.

Bounded action: correlate existing AU identity and local durations with transport
observations while preserving payload bytes and rate policy. Benefit: useful
end-to-end diagnosis; no cross-host clock accuracy claim. Risk: double pacing,
head-of-line delay or changing marker/RTP meaning. Validate same-host clocks,
direct/TURN, one/many monitors plus audio, queue age and recovery traces. Never
subtract unrelated hosts' `Instant` values or replace app recovery with generic
transport assumptions.

The eight flags already recorded in [MODULAR-LIBRARIES](MODULAR-LIBRARIES.md)
are retained without duplicate work items:

| Existing flag | Master item |
| --- | --- |
| Legacy daemon adapter and capability publication | MESH-01/02/06 |
| App registration and identity grants | MESH-07, with MESH-04/05 authority gates |
| Local queue packing and future routes | OPT-06 and MESH-08 |
| Resource policy and admission | OPT-06 and MESH-07 |
| Timing and observability | MESH-08 |
| Application pacing | MOD-03 and MESH-08 |
| Inventory and evidence | MOD-05 and MESH-07 |
| Encoded AU metadata | MOD-06 and MESH-08 |

## Remaining qualification and test opportunities

| ID / priority / state | Observed evidence and bounded action | Dependency, benefit, risk and validation |
| --- | --- | --- |
| TEST-01 / P1 / Safe selection validated; device qualification pending | Original `audio::tests::capture_and_playback_for_one_route_coexist`, now `io::tests::capture_and_playback_for_one_route_coexist`, calls real CPAL despite its CI comment. It remains excluded. The six retained safe cases plus 46 new definitions pass in exact default/codec/I/O selections on Windows and both Mac architectures. | CPAL `0.15.3` and Opus `0.3.1`. Benefit: honest device-free results. Risk: broad filters touching devices or counting an omitted case. [Audio evidence](reviews/modular-foundation/audio-library-extraction.md) covers paired persistent decoders, PCM/ring/callback behavior; 99 feature executions are 52 definitions, not device, quality or universal libopus-byte qualification. |
| TEST-02 / P1 / Proposed remaining Mac adapters | PORT-05 still omits `node_control::tests::` (17 Mac), `persist::tests::` (2), scanner `model_identity` (1), and `control_client::tests::` (13). Extracted audio is now included as 99 passed feature executions per architecture. Add remaining adapters only with source-audited exact filters. | Current app source; manager-owned native Mac execution. Benefit: adapter/platform evidence. Risk: broad node tests construct Mesh/stores or native backends. Require private home/state/tmp, positive expected counts, complete logs and descendant cleanup; enumeration of the omitted cases is not a pass. |
| TEST-03 / P2 / Proposed | Broad [existing CI](../.github/workflows/ci.yml) uses moving OS labels and some unlocked/broad root/node/GUI commands. PORT-05 adds an explicit bounded Mac lane but does not replace every old job. | GitHub hosted image/toolchain identity. Benefit: reproducible, attributable platform results. Risk: broadening runtime claims or removing useful legacy coverage. Review each product's selected targets, lock hashes and hardware assumptions before reconciling workflows. |
| TEST-04 / P2 / Proposed | GUI/mobile local-lock drift, separate workspaces, declared Rust 1.88, native features and build-script side effects remain independent qualification gaps. | FAT-03/06; desktop's existing `ALLMYSTUFF_SKIP_SIDECAR=1` only skips sidecar staging. Benefit: product-level evidence. Risk: treating root or host-library checks as GUI/mobile support. Require exact locked builds, target/feature graphs and authorized isolated runtime/device gates; no remote device qualification is claimed here. |
| TEST-05 / P2 / Deferred experiment | PORT-04 retains an unresolved RISC-V unoptimized codec abort and failed diagnostic link. Do not count the other 684 passes as codec-proof success. | Frozen OpenH264/toolchain/proof identities in the experiment. Benefit: a diagnosed portability boundary. Risk: suppressing checks, changing optimization or blindly replaying a failed recipe. A separately resumed investigation must first identify the fault with a reviewed diagnostic build, then rerun the same proof and retain negative evidence. |
| TEST-06 / P2 / Proposed capability work | [`nvdec::NvdecAv1`](../crates/allmystuff-video/src/nvdec.rs) and [`d3d11va::D3d11vaAv1`](../crates/allmystuff-video/src/d3d11va.rs) return not-implemented errors. Hardware availability cannot enable those stubs. | Current video backend interfaces, no Mesh prerequisite. Benefit: actual additional codec support. Risk: claiming support from a probe or compile result. Implement separately, then qualify real decode, frame shape, errors/fallback and device behavior; this is not dead-code cleanup. |

Near-term ordering: BOUND-01 is complete with its evidence linked above; measure OPT-01/02/05
before choosing performance work; design real-consumer feature/host-service
seams before removing dependencies. MyOwnMesh migration begins with MESH-01/02
against a reviewed target contract and ends with deletion, rather than starting
with a pin bump. No proposed item is marked complete by this audit.
