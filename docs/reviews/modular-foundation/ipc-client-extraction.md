# Node IPC client extraction: independent compatibility review

C2 baseline: `1bb9bff6a3d7598565513bd4496bab8faf3b4a8f`.
The operator authorized this extraction with no behavioral change. This review
and its fixtures concern the local application node client, not the MyOwnMesh
transport, media policy, storage or experimental RISC-V work.

## Frozen baseline before implementation adaptation

The independent [oracle manifest](../../../crates/allmystuff-node-client/tests/baseline/manifest.json)
records source Git blobs, SHA256 values and included line ranges. Oracle hashes
use UTF-8 with LF endings, so normalize checkout CRLF when checking them. The node and
terminal request/event method bodies were copied from that baseline before
reading C1's implementation. Only construction/address selection is replaced
by a private fixture endpoint; engine/server/process code and global probes
are omitted. The baseline is retained even if the new implementation differs.

[Literal frame vectors](../../../crates/allmystuff-node-client/tests/baseline/wire.json)
freeze tags 0/1/2/3, unknown tags, binary bytes, length byte order, the 256 MiB
read ceiling, zero length, partial length EOF and truncated tag/payload cases.
Partial one-to-three-byte length EOF is `Ok(None)` in both original readers,
despite stricter comments in the node implementation. Fixing that is outside
this extraction.

The client checks use scripted peers independent of the new serializer
and compare the frozen clients with the extracted facades. Relevant differences
must remain visible: node `anyhow` context chains versus terminal `String`
errors; different wrong-tag wording; node warnings versus terminal silence;
and the original warning target `allmystuff_node::node_control`. Oracle tracing
call sites naturally live in the test module, so their message/level/fields
are compared separately from the extracted node's required target.

The cases cover JSON defaults/nulls, JSON/raw response and error
handling, subscribe acknowledgement and rejection, malformed/unknown events,
JSON `Restart` continuing versus a tag-3 restart terminating, writer-half
lifetime, one connection per request, channel backpressure and disconnect.
Tests do not call the production `new`, `probe` or `wait_for_socket` helpers,
change process-global home variables, or bind/probe `allmystuff-node`.

## Fixture coverage

There are eight [public API tests](../../../crates/allmystuff-node-client/tests/public_api.rs)
and eight [private-endpoint differential tests](../../../crates/allmystuff-node-client/tests/support/compatibility.rs).
The latter are included in the library only under `cfg(test)` and call the two
facades' test-only `for_test_address` constructors. They are not another
automatically discovered external test target.

The public tests check the 15 literal framing cases, short writes and flushes,
transport error propagation, consecutive frames, request/event Serde contracts
and constructor result types without calling the constructors. A length-prefix
reader returns a sentinel error before supplying a tag: this distinguishes the
inclusive 256 MiB ceiling from oversized rejection without allocating a large
payload. The historical write-side length conversion is source-checked, not
exercised with a multi-gigabyte allocation.

The client matrices run the frozen and extracted implementations against the
same scripted exchanges, with literal expected outcomes as an additional check.
They compare complete displayed error chains and underlying `io::Error` kind or
`serde_json::Error` category/line/column. The response matrix has 28 cases and
the acknowledgement matrix has 13; both run against both old/new facade pairs.
Event tests assert warning target, WARN level and message-field shape, plus
terminal silence. A tag-3 peer stays open until the client closes, distinguishing
reader termination from ordinary peer EOF.

Lifetime cases wait on explicit peer/client handshakes. A short pending read
checks the writer remains alive; ordered draining of a capacity-one channel
checks awaited sends. Dropping that receiver also checks that a blocked sender
exits and releases its connection. The fixture does not invent network write
races to force platform-dependent errors. Write/subscribe-write context mappings
and the uncallable-in-this-slice default-address failure path are additionally
reviewed from source.

## Independent source review

C2 read the extracted core, both facades, address helper, wire types, error-stage
mapping, manifest and README. Framing/type defaults and response logic match the
frozen baseline. The facade conversion retains the two error contracts and the
event policy preserves the original node tracing target. Public arbitrary
endpoint selection is not added.

One blocking finding was caught before compilation: the existing Windows
`windows_node_process_session` calls the old module-private `NodeClient::connect`
to read pipe-owner credentials. C1 exposed that operation through the extracted
node facade, delegating to the shared connection implementation with the same
error conversion. The Windows owner-inspection body and its additional context
remain unchanged. This is a necessary visibility change across the crate
boundary; it does not remove or replace the credential check.

