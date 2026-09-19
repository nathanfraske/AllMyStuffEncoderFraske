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

## Central validation recipe and limits

Workers perform source/fixture review only. Jackson owns durable execution.
Use unique synthetic named pipes on Windows, or a socket beneath a disposable
private directory on Unix. Each case has bounded awaits and owns its listener,
streams and runtime; no production node, daemon or live endpoint is involved.

From the integrated root, the minimal Windows client gate is
`cargo test --locked -p allmystuff-node-client --lib --test public_api -- --test-threads=1`.
Follow with `cargo test --locked -p allmystuff-term --lib` and a native node
compile/check covering the Windows owner-inspection caller. Retain the manager's
single shared target directory and existing native build environment to avoid
redundant dependency builds. These commands are proposed for manager execution;
they have not been run by the worker.

For the library's actual target closure, record
`cargo tree --locked -p allmystuff-node-client --edges normal,build --target x86_64-pc-windows-msvc`.
It must not include the node, GUI, native codecs or capture stack. This is
distinct from claiming the desktop no longer depends on those packages. Any
native node check and caller-test results should be reported separately from
the new library tests, with exact commit and terminal status.

Unix runtime coverage is pending an available approved native Rust toolchain.
The manager found no Cargo/rustc in WSL and does not authorize expanding this
task into provisioning or RISC-V execution. Unix address/path parity will be
reviewed from source and qualified separately from Windows runtime evidence.

Status: baseline and test implementation prepared; source review has no remaining
finding after the connection-method repair. Test peer acceptance and central
execution remain pending. Formatting parsed the two new test files without
touching the frozen method bodies; this is not a type check or runtime result.
