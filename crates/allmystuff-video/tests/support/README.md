# Compatibility fixture adapters

The source receipts in `../baseline/manifest.json` refer to commit
`511c55be94cc6988836f4a423bb8d2929e31c36b`. Baseline files remain unchanged.

`core_compatibility.rs` and `receive_compatibility.rs` are public integration
tests. They include frozen source through test-only modules; the root
`video_wire`, `video` and `video_frame_timing` paths resolve the original
implementation's imports. Route timing uses the unchanged timing helper and a
local `NoSubscriber`, avoiding comparison of elapsed wall time. Frozen module
formatting is skipped. The one dead-code allowance is limited to the whole
frozen stub framing module, whose splitter is not used by route assembly.

`handoff_oracle.rs` starts with the exact frozen `../baseline/handoff.rs`
bytes, then appends three helpers for limits, packet accounting and state
inspection. No original declaration or method body is rewritten.
`handoff_compatibility.rs` is included as a child of the production `handoff`
module so it can set the existing private limits. It compares each operation's
result, queue count, charged bytes, fence, convergence state and batch bytes
with the frozen implementation, including six separately frozen literal traces.
Small private budgets exercise the same decision paths without large buffers.
The original and new `replace()` calls keep their own `Instant::now()`; traces
do not compare those timestamps and drain before a subsequent supplied-time push.

`legacy_output.rs` is included only under `cfg(test)` as `crate::test_support`.
Its handoff policy repeats the original application packet operations: key
bytes `[2,1]` and a four-byte little-endian length prefix. Under `decode`, the
output adapter uses the frozen 28-byte IPC header and the exact arithmetic,
allocation and resize sequence from `node/src/video_decode.rs:1076–1081`.
Its mutable RGBA view is the allocation's tail. Under `host`, the inert desktop
follower is for existing isolated worker tests only; it is not a production
default and does not establish real desktop/session behavior.

`ingress_compatibility.rs` includes the frozen ingress and inbound frame bodies
unchanged. Its import shims resolve the original classifier, metadata and marker
paths to frozen source; only the unchanged timing helper is reused. A wrapper
exposes original operations and the pending-map observation. Two actual local
bounded Tokio channels supply sent/full/closed feedback; the extracted policy
receives it through a synchronous test `Sink`, with no runtime or executor.
Each step compares return value, remaining capacity and pending state, and drains
compare complete event envelopes and order against the frozen implementation.
Thirteen tests also assert independently chosen literal outcomes, including
canonical peer/lane isolation, Reset/Gradual changes while fenced, first-reason
retention, exact marker closure, bounds, and closure observed only when a send
occurs. Opaque frame kind is preserved through assembly. The one age case waits
1,100 ms during central test execution; it distinguishes stale data arrival
from a matching stale closing marker, without claiming exact threshold equality
or equal clocks. The original unchecked first-fragment size remains exercised.
Unused declarations in the frozen modules have narrow module-level dead-code
allowances; the new fixture code does not suppress warnings.

These fixtures perform no global environment mutation, endpoint access or
device discovery. Worker preparation is source-only. Central compilation,
formatting and execution remain separate evidence gates.