Independent text comparison confirms the node's `SocketSink`-through-end tail,
including bind/ACL/dispatch/supervision and existing tests, is unchanged apart
from blank lines. Runtime ownership/control is also unchanged, and the terminal
`wait_for_socket` body matches exactly. Existing GUI imports remain valid through
the node reexports; its other node dependencies and mobile's embedded engine
are retained.

Independent TOML comparison of all four lockfiles found only the new local
`allmystuff-node-client` record and intended dependency edges. The terminal adds
the client edge and removes direct Serde/interprocess edges; the node adds the
client edge. All old package identities, checksums and other fields remain
unchanged. The pre-existing GUI/mobile local-version drift is retained rather
than upgraded incidentally. Root test dependencies include existing `tempfile`;
the other three lock records omit that dev-only edge.

## Central validation and limits

Workers perform source/fixture review only. Jackson owns durable execution.
Use unique synthetic named pipes on Windows, or a socket beneath a disposable
private directory on Unix. Each case has bounded awaits and owns its listener,
streams and runtime; no production node, daemon or live endpoint is involved.

The independently accepted source and fixtures were integrated at
`dc217e30cf2a2d6ef288940b12f39ed25ed3c583`, with the same tree as C1's
`a852a66cf5f8a0eefcd75bcce77f75adecdbc381`. C2 inspected the following retained
central run records and their complete stdout/stderr streams:

| Gate at that commit | Durable run | Result |
| --- | --- | --- |
| `cargo test --locked --offline -p allmystuff-node-client --lib --test public_api -- --test-threads=1` | `3521e5cc-0749-4067-be33-05c62ed3f9a3` | Exit 0, 16.451 s; eight differential and eight public API tests passed; zero failed, ignored, measured or filtered. Complete stdout/stderr: 1,457/1,933 bytes. |
| `cargo test --locked --offline -p allmystuff-term --lib` | `f7f420b4-dca8-4366-96d7-d277da9c1eee` | Exit 0, 8.437 s; all sixteen terminal library tests passed; zero failed, ignored, measured or filtered. Complete stdout/stderr: 1,091/1,636 bytes. |
| `cargo tree --locked --offline -p allmystuff-node-client --edges normal,build --target x86_64-pc-windows-msvc` | `5364af3d-756c-4092-af70-006c879b97ea` | Exit 0, 0.303 s; normal/build dependency tree recorded. Complete stdout/stderr: 3,030/0 bytes. |
| `cargo fmt --all -- --check` | `3a37f841-7227-4dea-aa4d-c7e751a8cc1b` | Exit 1, 0.780 s; requested wrapping of the `ResponseClosed` match arm only. Complete stdout/stderr: 775/0 bytes. |
| `cargo fmt --manifest-path node/Cargo.toml -- --check` | `dd2cb2b8-452d-4432-b17d-6ee6aa186357` | Exit 1, 1.103 s; requested wrapping of the new node-client reexport only. Complete stdout/stderr: 588/0 bytes. |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | `8b20a4de-7d1b-445d-91d3-a58a3cff3dee` | Exit 101, 52.902 s; `clippy::unused_unit` in the new test helper `closed()` was the sole diagnostic error. Complete stdout/stderr: 0/7,658 bytes. |
| `cargo clippy --manifest-path node/Cargo.toml --all-targets --locked --offline -- -D warnings` | `1ce03445-3fd4-49af-ad7f-96bab8b9e4ef` | Exit 0, 95.706 s; default-feature node caller check passed without warnings/errors. Complete stdout/stderr: 0/9,825 bytes. |
| `cargo check --manifest-path node/Cargo.toml --all-targets --no-default-features --locked --offline` | `8fe5a91b-1cc4-4ad3-9fb3-f575026ad30c` | Exit 0, 46.227 s; no-default-feature caller check passed without warnings/errors. Complete stdout/stderr: 0/1,515 bytes. |
| `cargo test --manifest-path node/Cargo.toml --lib --locked --offline node_control::tests::` | `a75114d2-f24e-4c7c-afeb-db58d7b9eb09` | Exit 0, 106.378 s; all sixteen selected node-control tests passed; zero failed, ignored or measured, 412 filtered out. Complete stdout/stderr: 1,276/9,989 bytes. |

