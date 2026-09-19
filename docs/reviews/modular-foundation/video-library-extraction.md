# Unified video library: independent compatibility review

C2 baseline: `511c55be94cc6988836f4a423bb8d2929e31c36b`.
The operator authorized one reusable `allmystuff-video` package with internal
modules and optional backends, preserving existing application behavior.
Existing helper libraries may remain implementation dependencies. This work
does not resume RISC-V diagnostics, doctests, storage extraction or protocol
upgrades. Workers prepare and review source/tests; Jackson owns execution.

## Independent baseline

The [manifest](../../../crates/allmystuff-video/tests/baseline/manifest.json)
records 21 exact source identities and twelve verbatim selected snapshots.
These were frozen before reading any new library implementation. Original
worker and incremental OpenH264 tests are retained as source snapshots too;
their presence is not evidence of execution. The snapshots are not yet wired
as compiled test modules. Any later import-path or fixture adapter must be
recorded separately without silently updating the original behavior.

The independent literal fixtures contain
[15 splitter cases, 23 classifier cases and five markers](../../../crates/allmystuff-video/tests/baseline/byte_vectors.json),
[14 sequence cases](../../../crates/allmystuff-video/tests/baseline/sequence_vectors.json)
and [six handoff traces](../../../crates/allmystuff-video/tests/baseline/handoff_vectors.json).
Expectations were derived from the original source, not captured from the new
implementation or claimed as executed tests. Source/oracle hashes use Git's
UTF-8 LF bytes; normalize checkout CRLF when checking receipts.

## Behavior that must remain distinct

| Boundary | Frozen behavior |
| --- | --- |
| Host and receive-only splitters | Preserve their separate walkers. For `[0,0,1,0,0,1,0x65,0,0,1,0x61]` at cap 7, host ranges are `0..7, 7..11`; stub range is `0..11`. The four-byte-prefix analogue is also frozen. These malformed-input differences are source-derived, not an authorized correction. |
| Split grouping | Cut only at slice units, keep preceding parameter/SEI runs with their slice, retain trailing metadata, leading bytes and oversized single units. Empty input still returns one `0..0` range. Preserve exact parameter bytes so H.264 `0x41` is not mistaken for HEVC; parameter-less HEVC and AV1 remain whole when no slice boundary is recognized. |
| Paced marker | Exact 26-byte prefix/UUID/trailer, little-endian `u16` count. Every count including zero is accepted; builder counts above 65,535 saturate. Other shapes and changed fixed bytes are rejected. |
| Codec and clean entry | A classifier is not full bitstream validation. Preserve exact HEVC header matches and the `0x28` H.264 collision, first recognized identifier, and Annex-B precedence over AV1. Preserve existing AV1 acceptance of reserved-bit, absent extension/body and eight-byte continuation forms; do not replace it with a stricter parser incidentally. |
| AU sequence | Duplicate equality is rejected even on a clean entry; clean entries can reset other stale sequence values. Preserve wrapping increment and the half-range gap/stale boundary using `u64`. |
| Handoff | Preserve IPC key detection from bytes `[2,1]`, little-endian batch lengths, payload plus metadata accounting, the inclusive 200 ms residence threshold and 64 MiB bound. Drop only to a fresh complete key suffix, fence dependent deltas after loss, preserve that fence across draining, and coalesce gradual recovery until drain. `replace()` retains its internal `Instant::now()` call. |
| Ingress and route assembly | Ingress canonicalizes peer/lane, uses a one-second inactivity check and changes state according to actual channel sent/full/closed feedback. Route assembly is route-keyed and lacks that age check. Both retain timestamp/count/continuity rules and the current first-fragment bound limitation; extraction does not silently repair it. |
| Integration | Authentication, route generation, negotiated pacing, callback ownership, recovery-message delivery and socket/device tasks stay in their adapters. Assembly does not confer sender authority. Preserve logging targets, fields and environment-driven backend selection as well as returned data. |

The original implementations are in the frozen source ranges from
`node/src/video.rs`, `node/src/stubs/video.rs`, `node/src/video_decode.rs`,
`node/src/video_handoff.rs`, `node/src/control_client.rs` and `node/src/mesh.rs`.
The existing metadata, timing and pacing libraries remain separately identified
inputs, rather than being treated as previously untested new algorithms.

## Fixture and backend execution boundaries

The first agreed public module names are `metadata`, `timing`, `pacing`,
`framing` and `codec`. Framing exposes `split_annexb_paced_host` and
`split_annexb_paced_stub` separately. Public tests will use those interfaces;
any necessary private queue-limit fixture will use a `cfg(test)` seam rather
than adding a production option merely for a test.

Existing decoder tests distinguish pure policy, worker lifecycle and actual
codec work. `disconnected_decoder_route_restarts_on_next_feed`,
`fresh_decoder_started_on_delta_requests_key` and preference replacement use
synthetic inputs and owned worker threads. Backend strike/demotion state tests
do not establish successful hardware operation. Preserve stop/join behavior,
clean-entry requests, pending capacity 12, strict automatic/software preference
and zero-output/failure recovery through the move.

`decodes_what_the_encoder_produces` and
`h264_route_rebuilds_across_resolution_change` generate real OpenH264 input but
select `DecoderPreference::Automatic`. On Windows with the current host
feature this can open NVDEC, despite the round-trip test's software-oriented
comment. An isolated central software gate should use the existing
`ALLMYSTUFF_H264_DECODER=openh264` override or a software-only feature build,
without changing the original test bodies. The host's
`openh264_accepts_paced_slice_chunks_incrementally` is a separate real-codec
test with its original synthetic content and slice accounting.

`hevc_stream_decodes_through_bridge` opens GPU/NVENC/NVDEC resources and can
return successfully when unavailable. It belongs to a separate hardware gate;
a passing early return is not evidence of hardware coverage. No worker has
run a compiler, test, emulator, live endpoint or device probe for this slice.

## Review and validation status

Baseline source and literal expectations are frozen. Public compatibility tests
and production boundary review are in progress. The new package must have no
reverse node/Mesh dependency; optional native backends must stay outside the
default core dependency closure, while node default/no-default behavior stays
unchanged. Exact manifests, lock changes, source adapters and central commands
will be reviewed before execution. No new implementation or runtime result is
accepted by this baseline checkpoint.
