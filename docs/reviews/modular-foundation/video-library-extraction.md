# Unified video library: independent compatibility review

C2 baseline: `511c55be94cc6988836f4a423bb8d2929e31c36b`.
Validated code: `a5739129854ad4dd076b9ace30e65df7b2c977ba`.
The selected Windows gates passed 126 distinct tests: 37 new compatibility
comparisons and 89 retained tests. Root and node strict lint, node no-default
checking and the corrected formatting gates passed. This evidence covers the
selected software codec, worker, queue and caller behavior; it does not qualify
hardware capture/encoding, other operating systems or all application tests.

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
their presence is not evidence of execution. The public compatibility fixtures
include frozen source with separately recorded import adapters. The private
handoff oracle appends accessors without rewriting the original bodies.

The independent literal fixtures contain
[15 splitter cases, 23 classifier cases and five markers](../../../crates/allmystuff-video/tests/baseline/byte_vectors.json),
[14 sequence cases](../../../crates/allmystuff-video/tests/baseline/sequence_vectors.json)
and [six handoff traces](../../../crates/allmystuff-video/tests/baseline/handoff_vectors.json).
Expectations were derived from the original source before execution, not
captured from the new implementation. Source/oracle hashes use Git's
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

The core public module names are `metadata`, `timing`, `pacing`,
`framing` and `codec`. Framing exposes `split_annexb_paced_host` and
`split_annexb_paced_stub` separately. Public tests use those interfaces;
the private queue-limit fixture uses a `cfg(test)` seam without adding a
production option merely for a test.

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
`ALLMYSTUFF_H264_DECODER=software` override or a software-only feature build,
without changing the original test bodies. The host's
`openh264_accepts_paced_slice_chunks_incrementally` is a separate real-codec
test with its original synthetic content and slice accounting.

`hevc_stream_decodes_through_bridge` opens GPU/NVENC/NVDEC resources and can
return successfully when unavailable. It belongs to a separate hardware gate;
a passing early return is not evidence of hardware coverage. No worker has
run a compiler, test, emulator, live endpoint or device probe for this slice.

The host test `h264_ladder_picks_a_backend_that_emits_a_frame` explicitly
opens the real encoder ladder. `h264_stream_emits_annexb_with_a_leading_idr`
does so indirectly through `H264Stream::new`, despite its `openh264 init`
expectation text. Both must be excluded from isolated host runtime evidence;
the decoder environment override does not select an encoder. The other
reviewed H264 stream/rebuild fixtures inject scripted or rate-aware codecs,
with dimensions that avoid a real-ladder rebuild.

AV1 classification and platform dispatch must be described separately from
an implemented decoder. Windows with `host` has an AV1 NVDEC/D3D11VA branch,
but both backend `open` and `decode` methods unconditionally return
`not yet implemented`; their bodies are unchanged from the baseline. Other
decode builds retain the unsupported-platform fallback, and the default core
has no decode worker. The initial review inference from dispatch alone was
corrected after checking these callees. No hardware AV1 capability is claimed
and no codec selection behavior changes.

## Review and validation status

Baseline source and literal expectations were committed independently as
`105af92d55d7ab71bec825d9266fcc121c195c43`. C1 verified every source identity,
selected range and literal-vector hash. Exact peer-reviewed public tests were
committed separately as `360d8ad9bd215475f0bd7fbd1611c85035a77849`: ten core
tests and seven route tests. Their frozen includes and adapters are described
in the [fixture notes](../../../crates/allmystuff-video/tests/support/README.md).
These initial acceptances covered source only; actual central execution is
recorded separately below.

C2 reviewed the core production stage `6480df37a47131ce56b999956315dbc9f892b4dd`.
Fifteen moved function bodies compare exactly after module-path substitutions
and the explicit original host warning target. The two walks remain separate;
the route caller retains admission, authentication and recovery delivery.
At that stage the four locks only add the local package and route the node's
three helper dependencies through it. Prior package versions, checksums and
other dependency edges remain unchanged; only the root lock includes the new
fixture's `memchr` and `serde_json` dev edges. The native/backend extension and
its separate resolution limits are reviewed below.

Generic handoff source review preserves `Packet`'s `Vec`/`Instant` layout,
charge arithmetic, allocation, queue decisions and drain/reset order. The
node policy supplies its exact key bytes and length-prefix emission. The
new `PhantomData<fn() -> P>` stores no policy value or guard and adds no
`Send`/`Sync`/`Unpin` requirement to the original node specialization. Five
original queue test bodies remain unchanged except one explicit generic charge
argument. Seven additional private-limit comparisons and test-only legacy
output adapters were independently accepted by C1 and committed as
`53f5171fd33f8b3dcbee8f612c8a41d73cef08c9`. These bring the new compatibility
tests to 24; the five preserved queue tests are separate existing cases.