The compatibility run was native Windows x64 and used private fixture pipes.
Its stderr contains compilation progress, without compiler warnings or errors.
It validates the sixteen tests described above, including both frozen client
oracles; it does not exercise the production endpoint or a running node/daemon.
The terminal run also completed without compiler warnings or errors. The
formatting failures are retained as failed attempts. C2 accepted C1's exact
two-hunk repair: a braced match arm with the unchanged error literal and a
reexport line wrap, with every other source byte and all fixtures unchanged.
It is committed as `ba4d951c1f4ba77735d97dbaceaa97edc5acb7ff`. The separate
reviewed README
clarification (`90308bac1198a1ec713f2f10d7fbfaff6c6ce865`) correctly identifies
the terminal's `client` module as private; it does not change visibility.

The initial root Clippy failure is also retained. Its requested change removes
the redundant final `()` from the branch that accepts a closed connection in
the new `closed()` fixture helper; it does not alter the frozen oracles or
suppress a lint. C1 independently accepted that exact fixture-only fix,
committed as `6767d02a011d58e18a22fb6591f1e97d6c655112`. All three corrections
are integrated at `caa0c61c5fd560d96cc14ad7e114c85161f8b746`; the initial source
and that commit differ only in the two formatting hunks, the README paragraph
and the helper's redundant unit expression. C2 inspected the complete retained
rerun streams at that corrected commit:

| Gate at `caa0c61c5fd560d96cc14ad7e114c85161f8b746` | Durable run | Result |
| --- | --- | --- |
| `cargo fmt --all -- --check` | `e1f7a817-ca97-49b0-ac88-b5ffda1b7abc` | Exit 0, 0.685 s; complete stdout/stderr: 0/0 bytes. |
| `cargo fmt --manifest-path node/Cargo.toml -- --check` | `c7c3f373-c369-4ea8-8483-dd2b7233dc1e` | Exit 0, 1.076 s; complete stdout/stderr: 0/0 bytes. |
| `cargo test --locked --offline -p allmystuff-node-client --lib --test public_api -- --test-threads=1` | `e36f247f-6cc5-4784-8f73-a08e886b3bf5` | Exit 0, 3.285 s; eight differential and eight public tests passed again; zero failed, ignored, measured or filtered. Complete stdout/stderr: 1,457/388 bytes. |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | `9389d17a-41c1-4c97-83e5-6ca3155d8fbd` | Exit 0, 2.827 s; clean root workspace/all-target check. Complete stdout/stderr: 0/1,072 bytes. |

Forty-eight distinct Windows tests passed: sixteen new compatibility tests,
sixteen terminal tests and sixteen existing node-control tests. The repeated
client gate adds no distinct test cases. The filtered
node run does not claim the other 412 tests ran or native providers were exercised.
Both default and no-default node caller checks passed. Windows owner inspection
was compiled, not run against a live node. Central commands use the
manager's single shared target directory and existing native build environment;
workers have not compiled or executed the tests.

The recorded Windows normal/build closure contains `allmystuff-protocol` and
its graph/Serde/`dirs` dependencies, plus `anyhow`, `interprocess`, Tokio and
tracing. It excludes the node, GUI, native codecs and capture stack. This is
distinct from claiming the desktop no longer depends on those packages. Node
caller validation is reported separately from the new library's tests.

Unix runtime coverage is unqualified. The manager found no Cargo/rustc in WSL;
this extraction did not provision a toolchain or resume RISC-V execution.
Unix address/path parity was reviewed from source only, including the unchanged
profile-home fallback and conversion-error mapping. The Unix socket-permission
test is not part of the sixteen selected Windows node-control tests.

Neither the GUI desktop nor mobile application workspace was built or run in
this slice. Root Clippy covers the shared `allmystuff-mobile-core` crate, not
those application workspaces. Their retained imports/dependency wiring and
mobile engine embedding were source-reviewed. The private-endpoint fixtures do
not validate live default-endpoint discovery, command authorization or daemon
lifecycle. Windows owner inspection was compile-checked rather than exercised
against a running node.

Source and fixture peer reviews are accepted, and the bounded Windows gates
above are complete. The frozen oracle modules remain excluded from recursive
formatting so their recorded method bodies stay stable.
