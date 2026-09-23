# BOUND-01: paced-video access-unit byte bound

The reviewed change applies the existing inclusive **16 MiB (16,777,216-byte)**
payload limit to initial and replacement paced-video fragments in both
assemblers. An oversized fragment is rejected and the affected pending unit is
discarded, so a later closing marker cannot release stale data.
Windows validation passed **206 executions covering 140 distinct test names**,
scoped formatting and both strict Clippy gates. Native Mac validation passed
**352 executions across 39 suites on each of x86_64 and arm64**, including the
new boundary and caller regressions. These totals include feature and platform
repetitions; they are not counts of newly added tests.

At baseline `ce1452fa173de2453fcfb32ec02acf6e58fa6172`, continuations checked
the byte limit, but initial fragments and fragments replacing an unfinished
timestamp entered pending state unchecked. A matching count marker could then
emit an oversized access unit. The
[video extraction report](video-library-extraction.md) deliberately preserved
that historical behavior; its report and frozen test oracles remain unchanged.
This follow-up is an explicit behavior correction.

The two production changes are:

| Path / reviewed source commit | Change |
| --- | --- |
| [`Freshness::forward_paced`](../../../crates/allmystuff-video/src/ingress.rs), `ca850ad61bdb30cdf7b515b66f4a2b0215a60833` | After closing-marker handling, reject a data fragment longer than the limit, remove its canonical peer/lane pending entry, and use the existing `paced AU exceeded assembly bounds` discontinuity path. |
| [`accept_paced_fragment`](../../../crates/allmystuff-video/src/receive.rs), `356e09891bd7e27410a6751ffddb400997826958` | After closing-marker handling, reject a data fragment longer than the limit, remove only the supplied route's pending entry, and return `(None, true)` to report damage. |

Ingress source was independently reviewed by B2; route source by C2. C1 also
independently reviewed B2's exact ingress regression commit `828d944`, and B1
accepted C2's route/caller fixtures `532d249`. Source integration
`03e2bed4-607b-44ec-8d7e-5970d2d2f502` succeeded with exit 0: `ca850ad` became
`1d44944`, followed by `356e098` as `519310b`. These source reviews and integration
are separate from runtime qualification.

The current validation source is `c75e7aa1fb2435a0d45ec2c250d1a6c70edd81c9`.
Its only delta from the core-tested `aefa65ab47f779e5d5e6570605c170a98f4489ef`
is C2's independently reviewed formatting correction in route and node fixtures;
both production guard files remain identical.

The preserved contract is:

- Exactly 16 MiB remains admissible for an initial fragment, a replacement,
  or the accumulated payload. One additional byte is rejected. The existing
  inclusive 2,048-fragment bound and matching timestamp/count marker rules stay
  in place; markers are processed before data-size rejection.
- An oversized replacement removes the old pending unit as well as rejecting
  the new data. Neither its old timestamp nor the rejected timestamp can close
  those discarded bytes later. Other canonical peer/lanes and routes continue
  independently.
- Ingress keeps its existing nonblocking sent/full/closed handling, first
  discontinuity reason, and Reset versus remembered Gradual recovery. A full
  sink is not retried or waited on; closure is observed at the existing send
  sites. Route assembly continues to report damage to its caller.
- Accepted payload bytes, first-envelope identity, key aggregation, timestamps,
  sequence decisions and assembly timing remain unchanged. Ingress retains its
  existing one-second age check on data arrival; route assembly gains no timer.
  Unpaced forwarding, framing negotiation, Mesh authorization, dependencies,
  lockfiles and feature defaults are outside this change.