Thirteen additional canonical-ingress comparisons were independently accepted
and committed as `e9e0de9f49a5d112f2fbe8de525940f2f27551a1`. These use real local
bounded Tokio channels, without an executor, to compare sent/full/closed
feedback, complete event envelopes and order, pending state and remaining
capacity. Literal expectations cover first-loss reason retention, canonical
peer/lane separation, opaque kind preservation, Reset/Gradual changes while
fenced, marker closure, transport discard and bounds. A single 1,100 ms wait
distinguishes stale data arrival from a stale matching closing marker; exact
clock equality is not asserted. Total new comparisons: 37. The five original
queue tests remain separate, giving the 42 cases in the executed core gate.

## Native and ingress source review

C2 checked all 48 paths in native commit
`18f3d9935fa93ec5f6117b35babc225e9b9a3d06` against the reviewed byte identities.
All fifteen moved native source inputs and outputs matched their receipts.
After checking explicit original tracing targets, twelve modules were
mechanical moves, with the necessary `os_perf` visibility change. The three
substantive adapter changes are in capture, Windows capture and decoding:

- `DecodeOutput` allocates the final output. The node specialization repeats
  the exact IPC header allocation, resize and writable tail; existing conversion
  calls write into it at the same points. The default library output carries
  typed RGBA metadata. Node does not gain an intermediate full-frame buffer.
- `DesktopFollower` is constructed inside the original Windows pump thread,
  at the same point and lifetime. The node implementation delegates its existing
  desktop operations. It adds no `Send` requirement or transferred handle.
- `PhantomData<fn() -> P>`/equivalent output and desktop parameters store no
  policy value. Manual `Default` retains the original mutex/map initialization,
  and worker construction, pending capacity, stop/join and callback order stay
  at their original sites. `os_perf` and `wake` have one implementation each;
  node reexports their shared registry, guards and static state.

Original host test bodies compare exactly apart from the concrete test alias.
Original decoder tests change only the output alias and test-only IPC header
path. They retain their existing expectations and hardware limitations.

All seven ingress decision methods compare exactly after declared type/module
substitutions and the supplied marker function. The node's frame/event adapters
move strings and byte vectors; the sink translates the actual synchronous
Tokio outcome at the original call site. The transport code from `MediaPipe`
through the end of the file is byte-identical after the two pending-map test
observations are changed to the wrapper accessor. No early closure check,
extra queue, async boundary, recovery reorder or authentication change was added.

## Feature, lock and execution gates

The reviewed source has one public package and no reverse node, GUI or Mesh
runtime dependency. Default features are empty; existing metadata, timing and
pacing packages remain implementation dependencies. `decode` enables the
existing software workers and capture-less surface; `host` adds the original
capture/encode/platform backend group; `hwenc` adds the existing FFmpeg ladder.
Node always enables `decode`, forwards its existing host/hwenc choices, and
retains separate audio behavior. Native backend availability remains a runtime
property, not a consequence of importing a module.

The root workspace carries the same local OpenH264 sys2 patch and the nine
native package optimization overrides used by node. Existing node/GUI/mobile
profiles remain unchanged; node's additional dev overrides for `serde_json`
and `base64` were not copied into root. This is not a claim that every root and
application profile is identical.

The reviewed root lock seed retains all 282 package identities from the core
checkpoint (281 at the original source base, plus the new video package) and
adds 241 identities already frozen in node. Every added record's dependencies
match that node input. Existing root dependency edges are unioned where needed;
the seed is not a successful Cargo resolution. Other three consumer locks
change only the local node/video dependency records and retain their existing
version drift. The read-only identity guard rejects removed old identities or
unreviewed versions/checksums; it does not by itself validate root dependency
edges. Actual central offline canonicalization and its edge/closure diff were
independently reviewed before accepting the resulting lock below.

After the first resolver conflict, C2 accepted the exact root-only candidate
SHA-256 `86b6d6863a8aad55d0a4050711b7c54282647e5e9d61d2cb6ab07520ec81f8e8`.
It removes nineteen imported semver-compatible duplicates and directs 69
incoming edges to existing root versions, leaving 504 records. C2 independently
matched each changed range and feature request against checksum-matched cached
registry records (the local sys2 `cc` requirement against its vendor manifest),
verified all 282 prior identities, all retained package fields/checksums and
every other edge, and rehashed the three unchanged consumer locks. This is a
reviewed resolver input; the successful canonical resolution follows below.
It did not authorize a pin refresh.

