# Selective AllMyStuff libraries

The current direction is to select reusable libraries individually, with an
optional AllMyStuff runtime hosting capabilities and the GUI consuming them.
There are no new consumer presets or profiles in this change. The optional
host and application registration model remain future work; these small
extractions preserve the existing calculations and caller behavior.

| Library | Current contents | Direct dependencies |
| --- | --- | --- |
| [allmystuff-byte-queues](../crates/allmystuff-byte-queues/src/lib.rs) | Viewer byte queues, watcher tokens and local IPC chunk packing. | `parking_lot` 0.12, `tracing` 0.1. |
| [allmystuff-frame-timing](../crates/allmystuff-frame-timing/src/lib.rs) | `FrameCadence`, `AssemblyClock`, `SendBreakdown`, `send_breakdown` and `periodic_sample`. | Standard library only. |
| [allmystuff-update-policy](../crates/allmystuff-update-policy/src/lib.rs) | `ApplyPolicy`, exact policy-token parsing, `compare_semver` and `policy_allows`. | `serde` 1; `serde_json` is test-only. |
| [allmystuff-video-pacing](../crates/allmystuff-video-pacing/src/lib.rs) | `PacePolicy`, `PaceRouteState`, `pace_policy`, `frame_policy` and `LAN_AGGREGATE_POLICY`. | Standard library only. |

These packages do not depend on the node, GUI, codecs, capture backends or a
Mesh transport. They inherit workspace version `0.2.121`, edition 2021 and declared
minimum Rust `1.88.0`; a declared minimum is not a new toolchain qualification.
The node modules are compatibility shims: the public
`allmystuff_node::byte_queues::ByteQueues` path remains available, and existing
internal frame-timing/pacing call sites use the extracted implementations.
The updater's public `allmystuff_updater::policy` path is also a reexport shim.

**Selective use.** An application can depend on each package directly. For
a consumer beside this repository, choose the dependency lines it needs:

```toml
[dependencies]
allmystuff-byte-queues = { path = "../AllMyStuff/crates/allmystuff-byte-queues" }
allmystuff-frame-timing = { path = "../AllMyStuff/crates/allmystuff-frame-timing" }
allmystuff-update-policy = { path = "../AllMyStuff/crates/allmystuff-update-policy" }
allmystuff-video-pacing = { path = "../AllMyStuff/crates/allmystuff-video-pacing" }
```

Adjust these paths to the checkout location. Keep the repository workspace
metadata available when using these path dependencies; this example does not
claim registry publication. Public usage is exercised by rustdoc examples
and/or integration tests that import the independent crates.

Reuse the queue for a local viewer that can tolerate whole-chunk eviction;
reuse timing for local capture scheduling or assembly/send measurements. An
updater can reuse the decision helpers without network/install code, and a
video sender can reuse pacing calculations while owning its waits and state.
These choices require no running AllMyStuff host. The extractions supply no
application SDK, provider registry or identity-approval system.

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

**Focused commands.** From the repository root, after package wiring is
assembled, check the current extraction pair:

```sh
cargo test --locked -p allmystuff-update-policy -p allmystuff-video-pacing
cargo clippy --locked -p allmystuff-update-policy -p allmystuff-video-pacing --all-targets -- -D warnings
cargo fmt --check -p allmystuff-update-policy -p allmystuff-video-pacing
```

Select `allmystuff-byte-queues` and/or `allmystuff-frame-timing` instead to
check the earlier libraries. These commands are package selections, not
application profiles.

The test command covers unit, public API integration and doc tests without
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

**Central verification (2026-09-18: update policy and video pacing).** The
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

The current pair retains the earlier execution limits: no full `just check`,
broad node runtime tests, GUI/mobile execution, device tests, other-OS builds
or Rust 1.88 qualification. Existing desktop/mobile `0.2.118`/`0.2.119` local
package drift remains; their lock changes add only this pair and dependency
edges. Compilation and pure comparisons do not qualify live updates, media,
Mesh transport or the running application.

**Revisit with the MyOwnMesh v1 contract.** These flags remain open:

| Boundary | Work to reconcile later |
| --- | --- |
| Legacy daemon adapter and capability publication | The app still consumes the old pin and built-in capability/protocol stack. Review replacement transport and richer registration contracts before changing that adapter. These packages do not implement a custom-app registry. |
| App registration and identity grants | The proposed MyOwnMesh-owned app approval, identity custody and grant issuance/revocation work is in progress. AllMyStuff consumes scoped authority and applies capability policy; it must not issue or broaden Mesh authority. No upstream method names or finalized API are assumed here. |
| Local queue packing and future routes | The queue's `u32` records are local IPC packaging. They do not specify a future Mesh channel/route format. Keep application payload/framing decisions distinct from transport packetization and route ownership. |
| Resource policy and admission | Revisit injectable queue limits, notification semantics, aggregate memory/accounting and backpressure with application requirements and Mesh resource admission. Preserve the current drop policy until a separately reviewed change supplies the required boundary tests. |
| Timing and observability | Timing remains local application measurement. Future transport metrics may correlate frame identities, but must not turn these durations into cross-host clock arithmetic. |
| Application pacing | These rate/burst calculations and bucket reservations remain separate from Mesh transport, congestion control and backpressure. Review the eventual v1 integration contract later; this extraction changes no transport behavior and assumes no new upstream API. |

The [original roadmap](MODULAR-FOUNDATION.md) and its reviews remain historical
evidence. Its proposed consumer-profile framing is superseded by selective
libraries, an optional host and a GUI consumer; the broader implementation and
platform qualifications have not been supplied by these library extractions.