This is a **per-access-unit payload bound** in one canonical ingress peer/lane
or one route. It does not bound the number of pending lanes/routes, aggregate
memory, vector capacity, already allocated incoming frames or queued complete
units. The separate
[`MAX_MEDIA_FRAME_BYTES`](../../../crates/allmystuff-protocol/src/control.rs)
limit remains 64 MiB for a media IPC frame body; the
[`read_media_source`](../../../node/src/control_client.rs) dispatch still
separates negotiated paced video, unpaced video and audio. The independent
[handoff queue's](../../../crates/allmystuff-video/src/handoff.rs) 64 MiB budget
is also unchanged. No production latency, crash-resilience or performance
improvement is measured by this correction.

The reviewed regression design has 21
[ingress cases](../../../crates/allmystuff-video/tests/ingress_compatibility.rs)
and 12 [route cases](../../../crates/allmystuff-video/tests/receive_compatibility.rs).
It covers exact-limit and plus-one initial/replacement/cumulative payloads,
oversized continuations, stale closing markers, recovery and isolation. Ingress
adds full/closed sink and canonical peer/lane cases. The new node fixture
`control_client::tests::oversized_paced_au_does_not_stop_audio_or_other_video_lanes`
feeds an in-memory media pipe with an oversized initial/replacement fragment,
stale closers, a valid second video lane, recovery frames and three audio
payloads. It asserts exact ordered output while consumers remain idle; it opens
no daemon connection or device. It passed within the Windows caller module and
as an exact selection on both Mac architectures.
Unchanged behavior still compares original and current implementations. New rejection cases explicitly
assert the old oracle's oversized acceptance against the corrected rejection;
the oracle is not rewritten to make the correction appear historical.

| Evidence | Exact source / selection | Result |
| --- | --- | --- |
| Historical reproduction, `506f5682-e7a0-454f-81f4-99e630facc9b` | Baseline `ce1452fa`; `cargo test --locked --offline -p allmystuff-video --no-default-features --test ingress_compatibility --test receive_compatibility first_fragment -- --test-threads=1` | Windows x64, exit 0, 60.297 s; both old-exception fixtures passed (one per target; 12 and 6 filtered). Full retained streams: 386 stdout / 1,388 stderr bytes. This demonstrates the old gap, not corrected behavior. |
| Ingress negative control, `4e4dae1e-f383-4043-8ac5-df20d702921a` | B2 test-only `828d944`; `cargo test -p allmystuff-video --no-default-features --test ingress_compatibility --locked --offline -- --test-threads=1` | Windows x64, exit 101, 33.593 s; 15 passed / 6 intended failures, 4,423 / 1,421 stdout/stderr bytes. Old source retained rejected pending data, did not report closed-sink rejection, or used the old replacement reason. |
| Route negative control, `0c6e91a6-ae84-4f7d-b27d-2f0d86af517a` | C2 `e8395a4`, containing route/caller fixtures but unchanged route production; same command with `--test receive_compatibility` | Windows x64, exit 101, 1.519 s; 9 passed / 3 intended failures, 2,334 / 452 bytes. Old source lacked initial damage reporting and retained the oversized replacement. |
| Ingress fixture formatting, `a774d58e-f60e-499f-906f-ca9c27c64dee` | `rustfmt --check --edition 2021 --config skip_children=true crates/allmystuff-video/tests/ingress_compatibility.rs` at `828d944` | Exit 1, 0.366 s; 6,437 / 0 bytes, nine wrapping hunks. C1 independently reconstructed and accepted exact correction `0e6c1fe`; all non-whitespace bytes and 21 test names match. |
| Route/caller formatting, `ea8c955f-37b2-4714-8c45-c7d70094e48b` | Same formatter options, selecting `crates/allmystuff-video/tests/receive_compatibility.rs` and `node/src/control_client.rs` at `e8395a4` | Exit 1, 0.112 s; 1,691 / 0 bytes. C2 applied the two retained hunks as `4a419d67`, independently accepted by B1. |
| First integrated core run, `440cf9de-0b74-489e-858e-b57e61dbbb5b` | `aefa65ab`; `cargo test -p allmystuff-video --no-default-features --tests --locked --offline -- --test-threads=1`, shared target | Exit 101, 7.691 s; 12 handoff + 10 core + 15 ingress passed, six ingress failures; Cargo stopped before route tests. Full streams: 6,495 / 578 bytes. Retained artifact discrepancy is discussed below. |
| Fresh-target core run, `3e974b1d-c774-476a-9686-ad477e31a822` | Identical `aefa65ab`, command and checkout; new dedicated target | Exit 0, 43.156 s; **55 passed** (12 handoff + 10 core + 21 ingress + 12 route), zero failed/ignored/filtered. Full streams: 4,901 / 1,705 bytes. |
| Final scoped formatting, `eb59b44f-4526-4bbe-907b-9040870248e6` | `c75e7aa`; same formatter options, both production guard files, both regression files and `node/src/control_client.rs` | Exit 0, 0.451 s; 0 / 0 bytes. |
| Software decode, `5ebc566d-17d5-4f48-a22e-13af3d6e9b9f` | `c75e7aa`; `cargo test -p allmystuff-video --no-default-features --features decode --lib --locked --offline video_decode::tests:: -- --test-threads=1` | Exit 0, 152.049 s; 7 passed, 15 filtered; 673 / 2,729 bytes. |
| Decode-feature regressions, `d1a1b304-5e72-4302-94a8-d5831cc891b6` | `c75e7aa`; `cargo test -p allmystuff-video --no-default-features --features decode --test ingress_compatibility --test receive_compatibility --locked --offline -- --test-threads=1` | Exit 0, 8.517 s; 21 ingress + 12 route passed, none filtered; 2,827 / 475 bytes. These repeat the core definitions under another feature configuration. |
| Host-feature unit selection, `b1f1edd9-1d65-4c8d-8671-595ae3782beb` | `c75e7aa`; `cargo test -p allmystuff-video --no-default-features --features host --lib --locked --offline video::tests:: -- --test-threads=1`, skipping exactly `video::tests::h264_ladder_picks_a_backend_that_emits_a_frame` and `video::tests::h264_stream_emits_annexb_with_a_leading_idr` | Exit 0, 219.402 s; 53 passed, 56 filtered; 4,193 / 2,773 bytes. Existing OpenH264 runtime warnings are retained. |
| Host-feature regressions, `05841706-8ecd-4aba-b894-c2cfec42cb86` | `c75e7aa`; the decode-feature regression command above with `--features host` | Exit 0, 13.039 s; 21 ingress + 12 route passed, none filtered; 2,827 / 476 bytes. These repeat the same boundary definitions. |
| Node caller module, `33aecc49-be95-4c65-97a5-60c80e040afc` | `c75e7aa`; `cargo test --manifest-path node/Cargo.toml --lib --locked --offline control_client::tests:: -- --test-threads=1`, short dedicated target | Exit 0, 374.833 s including compilation; 14 passed, 318 filtered; 1,410 / 12,327 bytes. Test runtime was 0.09 s. |
| Seven pure Mesh selections, run IDs below | `c75e7aa`; same node command prefix, reviewed filters and short dedicated target | All exit 0; 11 passed in total, with no live Mesh construction. |
| Workspace Clippy, `2d3f7802-5473-4288-9e27-ae9df0471883` | `c75e7aa`; `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | Exit 0, 212.388 s; 0 / 8,332 bytes. |
| Node Clippy, `c3e55920-fd21-4be5-b3bd-d1c0ca1efd0d` | `c75e7aa`; `cargo clippy --manifest-path node/Cargo.toml --all-targets --locked --offline -- -D warnings`, short dedicated target | Exit 0, 138.292 s; 0 / 11,110 bytes. |

The seven Mesh selections used `--test-threads=1`; the last five also used
`--exact`. They exercise pure ordering, negotiation and recovery decisions:

| Filter after `mesh::tests::` | Durable run | Passed / filtered |
| --- | --- | ---: |
| `au_sequence_` | `deb8c008-f71b-499a-a4aa-79b4cdfab307` | 4 / 328 |
| `paced_ingress_` | `2f0e19c2-fe0f-479a-b62f-2b8fbd23ccdc` | 2 / 330 |
| `au_identity_survives_paced_fragment_reassembly` | `c0dc685b-86cf-4514-ab15-6610c0b38656` | 1 / 331 |
| `paced_video_requires_an_explicit_two_sided_selection` | `75adbb3b-aab1-42ca-973c-f732eca2c7ac` | 1 / 331 |
| `pacing_policy_preserves_target_but_caps_recovery_headroom` | `725ad039-e305-4292-9d36-e01c88e1b431` | 1 / 331 |
| `video_refresh_gate_is_single_flight_and_rate_limits_recovery_retries` | `b2c9b2c8-7bb7-4c62-ac8a-fa38b9f79e93` | 1 / 331 |
| `recovery_requires_a_delivered_key_from_the_current_epoch` | `79ffec4c-2514-43bd-88c2-2fd58e7391b1` | 1 / 331 |

The final Windows total is 55 core + 7 decode + 33 decode-boundary + 53 host +
33 host-boundary + 14 caller + 11 Mesh = 206 passing executions. The 33 boundary
definitions run in three feature configurations, leaving 140 distinct test
names. Historical reproductions, negative controls and failed attempts are not
added to this total. C1 and B2 independently inspected the retained terminal
records and complete stdout/stderr streams without truncation. Successful
strict lint gates had no compiler warnings; the host test's retained OpenH264
warnings are separate from that lint result.

The failed integrated run rebuilt its ingress test executable, but its
shared-target library files still carried timestamps from the old-source
negative control. B2's retained-artifact audit found relative dependency paths,
checksum-disabled fingerprints and source/library timestamp ordering consistent
with reuse of that old library. The same source and Cargo arguments passed in
`target/bound01-20260922/cargo-integrated`, with different library bytes and
SHA256 hashes. C1 independently rehashed both sets of library/metadata/test
artifacts against B2's receipt. This strongly supports stale shared-artifact
reuse; the exact Cargo freshness decision was not traced. Identical Cargo
fingerprint tokens/JSON did not imply identical library contents. Both runs
remain recorded, and subsequent validation used the manager checkout's dedicated
target. No source repair was needed for this discrepancy. After the authorized
cache cleanup, the original shared/dedicated artifact paths are historical;
the six hash-verified copies under
`target/bound01-20260922/retained-artifact-audit/{shared,dedicated}` are the
retained binary evidence, alongside B2's audit and fingerprint records.

The first eight node test attempts and the first node Clippy attempt failed
before any test execution. `audiopus_sys`'s CMake/MSBuild step rejected a
261-character generated `ParallelCustomBuild.command.1.tlog` path because it
required a path shorter than 260 characters. The caller failure was
`3a2d5f4f-0338-4ab5-9c29-86ba56a56f70`; the seven Mesh failures were
`ac2a486e-a2f8-4fcc-a1fc-c39efb0d1fa5`,
`a602a8e8-4399-4cc5-8fa1-996993d8b764`,
`426eb6bf-3be7-4e66-8d61-c2a798685886`,
`9630a072-4f78-463a-a6fd-33496dd5bb48`,
`9072c9ff-62c3-4bee-b802-7e996b0797cb`,
`baf86683-6671-427c-afd5-7cecad89a494` and
`4b919a76-db2c-4175-a27f-272aa63efc3b`; node Clippy failed as
`22a3717f-d09b-42ca-a288-05a2fd969b5e`. All exited 101 with empty stdout and
complete retained dependency-build diagnostics. Moving only the dedicated
Cargo target to `target/b01` reduced that generated path to 231 characters;
the same source then passed the caller, all seven Mesh selections and node
Clippy shown above. These are build failures followed by a corrected build
environment, not test assertion failures or an additional source correction.

Native Mac run
[35797280354](https://github.com/nathanfraske/AllMyStuffEncoderFraske/actions/runs/35797280354)
passed at exact `c75e7aa` on both architectures. Each job passed all **352
selected executions / 39 suites** from the
[current inventory](../../../scripts/ci/README.md), including 21 ingress,
12 route and the exact new node caller; that caller filtered 301 other tests.
The two host gates per architecture compiled video and default-feature node
libraries without executing device code. This is 704 executions across both
platforms, with deliberate repeated definitions, not 704 distinct tests.

| Native runner | Recorded image | Rust / Cargo | CMake |
| --- | --- | --- | --- |
| `macos-15-intel`, `x86_64-apple-darwin` | `20260824.0482.1` | 1.98.0 / 1.98.0 | 4.4.2 |
| `macos-15`, `aarch64-apple-darwin` | `20260907.0337.1` | 1.98.1 / 1.98.1 | 4.4.3 |

Both recorded macOS 15.7.9, Xcode 16.4 and Apple Clang 17.0.0. B1 independently
matched all 99 command records and 198 complete log streams per architecture,
all 39 list/run name sets, and all 201 files in each artifact bundle. C1 also
checked all log byte counts, selected names, results, four exact-commit
lockfile hashes before/after and empty tracked-tree diffs. Every command, final
process census and always-cleanup receipt was clean, with no cleanup signals or
scan errors; the private roots were removed. This qualifies normal cleanup,
not forced-cancellation recovery. Logs retain expected compatibility panic
probes and the existing `block 0.1.6` future-incompatibility warning. They do not
invalidate the passed selections, but the run is not described as warning-free.
The downloaded bundles remain under
`target/bound01-20260922/macos-35797280354`.

Manager-only cleanup removed three completed
Windows Cargo caches: 752,347,959 logical bytes from `target/debug`,
2,139,571,168 from `target/bound01-20260922/cargo-integrated`, and 1,978,007,693
from `target/b01`. The receipts total **4,869,926,820 logical bytes (about
4.54 GiB) and 11,195 files**; this is not a measurement of physical freed blocks.
`cleanup-shared-debug.json` and `cleanup-completed-caches.json` record removal.
C1 verified the three paths are absent and rehashed all six retained diagnostic
artifacts. Source, locks, managed `AGENTS.md`, durable logs, coordination receipts
and downloaded Mac evidence were preserved. No further native run was needed
for the final documentation-only change.

Historical run `35763211936` still records 338 executions at its original
revision. Current library/caller/feature checks retain their reviewed
exclusions. This scope does not qualify live Mesh traffic, audio/video devices,
hardware encoding, GUI/mobile products, an MSRV, aggregate-memory behavior or
performance. The final code qualification is `c75e7aa`, with the earlier core
run's formatting-only difference stated above.

See [BOUND-01 in the master list](../../MODULARIZATION-MASTER-LIST.md#bound-01--paced-video-first-and-replacement-fragment-limit)
for current status. Historical extraction and Mac reports remain evidence for
their original commits.