Windows-filtered root metadata subsequently succeeded in
`e55b8674-9a1d-426b-956e-c4dc180cdc4d` at central
`f6c43ca63373085aa668e1b1a06eea941cc6a7b0`: exit 0, 0.936 s, complete wrapper
885/0 bytes and empty Cargo stderr. C2 rehashed all retained files in
`target/unified-video/root-metadata-windows-02`; metadata is 1,032,130 bytes,
SHA-256 `46f2984e31340015c8448e197b380b33dbb93965541f8e009dac9d1ed871d48e`.
The resulting lock is blob `9bb3301c85d958188e2f4facf00a894dba109b50`,
SHA-256 `b7ccb0f5e63c21c5304b536429c4aab65449f1093bf62fdce70319009c457abb`.

Independent parsed comparison found 492 records: only twelve imported extras
were removed; all 282 prior checkpoint identities, retained package fields and
checksums remain. C2 checked all seven changed edge sets against pinned cached
registry metadata. `cc` adds the `jobserver`/`libc` edges needed by its existing
`parallel` request; five packages drop unused optional-feature edges. `rustix`
retains Windows-sys 0.52.0 within its `>=0.52, <=0.59` range and drops the union
seed's duplicate 0.59.0 edge. Every other resolved edge is unchanged, and the
three consumer lock hashes still match. This exact canonical lock is accepted.

The post-metadata identity guard `c00aa5b8-348a-44f9-b72d-b6850dd48458`
passed at the same source head: exit 0, 0.199 s, complete 1,197/0 bytes.
It confirmed 492 root records, 210 additions and all 282 prior identities,
with the three consumer locks unchanged. C1 committed only the accepted
canonical lock as `857c5b75517b37b4f182c1e3cb8e2dbd4c6839f3`.

The first integration attempt, `a7f2e430-3244-4adb-9bbc-5da0a107c5e0`,
failed with exit 1 in 0.393 s (complete 0/804 bytes): after writing the verified
seed's LF bytes back, Git refused to overwrite the working `Cargo.lock` during
cherry-pick. No commit was created. The follow-up verified the unchanged head,
clean index, absence of `CHERRY_PICK_HEAD`, exact current seed and retained
before/after hashes before using Git to restore the seed's checkout form.
`81f9345e-45da-401b-82af-81943f98d122` then integrated that exact lock with
exit 0 in 0.537 s (356/0 bytes), reaching the validated `a573912` head above.
The integrated LF hash equals the accepted canonical hash; only managed
`AGENTS.md` was untracked. This was a checkout-normalization correction, not
a new resolution or a change to the reviewed lock.

In the actual Windows root graph, video's sole selected feature is its empty
`default`. Its normal/build closure has fourteen package identities including
itself: the three helper libraries, `memchr`, `tracing`, `tracing-core`,
`tracing-attributes`, `once_cell`, `pin-project-lite`, `proc-macro2`, `quote`,
`syn` and `unicode-ident`. No node, node-client, terminal, native codec,
capture, Windows API or Tokio package enters this closure. The fixture's Tokio
and serde-json edges are dev-only. This is dependency-resolution evidence for
Windows, not cross-platform compilation or runtime validation.

Initial root formatting gate `b71c2e1b-5d88-48b9-96b4-95301a99c91c` at
`1c95a86346ac1a7ccc492cbf8d3bd7289f48adb2` failed with exit 1 before rustfmt:
Cargo rejected `node/pixels` workspace membership. C2 read the complete
0-byte stdout and 1,396-byte stderr. Pixels had been an implicit member through
node's direct path dependency; moving that edge to video required explicit
`members = ["pixels"]` in the existing node workspace. C2 accepted that exact
manifest correction (`a009eeaf84c2bb72c8b4868dd38032a7997cb4de`); reversing
only the member addition restores the prior parsed TOML. Features, profiles,
dependencies and pins are unchanged. C1 committed this repair as
`0641c13b4aba9464b984f2a9710db6982e8ddf45`; central integration reached
`db7d03645e6492c4fc0c57e218ca1ddb49c643cd` after the reviewed AV1 README correction.

