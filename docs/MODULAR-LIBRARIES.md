# Selective AllMyStuff libraries

The current direction is to select reusable libraries individually, with an
optional AllMyStuff runtime hosting capabilities and the GUI consuming them.
There are no new consumer presets or profiles in this change. The optional
host and application registration model remain future work; these small
extractions preserve the existing calculations and caller behavior.

**Experimental RISC-V checkpoint.** The cumulative changes also carry a
two-line correction to the pinned OpenH264 wrapper's RISC-V target recognition.
The full no-default Serve binary linked for RISC-V musl; 684 root/node tests
and a degraded Serve lifecycle fixture passed under QEMU. A separate
unoptimized codec proof still aborts on a shift check, and a diagnostic relink
failed before execution. Further diagnosis is deferred while work returns to
modularization. Doctests, real Mesh sessions, target child/self-execution and
hardware remain unverified. The [experiment record](reviews/portability/openh264-riscv64.md#risc-v-build-and-emulation-checkpoint)
preserves the failures, earlier Windows evidence, workspace limits and exact
artifact identities.

| Library | Current contents | Direct dependencies |
| --- | --- | --- |
| [allmystuff-terminal](../crates/allmystuff-terminal/README.md) | Viewer queues and optional shared PTY sessions, scrollback, resize and lifecycle. | Default: byte queues, `serde`, `tokio`; `host` adds `dirs`, `parking_lot`, `tracing` and existing `xpty` under its `portable-pty` alias. |
| [allmystuff-storage](../crates/allmystuff-storage/README.md) | Fleet storage-plan records, validation, ordered transitions and digest, with caller-supplied persistence. | `serde` and `serde_json`; `parking_lot` is test-only. |
| [allmystuff-video](../crates/allmystuff-video/README.md) | Encoded-video rules, receive/handoff policy, optional decode workers and capture/encode backends. | Default: existing timing/metadata/pacing libraries and `tracing`; native dependencies are feature-gated. |
| [allmystuff-byte-queues](../crates/allmystuff-byte-queues/src/lib.rs) | Viewer byte queues, watcher tokens and local IPC chunk packing. | `parking_lot` 0.12, `tracing` 0.1. |
| [allmystuff-frame-timing](../crates/allmystuff-frame-timing/src/lib.rs) | `FrameCadence`, `AssemblyClock`, `SendBreakdown`, `send_breakdown` and `periodic_sample`. | Standard library only. |
| [allmystuff-update-policy](../crates/allmystuff-update-policy/src/lib.rs) | `ApplyPolicy`, exact policy-token parsing, `compare_semver` and `policy_allows`. | `serde` 1; `serde_json` is test-only. |
| [allmystuff-video-pacing](../crates/allmystuff-video-pacing/src/lib.rs) | `PacePolicy`, `PaceRouteState`, `pace_policy`, `frame_policy` and `LAN_AGGREGATE_POLICY`. | Standard library only. |
| [allmystuff-inventory-model](../crates/allmystuff-inventory-model/src/lib.rs) | Inventory/device records, enum wire values and their existing pure helpers. | `serde` 1; `serde_json` is test-only. |
| [allmystuff-video-metadata](../crates/allmystuff-video-metadata/src/lib.rs) | Annex-B offsets and AU identity marker insertion, inspection and removal. | `memchr` 2. |

These packages do not depend on the node, GUI or a Mesh transport. Their default
features exclude native codecs and capture backends; the unified video package
adds those through explicit features described below. They inherit workspace
version `0.2.121`, edition 2021 and declared
minimum Rust `1.88.0`; a declared minimum is not a new toolchain qualification.
The node modules are compatibility shims: the public
`allmystuff_node::byte_queues::ByteQueues` path remains available, and existing
internal frame-timing/pacing/video-metadata call sites use the extracted implementations.
The updater's public `allmystuff_updater::policy` path is also a reexport shim.
The scanner continues to export `allmystuff_inventory::Inventory` and all its
device types, now reexports of the same canonical model definitions.

**Selective use.** An application can depend on each package directly. For
a consumer beside this repository, choose the dependency lines it needs:

```toml
[dependencies]
allmystuff-terminal = { path = "../AllMyStuff/crates/allmystuff-terminal" }
allmystuff-storage = { path = "../AllMyStuff/crates/allmystuff-storage" }
allmystuff-video = { path = "../AllMyStuff/crates/allmystuff-video" }
allmystuff-byte-queues = { path = "../AllMyStuff/crates/allmystuff-byte-queues" }
allmystuff-frame-timing = { path = "../AllMyStuff/crates/allmystuff-frame-timing" }
allmystuff-update-policy = { path = "../AllMyStuff/crates/allmystuff-update-policy" }
allmystuff-video-pacing = { path = "../AllMyStuff/crates/allmystuff-video-pacing" }
allmystuff-inventory-model = { path = "../AllMyStuff/crates/allmystuff-inventory-model" }
allmystuff-video-metadata = { path = "../AllMyStuff/crates/allmystuff-video-metadata" }
```

Adjust these paths to the checkout location. Keep the repository workspace
metadata available when using these path dependencies; this example does not
claim registry publication. Public usage is exercised by rustdoc examples
and/or integration tests that import the independent crates.

Reuse the queue for a local viewer that can tolerate whole-chunk eviction;
reuse timing for local capture scheduling or assembly/send measurements. An
updater can reuse the decision helpers without network/install code, and a
video sender can reuse pacing calculations while owning its waits and state.
Inventory consumers can exchange snapshots without linking platform scanners;
encoded-video consumers can inspect or add the existing AU metadata without
linking a capture, decoder or node runtime.
These choices require no running AllMyStuff host. The extractions supply no
application SDK, provider registry or identity-approval system.

**Unified video library.** `allmystuff-video` has an empty default feature set:
metadata, timing, pacing, classification, the separate host/receive-only framing
walks, route assembly, canonical-peer ingress and handoff policy are available
without native media dependencies. `decode` adds software receive workers and
a caller-supplied RGBA output policy. `host` includes `decode` and the existing
capture/encode group with platform backends; `hwenc` includes `host` and the
optional FFmpeg encoder ladder. Node always enables `decode` and forwards its
existing `host` and `hwenc` flags, preserving its default and no-default mappings.

Node retains route binding, authorization, local IPC envelopes/transport and
process supervision. Its decoder output adapter writes directly into the
existing final packet allocation; its desktop follower is constructed inside
the original capture thread. Shared performance and wake state each have one
implementation, with node compatibility reexports. Queue feedback, recovery,
malformed-input differences, logging targets and backend selection retain their
existing behavior. The [video extraction review](reviews/modular-foundation/video-library-extraction.md)
records exact compatibility evidence and remaining platform/hardware limits;
the [package README](../crates/allmystuff-video/README.md) describes its interfaces.

**Terminal library.** `allmystuff-terminal` has an empty default feature set
and supplies the existing viewer API and queues. Enable `host` for real PTY
sessions; `host::TerminalHost<S>` accepts a static `TaskSpawner` policy for the
existing idle-reaper and legacy-output-bridge task sites. It does not capture a
runtime during construction. The always-available `viewer` module retains
hosting refusal even when another consumer enables `host` in a shared graph.
This package is distinct from the `allmystuff-term` command-line application.

Node retains its public terminal paths, forwards its existing `host` feature
and supplies its original global spawn behavior. Authentication, terminal-share
consent, route binding and IPC dispatch remain in the application. Existing
close/stop and detach behavior, queue bounds, scrollback bytes and resize rules
are preserved. The [terminal extraction report](reviews/modular-foundation/terminal-library-extraction.md)
records the frozen comparisons, isolated PTY tests and actual validation limits;
the [package README](../crates/allmystuff-terminal/README.md) describes the API.

**Storage plan library.** `allmystuff-storage::plan` provides the existing
records, validation, ordered merge decisions and serialized digest without a
node runtime, state-directory lookup, mutex or filesystem dependency.
`PlanState::sanitize()` retains the original load filtering and ordered caps.
Prepared local updates and peer patches apply through a synchronous persistence
callback, preserving validation order and the distinct setter/merge rollback
rules, including consumed counters after a failed setter.

The caller supplies synchronization, durable loading/writing and authenticated
sender/management decisions. Node retains `storage_plan::StoragePlanStore` and
the existing public record paths, plus volume/capacity checks, storage-root
materialization, reconciliation and broadcasts. The library does not implement
file transfer, mounts or fleet authentication. The
[storage extraction review](reviews/modular-foundation/storage-library-extraction.md)
records the frozen comparisons, isolated persistence tests, actual central
results and platform limits.

**Byte-queue policy.** These are the existing policies, retained unchanged:

- `ensure(key)` creates an eager queue with reserved token `0`, keeping bytes
  received before a viewer subscribes. It leaves an existing queue intact.
- `watch(key)` returns a new token and takes ownership of the same buffered
  queue. `unwatch(key, token)` removes it only if the token still matches, so
  stale window cleanup cannot remove a newer watcher's queue. `remove(key)`
  removes the queue regardless of token. Tokens scope cleanup, not permission.
- The configured limit bounds retained payload bytes **per key**. Overflow
  evicts whole oldest chunks until they fit; a chunk larger than the limit can
  evict itself and everything before it. This is not reliable delivery.
- `enqueue` drops bytes and returns `false` if the key has no queue. Otherwise
  it returns whether the queue was empty before appending, as a notification
  hint. Oversized chunks, including nonempty input at a zero-byte limit, can
  return `true` while leaving the queue empty. Callers poll after notification;
  the result is not a delivery acknowledgement.
- `poll` drains chunks in order, each packed as a four-byte unsigned
  little-endian length followed by its bytes. An empty or absent queue yields
  an empty buffer. Callers must keep each chunk length representable as `u32`;
  the current packing code casts the length without checking it.
- The limit excludes allocation/framing overhead and does not cap key count
  or empty chunks. It is not a total memory budget or Mesh admission control.

The three original unit tests moved with the implementation. A public API
integration test preserves the oversized-chunk/notification boundary; the
rustdoc example exercises use from outside the node crate.

**Frame-timing policy.** Callers supply local `Instant` values and perform the
actual wait. `FrameCadence` clamps the requested rate to 1 through 240 fps and
advances past missed slots without replaying them. `AssemblyClock` includes
the closing marker in total duration and longest-gap measurements.
`send_breakdown` uses saturating microsecond accounting. `periodic_sample`
selects matching sequence numbers divisible by 60; it does not synchronize
clocks. Never subtract timestamps from different hosts. The five original
tests move with the implementation, alongside a public API rustdoc example.

**Update-policy behavior.** The three original tests and implementation move
unchanged. `ApplyPolicy` retains `Patch`, `Minor`, `All` and `None`, with exact
lowercase parse/serde tokens. `policy_allows` rejects equal/older candidates;
`None` always returns false. Fetching, staging, applying and the existing
callers' interpretation remain outside this decision library.

Version comparison remains permissive and semver-like: it reads only the
first three numeric components, maps missing/invalid/overflowing components
to zero, and compares prerelease suffixes lexicographically, including case.
A bare version outranks a nonempty prerelease. Build metadata is not parsed
as SemVer metadata: for example, `1.2.3+build` compares equal to `1.2.0`.
Public fixed tables preserve these quirks, policy gates and serde tokens;
this move does not tighten version validation or change update decisions.

**Video-pacing behavior.** The calculations and four original tests move
unchanged, with public visibility added for independent consumers:

- `pace_policy` returns a 96 KiB burst. With no diagnostic override,
  WAN/LAN floors are 8/16 Mbps; normal/game headroom is 3/2 or 5/4 of the
  target. Recovery headroom stops at 32 Mbps unless the target itself is
  higher. A nonzero override uses at least 8 Mbps.
- For LAN frames without an override, the rate considers the frame's bytes
  and requested fps (clamped to 1 through 240), capped at the shared 256 Mbps
  ceiling. That cap also applies to a higher supplied base rate. WAN paths
  and nonzero overrides return the supplied base policy unchanged.
- `PaceRouteState` retains debt across reservations and frame/policy changes.
  Elapsed time refills whole bytes up to one burst; changing the policy does
  not reset the bucket or reprice debt already scheduled. Deficit waits
  round up to microseconds. Zero-byte reservations return zero even with
  debt outstanding; zero drain refills nothing but uses a denominator of
  one when calculating a deficit wait. No input validation is added.
- Existing arithmetic limits remain: `override_mbps * 1_000_000` can overflow
  above `u64::MAX / 1_000_000` (panic with overflow checks, wrap otherwise),
  and adding an unrepresentable future deadline to `Instant` can panic.
  Public traces use safe large values and representable deadlines; they do
  not establish an all-values contract.

Six public fixed-vector tests cover policy outputs, same-timestamp variable
reservations, idle refill, policy changes, zero/small limits and rounding.
The node keeps its remaining pure pacing tests in `mesh.rs` and all runtime
integration: shared bucket ownership, caller sleeping, environment override reads,
unsplittable-slice bypass, bilateral negotiation and route-generation guards.

**Inventory-model behavior.** The original scanner `types.rs` is copied
unchanged: all 21 public record/enum definitions, serde attributes and pure
methods have one canonical implementation. Existing scanner imports remain
valid without conversions. The defining crate changes, so diagnostic
`std::any::type_name` strings change; no such consumer was found in current
application source. This does not promise a stable Rust binary ABI.

Missing collection fields still become empty vectors, missing `Option` fields
become `None`, and existing additive defaults stay unchanged: device default
flags are false, input `endpoints` is one, and optional network/listening
details are empty or false. Explicit `endpoints: 0` stays zero; explicit null
collections remain errors. Unknown object fields are ignored, while unknown
enum variants remain errors. No new normalization or validation is added.

`device_count` still counts the nine device categories plus CPU/memory and
excludes listening services and temperature sensors. `is_array` requires an
input device with at least four channels. Service labels, schemes and web
classification remain exact. Scanner probes, category-default selection,
reports and host semantics remain in `allmystuff-inventory` unchanged.

The bridge consumes the model directly while retaining its graph/protocol
dependencies and mapping behavior. Its nine existing mapping/site tests remain;
the two site fixtures that previously called `scan()` now reuse the existing
fixed inventory fixture. Seven public model tests cover full/legacy JSON,
defaults, helpers and enum tokens. A scanner integration test checks all 21
reexport identities and scanner function signatures without invoking probes.

**Video-metadata behavior.** `AuIdentity`, `AuRecovery`, `annexb_nals`,
`insert_au_identity_marker`, `peek_au_identity_marker` and
`take_au_identity_marker` move unchanged except for public visibility.
The Annex-B walk keeps its three/four-byte start-code offsets and permissive
header handling. Insertion retains the exact 41-byte H.264 or 42-byte HEVC
marker, UUID, recovery byte and 16 lowercase hexadecimal sequence digits.
It inserts before the first VCL NAL or appends if none is found; existing
markers are not replaced. Inspection/removal still recognizes only the exact
four-byte-prefix marker shape and removes only the first valid marker.
Foreign, malformed and truncated markers remain untouched.

Five public tests use fixed byte/offset expectations, malformed and truncated
fixtures, and repeated marker ordering; a rustdoc example imports the crate.
Existing node caller tests remain in place. Encoding, decoding, pacing,
fragmentation, transport packetization and recovery decisions are unchanged.
The node's direct `memchr` dependency moves to this library with the same
locked resolution; this is application coded-unit metadata, not a new Mesh API.

**Focused commands.** From the repository root, after package wiring is
assembled, check the current extraction pair:

```sh
cargo test --locked -p allmystuff-inventory-model -p allmystuff-video-metadata -p allmystuff-bridge
cargo test --locked -p allmystuff-inventory --test model_identity
cargo clippy --locked -p allmystuff-inventory-model -p allmystuff-video-metadata -p allmystuff-inventory -p allmystuff-bridge --all-targets -- -D warnings
cargo fmt --check -p allmystuff-inventory-model -p allmystuff-video-metadata -p allmystuff-inventory -p allmystuff-bridge
```

Select the earlier queue/timing/update-policy/pacing packages instead to
check those libraries. These commands are package selections, not
application profiles.

The test commands cover unit, public API integration and doc tests without
building the node. Node-shim compilation is separate integration evidence.
The [contribution guide](../CONTRIBUTING.md) describes wider checks. During this
managed extraction, important checks run through the manager's durable run
records; the commands here are recipes, not claims of successful execution on
every platform. The [stage-one ledger](reviews/modular-foundation/stage-one-verification.md)
records the earlier changes and does not qualify these new packages.

**Historical verification (2026-09-17: byte queues and frame timing).** The integrated source was
`44b38dbf0fcf5d341675949cf6f41d0476c4e5c0`. The following manager-owned durable
runs have terminal success on local Windows x64 with Rust/Cargo 1.97.1:

| Command from the integration root | Run ID | Result |
| --- | --- | --- |
| `cargo fmt --all --check` | `ccd33e1d-0269-435a-a96f-8800a3c1f2a9` | Exit 0. |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | `687088a0-0d5a-4806-81b3-eb5f709528c3` | Exit 0. |
| `cargo test --workspace --locked` | `24206c7b-34b8-4733-b993-7c53f61c4a15` | Exit 0; 324 unit, 1 integration and 5 doc tests passed, none failed or ignored. |
| `cargo tree --locked -p allmystuff-frame-timing -p allmystuff-byte-queues --edges normal,build` | `639df759-da0a-49fe-b775-8ba9ced0080e` | Exit 0; timing has no dependencies; queues include only the parking_lot/tracing dependency trees. |
| `cargo fmt --manifest-path node/Cargo.toml --all --check` | `95462032-ed4f-4006-8cc8-1a95171da36e` | Exit 0. |
| `cargo clippy --locked --manifest-path node/Cargo.toml --all-targets -- -D warnings` | `e4421b39-b20a-4919-bc13-14f2c40c388c` | Exit 0; default node targets compiled/linted, no node tests executed. |
| `cargo check --locked --manifest-path node/Cargo.toml --all-targets --no-default-features` | `87cc1632-0bc3-4158-bcb7-a79c2aa8121f` | Exit 0; node targets compiled with defaults disabled, no node tests executed. |

Those two libraries account for 11 of the 330 passing executions: eight unit
tests, one public API integration test and two doc tests. Both node logs have
no warnings; all seven runs retained complete, untruncated output.

Those central runs used `CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`
and `CARGO_INCREMENTAL=0`, with a shared `CARGO_TARGET_DIR` in the manager's
worktree. Native checks use its existing Visual Studio CMake and
`CMAKE_POLICY_VERSION_MINIMUM=3.5`; no worker duplicates these builds.

Existing desktop/mobile lockfiles still contain local-package version drift
(`0.2.118`/`0.2.119`); that extraction only added its two new package records and
node dependency edges there. It does not qualify their locked builds. Full
`just check` and broad node test execution were not run: some node tests reach
`Mesh::new`, persisted stores and providers, and disposable runtime isolation
has not been verified. GUI/mobile apps, devices, other operating systems and
the declared Rust 1.88 minimum remain untested by this extraction. The source
move preserves behavior; these checks do not qualify the running application.

**Update-policy/pacing verification boundary.** This extraction started
from `a336412725bbb34a2e0b89833c351bd386392967`. Its frozen references are the
old updater policy blob `2b3c97774965730a4ec090cc2fdd7f0b4eca1ca5` and node pacing
blob `cf930bde99554303553c5e103b3a224723f34410`. Reviewed source commits are
`d0dec09157f15ff56e1c81c36251361c6afea77a` (update policy) and
`41f610959857cb52caa1f05707bcd29dd958ceef` (video pacing).
The dated results above precede
these extractions and are not their validation evidence.

**Historical verification (2026-09-18: update policy and video pacing).** The
assembled implementation and Cargo wiring were checked at
`758fadf4950be52834bc4e361e47f10516f9e1e5` on local Windows x64. The manager
reports Rust/Cargo 1.97.1 and the same reduced-debug/nonincremental settings
listed above, with its shared target directory and existing Visual Studio
CMake. Retained run records establish platform, commands and source identity;
toolchain versions and environment values are manager-supplied context.
Commands below use the manager's `C:\Users\Admin\.cargo\bin\cargo.exe`.

| Command | Durable run | Inspected result |
| --- | --- | --- |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | `24074578-969c-4acc-980a-6f079e2ec663` | Exit 0. |
| `cargo test --workspace --locked` | `6f0e1d36-0886-414f-bab8-28a52cd57fe9` | Exit 0; 344 passed, none failed or ignored. |
| `cargo tree --locked -p allmystuff-update-policy -p allmystuff-video-pacing --edges normal,build` | `282d3377-6e04-4219-afd0-a61d88d6ed75` | Exit 0; policy has only the Serde/derive dependency tree; pacing has no dependencies. |
| `cargo clippy --locked --manifest-path node/Cargo.toml --all-targets -- -D warnings` | `5e271ccf-2076-4008-918f-b7f6293e748b` | Exit 0; default node targets compiled/linted, no node tests executed. |
| `cargo check --locked --manifest-path node/Cargo.toml --all-targets --no-default-features` | `790137d4-6ae4-4790-b384-30e9e83ddde9` | Exit 0; node targets compiled with defaults disabled, no node tests executed. |
| `cargo fmt --all --check` | `4256ed5d-44ef-464e-aa07-3a22d286fe86` | Exit 0 at the formatting/documentation descendant below. |
| `cargo fmt --manifest-path node/Cargo.toml --all --check` | `e761a2f5-fbb0-40d1-bcc6-0c5749575098` | Exit 0 at the same descendant. |

The pair contributes 17 passing executions: seven original unit tests, nine
public integration tests and one policy doc test. Initial root/node formatting
runs `d52c5592-a2ab-4ccb-abfe-8dec4737627b` and
`f6b66c7c-123c-4a96-8dc4-fa244b478d20` exited 1 for the same two assertion-wrapping
hunks in the policy public tests. Independently reviewed correction
`8467d4858b969310761d9b823a497a5053bc38d1` changes only whitespace. The successful
formatting runs above use `149c5acc632d2f4d3ced8e577b7a2c00791a814a`, whose only
differences from the compiled/tested commit are that correction and this guide.
All seven successful runs retain complete, untruncated output; the two node
compilation logs contain no diagnostic warnings or errors.

Temporary differential checks read those original files from Git and compare
the new libraries against them using the public tests' fixed inputs. Pacing
compares all six traces with one shared `Instant` anchor; update policy also
compares cross-pairs of its version inputs and policy/serde cases. The old
implementation is generated only into disposable verification output, not
shipped as a permanent test oracle. Golden expectations remain independent
of that comparison. No worker builds or live application tests are implied.

Initial policy/pacing differential preparations
`7c7bef78-a822-49bd-b75f-2c0b0373cc61` and
`5deb990a-7b40-4032-8a06-442ded110dd3` exited 1 at the frozen-byte guard before
compilation or comparison. PowerShell's native-output text decoding changed
non-ASCII source comments. The corrected disposable scripts copy raw Git stdout
bytes and retain the same identity guards and inputs. Both subsequent runs
at `149c5acc632d2f4d3ced8e577b7a2c00791a814a` succeeded with complete, untruncated
output:

| Differential command | Durable run | Inspected result |
| --- | --- | --- |
| `powershell.exe -NoProfile -NonInteractive -File <C1>\target\run-policy-differential-v2.ps1 -RepoRoot <manager>` | `0db68670-54fa-41dd-ad0a-284b6dd154dc` | Exit 0; 42 fixed version inputs, 1,764 ordering comparisons, 7,056 decisions, 14 parse/serialization and 19 deserialization cases match. Scratch external versions/checksums match the root lock; root lock unchanged. |
| `powershell.exe -NoProfile -NonInteractive -File <C2>\crates\allmystuff-video-pacing\target\verification\compare-baseline.ps1 -Repository <manager> -Rustc C:\Users\Admin\.cargo\bin\rustc.exe` | `e76a4e14-1a27-4055-a713-cde12367e996` | Exit 0; 56 equal outputs across policy/frame/debt/policy-change/boundary/large traces (14/11/8/8/10/5). |

These were disposable verification commands, not shipped tooling. Their exact
executable was `C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe`;
`<C1>`, `<C2>` and `<manager>` denote `9b1bd3f4`, `87ba9bd9` and `1503a621`
under `C:\Users\Admin\AppData\Roaming\AllMyAgents\data\worktrees`. Both ran in
the manager worktree. Reviewed script SHA256 identities are
`a57b7b65788a49ce056981e4b720a572aabb513836d1794a66bdd687c4920027` (policy v2)
and `6cda369e68ae19c31b98341122e4c4e70cf51f9dc561820aaa558769d830f634` (pacing).
The policy driver remained
`ba98b35e4aaea844e642f1afb1e4cca511fcfb2441c257ceb2ee3afcaba609a7`.

That pair retains the earlier execution limits: no full `just check`,
broad node runtime tests, GUI/mobile execution, device tests, other-OS builds
or Rust 1.88 qualification. Existing desktop/mobile `0.2.118`/`0.2.119` local
package drift remains; their lock changes add only this pair and dependency
edges. Compilation and pure comparisons do not qualify live updates, media,
Mesh transport or the running application.

**Inventory-model/metadata verification boundary.** This extraction starts
at `89326b5055b01848c8823b584a102078d67a5039`. The canonical model retains the
exact original types blob `18fee02c087366c6ee7b244dcd173bd8c8c44e34`; the frozen
video-wire reference is `1b6fea24996ae21e95749d83daa08bc4c7eeb9e2`. Independently
reviewed source commits are `71586c74b52584574625daaa8ab15b2413c84ab8` (model,
compatibility and bridge fixtures) and `a1c991a82a66b01593c2e402386572ea771faf24`
(metadata). Shared Cargo wiring is independently reviewed commit
`ba05a7086520ae82be4a157aa9316f310f90b56a`. The historical results above do not
validate this new pair.

Baseline Windows bridge dependency run `5dd88a3c-3445-4289-ac9c-17e65d3bf6e3`
used `cargo tree --locked --offline -p allmystuff-bridge --edges normal,build
--target x86_64-pc-windows-msvc` and included the scanner, `sysinfo` and `wmi`.
At integrated `d30c3f73604745fd5d786d70079f6b180b288f8a`, library graph run
`4c3cf3db-b798-40a1-9024-b8b40e9275e6` used `cargo tree --locked --offline
-p allmystuff-inventory-model -p allmystuff-video-metadata --edges normal,build`
and passed in 0.302 s (737/0 output bytes):
the model has only the Serde/derive closure; metadata has `memchr` 2.8.1.
Windows bridge graph run `9964fcb3-40d1-4bd8-8943-a9e1ce88be1b` passed in
0.295 s (1,965/0 bytes) with the same baseline command. The scanner, `sysinfo`
and `wmi` are absent; graph, protocol and `dirs` dependencies remain.

Initial root/node format runs `6f261e3d-db49-41bd-9ca6-8656e3c03d6b` and
`bc3d5f3c-b44f-43d0-a01e-5c853860569f` failed with identical 5,372-byte reports
limited to wrapping and optional trailing commas in three new test files.
Peer-reviewed corrections are `04225eff37c884d78ee358d603362eaa70bbda9f`
(model) and `99e73c1a7274210541d93bedb4dddb3cacd4f867` (metadata). The resulting
assembled tree is `b420beb8b6e9bc435fa29c837c91f6506522c050`; its only other
change from the graph-tested tree is the reviewed reuse guide. Implementations,
fixture values, callers and Cargo resolutions are unchanged. The initial format
failures are resolved by the successful root/node reruns below.

**Central verification (2026-09-18: inventory model and video metadata).** Runs
below use the frozen assembled tree `b420beb8b6e9bc435fa29c837c91f6506522c050`
on local Windows x64. The manager reports Rust/Cargo 1.97.1,
`CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`, `CARGO_INCREMENTAL=0`
and `CARGO_TARGET_DIR=C:\Users\Admin\AppData\Roaming\AllMyAgents\data\worktrees\1503a621\target`.
The manager-supplied native
settings are `CMAKE=C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe`
and `CMAKE_POLICY_VERSION_MINIMUM=3.5`. Retained provenance records the platform
and environment keys/hash; these version and environment values are the
manager's measured context. The executable is
`C:\Users\Admin\.cargo\bin\cargo.exe`, from the integration worktree. Every
following run succeeded with exit 0 and complete, untruncated retained streams.

| Check / exact Cargo arguments | Durable run | Result | Stdout/stderr bytes |
| --- | --- | --- | --- |
| `fmt --all --check` | `39a384db-b5c4-461e-bc46-0fbe9d634a98` | 0.466 s | 0/0 |
| `fmt --manifest-path node/Cargo.toml --all --check` | `1e3d992c-f5c4-4131-af9c-2ebb7e3f7b77` | 1.866 s | 0/0 |
| `clippy --workspace --all-targets --locked -- -D warnings` | `6f50b69d-50d6-4915-9de4-f2ff19d45211` | 40.782 s | 0/7,842 |
| `test --workspace --locked` | `b09304ef-5be6-4b65-99d2-86b51d35eb3f` | 358 passed, 53.304 s | 27,644/10,024 |
| `clippy --locked --manifest-path node/Cargo.toml --all-targets -- -D warnings` | `e63a3693-30d1-4c26-bfe0-4ede05997030` | 90.419 s | 0/9,585 |
| `check --locked --manifest-path node/Cargo.toml --all-targets --no-default-features` | `c6441311-3509-4218-9b4f-b52a2bf86fb4` | 41.694 s | 0/1,419 |
| `test --locked --manifest-path node/Cargo.toml --lib au_identity` | `0b3ea782-8db3-4b4e-adea-602d73149b4d` | 5 passed, 93.041 s | 526/8,753 |
| `test --locked --manifest-path node/Cargo.toml --lib video::tests::splitter_cuts_only_at_slices_and_partitions_exactly -- --exact` | `d2ef0b3c-d411-40b6-82c1-f7e68c16946a` | 1 passed, 0.724 s | 192/152 |
| `test --locked --manifest-path node/Cargo.toml --lib control_client::tests::` | `4987a328-31e8-44cb-a1d7-717bbb27c8ec` | 13 passed, 0.720 s | 1,315/152 |
| `test --locked --manifest-path node/Cargo.toml --lib video::tests::openh264_accepts_paced_slice_chunks_incrementally -- --exact` | `7c052c2d-8cb7-463a-b322-4655207efd02` | 1 passed, 0.835 s | 190/617 |
| `test --locked --manifest-path node/Cargo.toml --no-default-features --lib control_client::tests::` | `052d2a1d-b3fd-48bb-9709-91d156c0433f` | 13 passed, 51.350 s | 1,315/1,483 |

The root total is 328 unit, 23 integration and 7 doc tests, all with zero
failures/ignored tests. This includes the bridge's nine existing tests and 14
new extraction checks: seven model fixtures/helpers, one scanner reexport
identity test, five metadata fixtures and one metadata doctest. Existing root
scanner tests still run; the new identity test only checks types/signatures.
The five filtered node runs add 33 passing test executions with zero failures
or ignored tests; other node tests remain filtered out. The control-client
suite executes 13 tests in each configuration, not 14.

Node Clippy/check compile the integration and test targets; they do not run
the full node suite. The targeted H.264 test uses ten generated 640x480 frames
and the software OpenH264 encoder/decoder. It passed with three encoder
parameter warnings: max-NAL size takes precedence over the 4096-byte slice
constraint, and adaptive quantization/background detection are disabled for
screen content. This is software codec/marker compatibility evidence, not
capture, hardware encoder or live-media qualification.

The frozen metadata comparison uses disposable raw Git-byte export and the
public tests' fixed inputs, keeping the original implementation out of shipped
test oracles. Its reviewed runner SHA-256 is
`65015f580698931eeb73246a05574391ee396bddd6c93563195906e4ec630c37`; the driver is
`2e526657dbc968de52ea16588061579061a25439634b80bc8e66ce51a71e573c`. The formatting
correction changes only the expected vector blob to
`4535a1b385ee21de825f438024aed5df8e56a356`, retaining identical inputs.

Durable comparison `3657c1d2-0d3a-4d60-8ae1-510e503165c1` passed at the same
assembled tree in 1.552 s (537/434 output bytes), using this exact command from
the manager worktree:

```powershell
& 'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe' -NoProfile -NonInteractive -File 'C:\Users\Admin\AppData\Roaming\AllMyAgents\data\worktrees\9b1bd3f4\target\run-metadata-differential.ps1' -RepoRoot 'C:\Users\Admin\AppData\Roaming\AllMyAgents\data\worktrees\1503a621'
```

All 130 fixture/trace cases matched the frozen reference: eight scans, ten
insertions, 27 parsing cases, 83 truncated prefixes and two repeated-insertion
traces. Bytes, offsets, identities and sequential removals were equal. The
disposable offline consumer retained the integration lock's external package
versions/checksums and left the root lock unchanged.

These are manager-owned local runs; no worker Cargo execution was used. No
full `just check`, broad node runtime suite, GUI/mobile execution, live Mesh or
media, device testbed, other-OS build or Rust 1.88 qualification is claimed.
Broad runtime tests remain outside this slice because persisted stores and
providers lack verified disposable isolation. Existing desktop/mobile
`0.2.118`/`0.2.119` local-package drift stays in place; all pre-existing
registry/git resolutions are unchanged, with only the new local packages and
intended dependency edges changed. The extraction does not qualify a future
MyOwnMesh v1 integration.

After verification, manager cleanup `181ee090-c765-4a7c-b9fd-ee476f89be90`
removed 7,521 regenerable target files (3,494,982,880 bytes). Both manager
target directories are absent; source, retained logs and worker-authored
comparison inputs remain.

**Revisit with the MyOwnMesh v1 contract.** These flags remain open:

| Boundary | Work to reconcile later |
| --- | --- |
| Legacy daemon adapter and capability publication | The app still consumes the old pin and built-in capability/protocol stack. Review replacement transport and richer registration contracts before changing that adapter. These packages do not implement a custom-app registry. |
| App registration and identity grants | The proposed MyOwnMesh-owned app approval, identity custody and grant issuance/revocation work is in progress. AllMyStuff consumes scoped authority and applies capability policy; it must not issue or broaden Mesh authority. No upstream method names or finalized API are assumed here. |
| Local queue packing and future routes | The queue's `u32` records are local IPC packaging. They do not specify a future Mesh channel/route format. Keep application payload/framing decisions distinct from transport packetization and route ownership. |
| Resource policy and admission | Revisit injectable queue limits, notification semantics, aggregate memory/accounting and backpressure with application requirements and Mesh resource admission. Preserve the current drop policy until a separately reviewed change supplies the required boundary tests. |
| Timing and observability | Timing remains local application measurement. Future transport metrics may correlate frame identities, but must not turn these durations into cross-host clock arithmetic. |
| Application pacing | These rate/burst calculations and bucket reservations remain separate from Mesh transport, congestion control and backpressure. Review the eventual v1 integration contract later; this extraction changes no transport behavior and assumes no new upstream API. |
| Inventory and evidence | Hardware snapshot records are application data; they do not grant authority or implement Mesh evidence admission or capability registration. The optional host still chooses scanner and consumer components. |
| Encoded AU metadata | Preserve the application's marker/sequence/recovery representation when reviewing future routes. These bytes do not redefine Mesh-owned RTP markers or packetizer mechanics. |

The [source-checked MyOwnMesh follow-ups](reviews/modular-foundation/myownmesh-followups.md)
narrow three candidates: first-fragment AU bounds, explicit daemon contract
support, and a measured outgoing-copy optimization. They remain proposals;
the current portability work changes none of those behaviors or the Mesh pin.

The [original roadmap](MODULAR-FOUNDATION.md) and its reviews remain historical
evidence. Its proposed consumer-profile framing is superseded by selective
libraries, an optional host and a GUI consumer; the broader implementation and
platform qualifications have not been supplied by these library extractions.
