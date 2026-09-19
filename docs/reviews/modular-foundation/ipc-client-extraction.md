# Node IPC client extraction: independent compatibility review

C2 baseline: `1bb9bff6a3d7598565513bd4496bab8faf3b4a8f`.
The operator authorized this extraction with no behavioral change. This review
and its fixtures concern the local application node client, not the MyOwnMesh
transport, media policy, storage or experimental RISC-V work.

## Frozen baseline before implementation adaptation

The independent [oracle manifest](../../../crates/allmystuff-node-client/tests/baseline/manifest.json)
records source Git blobs, SHA256 values and included line ranges. The node and
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

The client checks will use scripted peers independent of the new serializer
and compare the frozen clients with the extracted facades. Relevant differences
must remain visible: node `anyhow` context chains versus terminal `String`
errors; different wrong-tag wording; node warnings versus terminal silence;
and the original warning target `allmystuff_node::node_control`. Oracle tracing
call sites naturally live in the test module, so their message/level/fields
are compared separately from the extracted node's required target.

The planned cases cover JSON defaults/nulls, JSON/raw response and error
handling, subscribe acknowledgement and rejection, malformed/unknown events,
JSON `Restart` continuing versus a tag-3 restart terminating, writer-half
lifetime, one connection per request, channel backpressure and disconnect.
Tests will not call the production `new`, `probe` or `wait_for_socket` helpers,
change process-global home variables, or bind/probe `allmystuff-node`.

## Validation boundary

Workers perform source/fixture review only. Jackson owns durable execution.
Use unique synthetic named pipes on Windows, or a socket beneath a disposable
private directory on Unix. Each case has bounded awaits and owns its listener,
streams and runtime; no production node, daemon or live endpoint is involved.

The first proposed gate is the new crate's unit and external integration tests
on Windows, followed by the existing terminal tests and the necessary node
compatibility checks. Confirm the lightweight normal/build dependency closure
does not acquire the node, GUI, native codecs or host capture stack, and confirm
existing registry/git pins remain unchanged. Existing desktop dependencies on
node lifecycle, daemon discovery/repair and diagnostics are expected to remain.

Unix runtime coverage is pending an available approved native Rust toolchain.
The manager found no Cargo/rustc in WSL and does not authorize expanding this
task into provisioning or RISC-V execution. Unix address/path parity will be
reviewed from source and qualified separately from Windows runtime evidence.

Status: baseline frozen; test implementation, peer review and central execution
remain pending. No test/build result is claimed by this document yet.