At that head, the following complete central records were independently read.
Metadata commands were captured in fresh ignored directories under
`target/unified-video`, retaining the full Cargo streams, before/after lock,
hashes and exit status. Wrapper stdout/stderr byte counts below are separate
from the retained inner Cargo output.

| Gate and durable run | Result and qualification |
| --- | --- |
| Root offline metadata, `c0ed83c4-1a10-4c16-9cc5-d42d11526aa7` | Exit 101, 1.772 s, wrapper 1,963/0 bytes. Cargo stderr 1,123 bytes: root `interprocess` retained `futures-core 0.3.32`, while the union seed's `zbus` edge selected `0.3.34`. Before/after root locks are identical (`f63bc525...`). This attempt produced no accepted canonical graph. |
| Root formatting, `8603aadd-95a0-4f76-b3d8-179f7b8a3206` | Exit 1, 3.383 s, 40,896/0 bytes. Workspace discovery succeeded and rustfmt reported 59 hunks: 37 production/node and 22 fixture hunks. No frozen baseline/oracle hunks. |
| Node full-target locked offline metadata, `a29cbb8e-ff6d-4a45-84c3-fd9017c6e80c` | Exit 101, 2.543 s, wrapper 971/0 bytes. The 119-byte Cargo stderr reports unavailable cached `block 0.1.6`; offline mode prevented a download. Node lock unchanged (`0e851664...`). This does not establish a native Windows resolution failure. |
| Node Windows-filtered locked offline metadata, `4f14576e-6568-477f-a285-7e3ad8cefeae` | Exit 0, 4.123 s, wrapper 907/0 bytes. Full metadata is 1,659,147 bytes, SHA-256 `b963b075892f27aed7bb1045bbeaab5081b2ca91847e29a97f89603e25e5e615`; Cargo stderr empty, node lock unchanged. |
| Node default all-target Clippy, `f068718a-036f-4a9b-9ba9-731a2e653577` | Exit 101, 89.331 s, 0/12,249 bytes. The only compiler error is the unused `allmystuff_video::amf::*` compatibility import under `-D warnings`. Dependency checking is not a passed node gate or runtime evidence. |

The Windows metadata uses `--filter-platform x86_64-pc-windows-msvc`.
Its actual node features are `default`, `host`, `audio-io`; video resolves
`host`, `decode` and its empty `default`. C2 traversed the normal/build graph:
the native video closure contains 129 package identities including itself,
with no reverse node, node-client or terminal edge. Node's remaining capture
and input edges serve its other planes. This graph does not establish the
empty-default root closure or non-Windows resolution.

C2 applied only the 22 retained fixture formatting hunks and committed the
four affected files as `b6105152011e6dcbf910e9a0a00febaea9b03e34` after C1's
exact acceptance. Removing whitespace reproduces the prior fixture text;
all other fixture/oracle files are unchanged. C2 also independently reconstructed
all 37 production/node hunks from the retained formatter output and verified
C1's twelve resulting files and receipt hashes. Neither worker ran rustfmt.
The original node no-default all-targets check
`6f149a46-7942-419b-9d9d-5a3f379a7f68` at `db7d036` passed with exit 0 in
40.727 s; all 0/1,858 stream bytes were read, without warnings/errors. After
the reviewed formatting commits reached central
`050d3439eb3372e18199509ca0bee9e75a49cc89`, root fmt
`d42172d5-9dd0-455d-912b-d65548266a2a` and node fmt
`f570dd86-9b26-4677-a813-ace7b732523e` passed with exit 0 in 2.139 s and
1.890 s respectively; both complete streams were empty. Default-node lint and
runtime results are recorded as separate gates below.

The narrow AMF correction removes an empty compatibility glob, retaining the
public node module. Both the original and moved AMF top-level types/functions
are crate-private; their nested public methods do not expose them. There are
no remaining node/GUI callers of that private implementation through the shim.
C2 accepted exact shim blob `5cf98dd9ec92c995cdc3128cb858019ec4c3d590`, with no
lint suppression, public API addition or backend behavior change. The subsequent
strict node lint gate below passed with this correction integrated.

The reviewed isolated runtime selection is:

