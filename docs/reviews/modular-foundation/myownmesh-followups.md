# Source-checked MyOwnMesh follow-ups

This records C2's independent cross-check of A1's three bounded proposals.
AllMyStuff application source was reviewed at
`723bf8f5664ab56ff842f4b7c67f1bea3bd7dfd1`; `.myownmesh-rev` is `v0.3.21`.
This review changes documentation only. It supplies no runtime reproduction,
performance result, dependency upgrade, or authority to change daemon behavior.
The RISC-V compilation and emulator qualification remain separate work.

The upstream comparison uses exact commit
`db7818e09fedd98899490347b86ac9bc9f97b59b` in
`nathanfraske/MyOwnMeshSecurityReview`. [Upstream PR #135](https://github.com/mrjeeves/MyOwnMesh/pull/135)
was marked Draft when checked for this review. Its staged implementation is a
reference for future integration, not the contract currently pinned here.

## 1. Apply the existing paced-AU limit to the first fragment

[`InboundVideoFreshness::forward_paced`](../../../node/src/control_client.rs#L173)
checks the 16 MiB `MAX_PACED_AU_BYTES` limit when appending to an existing AU
with the same timestamp. New-lane insertion and timestamp replacement store
the incoming frame with `chunks: 1` without checking its size. The replacement
path queues a discontinuity but retains the replacement frame. A matching
timestamp/count marker passes the assembled frame to the ordinary forwarding
and recovery path without another byte-limit check.

The outer reader admits bodies up to the distinct 64 MiB
[`MAX_MEDIA_FRAME_BYTES`](../../../crates/allmystuff-protocol/src/control.rs#L590)
limit, and `decode_inbound_frame` copies their payload before paced assembly.
Consequently, source inspection identifies a gap in the existing per-AU
retention bound. This is not a demonstrated exploit or an aggregate-memory
bound for all lanes.

A separately authorized fix can check the first/replacement fragment against
the existing limit and preserve ordered discontinuity/recovery behavior.
Acceptance should cover exactly-limit and limit-plus-one cases for initial,
replacement and continuation fragments; a matching end marker; no oversized
pending or emitted AU; existing queue-pressure/recovery cases; and independent
lane/audio progress. Aggregate accounting remains a separate policy decision.

## 2. Distinguish a version minimum from daemon contract support

[`ensure_daemon_current` and `log_daemon_version`](../../../node/src/daemon_spawn.rs#L477)
treat a parsed version greater than or equal to the pin as satisfying its
numeric minimum. `reuse_running_daemon` logs the version and returns
`Ok(None)`; that comparison does not negotiate a protocol contract. Startup
also tolerates some older or unreadable versions, so it is not a strict
compatibility gate.

There is already a narrower capability check:
[`mesh.rs`](../../../node/src/mesh.rs#L3408) reads `Status.media_pipes`. A true
boolean selects the binary media path. Missing, malformed or failed status
responses resolve that flag to false, retaining legacy JSON/base64 sends.
AllMyStuff still defines `MediaTrackPipe`, `MediaSourcePipe`, `VideoSend` and
`AudioSend` in its [daemon requests](../../../crates/allmystuff-protocol/src/control.rs#L251).

The exact staged [daemon wire contract](https://github.com/nathanfraske/MyOwnMeshSecurityReview/blob/db7818e09fedd98899490347b86ac9bc9f97b59b/crates/myownmesh/src/control/wire.rs#L495)
retires the JSON media sends and defines capability-scoped
`RealtimeFlowOpen`, `RealtimeFlowClose` and `RealtimePipe`. Outbound flow
authority is tied to an exact session and client. The inference from these
different interfaces is that passing the numeric minimum, or falling back
after a missing `media_pipes` flag, cannot establish compatibility with that
newer contract. This review does not claim today's pinned daemon fails.

A future compatibility/readiness change should have a fake-daemon matrix for
the supported legacy contract, missing/malformed/error status, unsupported
required operations and an incompatible newer contract. Keep any intentional
older-daemon fallback explicit. Unknown contract support should be visible;
no automatic pin change, foreign-daemon restart or authority fallback follows
from this proposal. Degraded Serve IPC tests with a failed helper establish
neither legacy nor v1 Mesh-session compatibility.

## 3. Measure one outgoing body copy before changing it

Binary H.264/Opus IPC already exists when `media_pipes` is true; the
[video/audio send branches](../../../node/src/mesh.rs#L2848) still include
their legacy base64 fallback. A wholesale switch from base64 is therefore
not the proposed optimization.

[`MediaTrackPipe::send_frame`](../../../node/src/control_client.rs#L849)
calls [`encode_media_frame`](../../../crates/allmystuff-protocol/src/control.rs#L607)
before acquiring the writer lock. The encoder allocates a header-plus-payload
`Vec` and copies the whole payload. The pipe then writes the length and body
under one lock, with separate audio/video deadlines and reconnect on a write
error or timeout.

A measured alternative could build the header separately and write a borrowed
payload while retaining that lock. Compare allocations/copied bytes and elapsed
time with representative AUs, paced slices and audio units; additional writes
may outweigh the saved copy. Acceptance includes byte-identical framing,
empty and UTF-8 IDs, checked representable lengths, partial writes,
concurrent producers, and existing timeout/error/reconnect behavior. Preserve
separate audio/video pipes. No speedup or end-to-end zero-copy claim is made.

## Boundaries for eventual v1 work

The exact [application embedding API](https://github.com/nathanfraske/MyOwnMeshSecurityReview/blob/db7818e09fedd98899490347b86ac9bc9f97b59b/docs/APPLICATION-API.md#opaque-and-webrtc-realtime-flows)
is distinct from daemon IPC. It exposes opaque and WebRTC flow APIs with
move-only, session-bound handles and an awaited close after successful open,
including send refusal. An adapter must preserve that ownership instead of
re-resolving a peer and label after reconnection. Advertisement metadata is a
hint, not authorization or proof of a complete application registry.

AllMyStuff retains responsibility for codec payload units and its AU metadata.
Future transport/RTP, route and authority mappings require their own explicit
contract review. These three proposals narrow the
[existing modularity flags](../../MODULAR-LIBRARIES.md)
without authorizing a broad adapter rewrite for RISC-V portability.