| Selection | Reviewed source inventory and limits |
| --- | --- |
| Video `--no-default-features --tests` | 42 cases: 37 new comparisons and five retained handoff tests. |
| Video `--no-default-features --features decode --lib video_decode::tests::` | Seven tests. Four Windows+host decoder cases are compiled out, including the real HEVC hardware test. Use existing `ALLMYSTUFF_H264_DECODER=software`; owned worker priority/registration behavior remains present. |
| Video `--no-default-features --features host --lib video::tests::`, skipping both encoder-ladder names above | 53 of 55 Windows host tests. Synthetic OpenH264/JPEG, scripted encoding and owned channel/thread tests; no capture or desktop follower is started. Other decoder/backend/OS test modules do not match this filter. |
| Node `--lib control_client::tests::` | Thirteen local channel/state and in-memory cursor tests. The first three also call the existing diagnostic-preference reader; set `MYOWNMESH_HOME` to a fresh private state directory and `ALLMYSTUFF_CWD_LOG=0`. The preference load happens even when the environment toggle is set. No client connection or production pipe binding occurs. |
| Selected node mesh adapters | Eleven pure cases: `au_sequence_` (four), `paced_ingress_` (two), plus `au_identity_survives_paced_fragment_reassembly`, `paced_video_requires_an_explicit_two_sided_selection`, `pacing_policy_preserves_target_but_caps_recovery_headroom`, `video_refresh_gate_is_single_flight_and_rate_limits_recovery_retries` and `recovery_requires_a_delivered_key_from_the_current_epoch`. |

These selections use locked/offline Cargo inputs and one test thread. Their
actual counts are recorded below. The existing mesh
handoff-reset integration test is outside this pure selection: it constructs
`Mesh`, installs and deliberately leaks a Tokio runtime and schedules retry
work, even though its synthetic route is not expected to contact a daemon.

## Completed central execution

C2 independently read the complete streams and terminal records for every run
below. They used Windows x86-64 at
`a5739129854ad4dd076b9ace30e65df7b2c977ba`, with one shared manager target,
`CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`,
`CARGO_INCREMENTAL=0`, `CMAKE_POLICY_VERSION_MINIMUM=3.5` and the existing
Visual Studio BuildTools CMake executable. Durations are the durable run's wall
times, not Cargo's inner elapsed time. Byte pairs mean full stdout/stderr.

| Gate and durable run | Actual result |
| --- | --- |
| Core, `bce348b3-405f-47ad-9bdc-087fe4c886c6` | Exit 0, 9.390 s, 3,852/1,472 bytes. Four harnesses: 12 handoff, ten core, thirteen ingress and seven route tests; 42 passed, zero filtered. No compiler warnings. |
| Decode-only, `5483f49d-96bb-447b-900c-8be023bc998c` | Exit 0, 21.069 s, 673/2,335 bytes. Seven passed, fifteen filtered; no compiler warnings. No `host` feature, and `ALLMYSTUFF_H264_DECODER=software` was supplied. |
| Selected host, `21473565-d0f3-4775-922d-b17f2403493a` | Exit 0, 40.715 s, 4,193/2,630 bytes. 53 passed, 56 filtered, including both explicit encoder-ladder exclusions and other test modules outside the filter. No compiler warnings; three OpenH264 runtime warnings retained below. |
| Root workspace all-target Clippy, `0caed28a-745b-4ae4-bfbf-8089e8526cfd` | Exit 0, 41.788 s, 0/6,039 bytes under `-D warnings`. This checks the root workspace's default features, not every optional native combination. |
| Node default all-target Clippy, `bbf57990-9806-403a-a3f6-7b6ab5c71392` | Exit 0, 13.859 s, 0/305 bytes under `-D warnings`; resolves the earlier empty-AMF-import error. |
| Decode normal/build tree, `2e7c211c-54ac-4bd0-8f1d-64c11b623def` | Exit 0, 0.346 s, 6,246/0 bytes. Windows `decode` includes software OpenH264, existing bridge/session/protocol helpers and Windows support, with no node, node-client, terminal, Tokio runtime or capture dependency. This graph is broader than the fourteen-package default core. |

The first five commands were:

```text
cargo test -p allmystuff-video --no-default-features --tests --locked --offline -- --test-threads=1
cargo test -p allmystuff-video --no-default-features --features decode --lib --locked --offline video_decode::tests:: -- --test-threads=1
cargo test -p allmystuff-video --no-default-features --features host --lib --locked --offline video::tests:: -- --test-threads=1 --skip video::tests::h264_ladder_picks_a_backend_that_emits_a_frame --skip video::tests::h264_stream_emits_annexb_with_a_leading_idr
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
cargo clippy --manifest-path node/Cargo.toml --all-targets --locked --offline -- -D warnings
```

The tree command was `cargo tree -p allmystuff-video --no-default-features
--features decode --edges normal,build --locked --offline --target
x86_64-pc-windows-msvc`. Both software codec selections used the existing
software decoder override. The host incremental OpenH264 test printed three
`ParamValidation` warnings: its slice-size constraint and maximum NAL size
were both 4,096, and adaptive quantization/background detection were
automatically disabled for screen content. These are runtime diagnostics from
the retained test, so this gate is not described as warning-free.

The final caller gates all used the following command shape, replacing only
the filter in the table:

```text
cargo test --manifest-path node/Cargo.toml --lib --locked --offline <filter> -- --test-threads=1
```

Each process received `ALLMYSTUFF_H264_DECODER=software`,
`ALLMYSTUFF_CWD_LOG=0` and `MYOWNMESH_HOME` pointing to the newly created private
`target/unified-video/node-runtime-01/control-home`. This contains the existing
diagnostic-preference lookup; none of these selections opens a production
client endpoint. All eight runs exited 0 without compiler/runtime warnings.

| Filter and durable run | Passed / filtered; wall time; stdout/stderr bytes |
| --- | --- |
| `control_client::tests::`, `dcb073e1-9713-4931-8d7a-f9decd77d520` | 13 / 313; 93.426 s; 1,315/9,299 |
| `mesh::tests::au_sequence_`, `8ea735d6-d353-4d7a-b3af-1b9d32dec722` | 4 / 322; 0.930 s; 442/152 |
| `mesh::tests::paced_ingress_`, `43c0aa7a-921c-4b07-81d0-67d8bb97a3aa` | 2 / 324; 0.832 s; 281/152 |
| `mesh::tests::au_identity_survives_paced_fragment_reassembly`, `a47fc006-2d3d-4a00-b8c2-8747bf805c56` | 1 / 325; 0.907 s; 186/152 |
| `mesh::tests::paced_video_requires_an_explicit_two_sided_selection`, `69579fd6-ae36-4481-9a97-5be31b969b53` | 1 / 325; 0.825 s; 192/152 |
| `mesh::tests::pacing_policy_preserves_target_but_caps_recovery_headroom`, `dbd15700-c6bd-4c56-b2ca-df26623da8f2` | 1 / 325; 0.850 s; 197/152 |
| `mesh::tests::video_refresh_gate_is_single_flight_and_rate_limits_recovery_retries`, `36db8510-7f99-4066-b240-a0110aaac0e9` | 1 / 325; 0.865 s; 208/152 |
| `mesh::tests::recovery_requires_a_delivered_key_from_the_current_epoch`, `d25d94c0-ec7a-46e7-abb5-6937b398fdb2` | 1 / 325; 0.894 s; 196/152 |

The selected runtime evidence totals 126 distinct passes:
42 core + seven decode + 53 host + thirteen control + eleven mesh. All selected
tests reported zero failures, ignored or measured cases. Filtered cases are
excluded from the claim; the repeated node harness invocations do not add
duplicate test cases. This is not a full root or node runtime suite.

After validation, cleanup `157ede49-47fc-47b1-bd4f-fed311928ff8` passed with
exit 0 in 15.689 s (complete 742/0 bytes). It removed 2,739,367,712 bytes
(about 2.55 GiB) of regenerable native build intermediates: 2,125 dependency
files plus `debug/build`, `.fingerprint` and `incremental`. All 1,296 protected
files were SHA-256 verified unchanged, including the tested executables and
DLL/PDB files, earlier portability evidence and retained metadata/source inputs.
Git status stayed unchanged. The resulting target size was 664,118,002 bytes
before adding `target/cleanup-video-01.json`, which records the protected hashes.

## Qualification limits

The evidence supports the extraction's selected behavior and dependency
boundaries on native Windows. Root default lint, node default lint and node
no-default checking have different scopes; no direct all-feature video lint
or optional FFmpeg build/runtime gate is claimed. The host test binary compiles
its unselected Windows hardware branches, but no real camera, desktop capture,
GPU encoder/decoder or desktop-switch behavior was exercised by this selection.
The decode worker's existing OS registration/priority behavior still executes;
the isolated tests do not validate all OS policy paths.

Unix and other operating systems, GUI/mobile builds, live Mesh transport,
production device/endpoint integration and end-to-end application video remain
unqualified by these runs. Source preservation and unchanged consumer locks do
not substitute for those executions. AV1 remains as described above. The earlier
experimental RISC-V evidence and unresolved codec issue are untouched. Workers
have not run compilers, formatters or tests for this slice.
