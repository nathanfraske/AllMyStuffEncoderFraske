# Audio library extraction evidence

This records the source review and Windows/macOS validation for extraction of
`allmystuff-audio` from `e6340b31daa6d9c2058c4ee345564d3e0c0ecebf`.
Production, baseline, fixture and test-include source reviews are complete.
The extracted package passed 99 selected feature executions covering 52 distinct
hardware-free definitions on Windows and separately on each macOS architecture.
Windows formatting, all three package strict-lint profiles and the three node
compile/lint configurations passed. The initial IO lint failure and its narrow
correction are recorded below. Both pure node caller definitions passed with
default and disabled-I/O features. Reviewed Windows cleanup removed 2,577,006,776
logical bytes of regenerable intermediates while preserving source, test
executables and native linkage evidence. No audio-device qualification is claimed.

The operator separately requested an inventory of extraction locations,
optimization opportunities and future MyOwnMesh adaptation. Those proposals
remain separate from this behavior-preserving change; no pin upgrade or
speculative optimization is part of this implementation. The independently
reviewed [master list](../../MODULARIZATION-MASTER-LIST.md) records those
opportunities and their evidence, risks and required validation.

## Baseline and independent construction

The [frozen baseline](../../../crates/allmystuff-audio/tests/baseline/README.md)
contains 21 files, 22 complete input identities and 14 declared source ranges.
It was independently accepted by C1 and committed alone as
`15ee2951c0c0c5e85e08e2f493656c01a18f234f`, parent `e6340b31`.
Its manifest SHA256 is
`eb451fae9f1df877c5647cda847925b48d9a379a17e9768f4700951f81caa430`.
The original complete audio module SHA256 is
`5c73be7cffb0458b9a6ce5ecef59d20b3fc76a52cfea423d8f47d3bebf4a2b7c`.
All text identities here normalize checkout CRLF to Git LF.

Source inputs cover real/stub audio, shared `AudioFrame`, Mesh encoding,
decoding, queueing and routing, feature and statistics selection, protocol
offers, consumer manifests and all four lockfiles. Five independently computed
literal sets pin sample float bits, downmix/resampling arithmetic, ring tails,
wire bytes/JSON shape and refusal/lifecycle constants. These are source-derived
expectations, not output captured from the new implementation.

The [new fixture inventory](../../../crates/allmystuff-audio/tests/README.md)
currently contains 46 definitions across public PCM/contracts and private codec
and IO children. The copied encoder spans are original lines 71–129 and
913–930. The IO oracle retains original lines 1–930, redirecting only its video
statistics-policy call into the test policy. Its appended callback observation
wrapper copies the original local output macro. Adjacent manifests record the
exact spans and observation suffixes. The shared IO adapter never implements
feed, resampling, trimming, lifecycle or metering behavior itself.

C1 and A2 independently accepted all 14 fixture/source-checkpoint payloads,
committed unchanged as `3c5ced3301d09ebab99c8ecae5b132960856e5a9` after the
separate baseline commit. All 21 baseline files remain unchanged. C1's
`2da6801ad90c24880cce09a3efe82d7ca9144dfa` adds only the independently reviewed
85-byte codec and 82-byte IO test-module suffixes. Removing them restores the
exact accepted production bytes; both relative paths resolve to the accepted
fixture files. That source checkpoint preceded the extracted-package execution
recorded below.

## Accepted source boundary

C1 production commit `c1bce0fee51843cc73df2299f3ae9236112cc1ea` contains 16
paths. C2 independently verified every receipt identity, ten unchanged
caller/model paths, complete function inverses and lock records before source
acceptance. Candidate IO SHA256 is
`8703f851cdb00943fba1e67afdd3be680f96dd3e85023e58be1dbe0bb4f62fed`;
codec SHA256 is
`1fb502b373e7d0bd03036e92d8b5ec69aa549adb83c24f70b2df65d6e371d305`.
Test includes are deliberately a later reviewed delta.

| Package surface | Responsibility |
| --- | --- |
| Default `pcm` | Original sample conversion, downmix, linear resampling and playback-ring rules, with no normal dependencies. |
| Optional `codec` | Original fixed 48 kHz mono encoder/framer, thin stateful decoder, shared session `AudioFrame` and explicit disabled bridge/encoder. |
| Optional `audio-io` | Original CPAL/Pulse and platform fallback implementations, maps, worker lifecycle, feed and callback bodies. Includes `codec`. |
| Node adapters | Existing feature-selected APIs and lazy video-statistics policy; explicit disabled types survive feature unification. |
| Mesh | Route/auth checks, decoder map and lifetime, allocation/locks, lane selection, bounded queue, protocol and transport. |

The encoder, all PCM helpers, map methods, statistics and native functions
reverse to the original bodies after declared visibility/import, generic policy
and explicit tracing-target substitutions. All seven retained test bodies
remain exact. The module-scoped `fill!(ring, channels, data, conv)` reverses to
the original function-local macro; all three output conversion expressions are
unchanged. The new `PhantomData<fn() -> S>` carries no policy instance and adds
no ownership-based `Send`, `Sync` or `Default` requirement to the bridge.

`OpusDecoder` directly calls the original `opus::Decoder::new(48000, Mono)` and
`decode(data, pcm, false)`, preserving native errors. Reversing exactly three
Mesh substitutions restores its entire original file: decoder type, constructor
and the wrapper's removal of the explicit `false` argument. Consequently the
120 ms output allocation before the decoder-map lock, lazy vacant construction,
error returns, decoder retention and later truncation remain at their original
sites. No new transport authorization or queue policy is introduced.

The Opus receive path resolves the route/lane and calls `inbound_media_ok` before
decoder allocation/use. The separate PCM `MediaPayload::Audio` arm forwards
through its existing path; this report does not pretend those two branches have
identical local checks. Application authority remains a caller concern.

## Preserved details and limits

- Capture conversion uses 32767 while playback normalization uses 32768.
  Downmix accepts zero channels as mono and averages a partial final group by
  its actual size. Signed divisions and float casts retain their original
  truncation behavior.
- Linear resampling restarts for each buffer, floors output length and holds
  the last sample at the tail. Zero rates pass through. The encoder already
  borrows input at 48 kHz or zero rate; only nonmatching rates allocate a
  resampled vector before copying into its remainder buffer.
- Ring trimming happens only beyond the strict threshold, uses integer
  samples per millisecond and keeps the newest target-depth samples. Rates
  below 1000 have the original one-sample/zero-target corner case. Empty
  appends can still trim an oversized existing ring.
- Encode callbacks run synchronously before offset advancement and the final
  drain. A callback panic retains input even though native codec state has
  advanced. Native encode errors consume that canonical frame and continue;
  this error branch is source-preserved, not artificially forced by fixtures.
- Feed uses its route argument, ignores frame route/sequence for lookup and
  increments the feed counter even for empty frames. It observes the current
  device rate, downmixes first and then resamples.
- Capture and playback maps remain separate. Duplicate start guards precede
  native worker creation, while the check and insert still use separate locks.
  `stop` removes both records then joins capture and playback off-lock;
  `stop_all` clears and joins under each respective lock. Map presence remains
  the running-state criterion, regardless of worker liveness.
- Route drop sets the stop flag with sequential consistency before joining
  and ignores a worker panic result. Metering calls the consumer after releasing
  its statistics lock. Statistics retain five-second windows, saturating peaks,
  lazy policy lookup and the shared node logging dial.
- Output callbacks pop one mono sample per interleaved frame, including a
  partial final frame, duplicate it across channels and supply format-specific
  silence on underrun. Zero output channels retain the original panic.

Hardware-free fixtures cover these arithmetic, state and callback rules using
paired original/new instances, independent literals, seeded maps and bounded
fake workers. They do not establish device selection, microphone permission,
native stream errors, OS fallback quality, audio quality/latency, or real-start
races. The historical coexistence test actually opens real devices and is
excluded from hardware-free selections. Five-second microphone/starvation
warnings and native backend errors remain source-reviewed unless separate
runtime evidence is later recorded.

## Dependency review

Root lock packages increase from 501 to 538: one local audio record plus 36
existing native dependency identities imported from the frozen node lock.
Removing those 37 blocks reconstructs the entire original root lock text.
Every old root package block and resolved edge remains unchanged. Ten incoming
edges from newly imported records reuse versions already in the root lock;
C2 independently checked their requirements, requested features and both
package checksums against cached registry metadata.

Node, desktop and mobile lock counts respectively change 608→609, 805→806 and
736→737. Only the new local package and relocation of its audio edges change;
all old external metadata and resolved edges remain intact, including historic
consumer version differences. The Windows graph checks below qualify the
selected root and node feature resolutions; package build/test results are
recorded separately below.
No registry version refresh is authorized by the extraction.

Native libopus provenance is a separate question from the Rust package pin.
`audiopus_sys` can select pkg-config, an explicit library directory or bundled
CMake sources. Differential codec fixtures compare independent states using
the same linked native library and make no cross-version packet-byte claim.

## Retained Windows baseline execution

Manager ran six isolated exact filters in the retained
`allmystuff_node-0d876981d2166e53.exe` proof binary. C2 read every full recorded
stdout/stderr, terminal status and filter argument from the durable receipts,
rehashed the 15,899,648-byte binary and verified no relevant source/manifest/lock
delta from its tested `82af488` source to baseline `e6340b31`.

Binary SHA256:
`602adf0e62b55adcaeb20b9a2417a36ef3832568429f34179cfd96f96a6f81e9`.
Every command selected exactly one test with `--exact --test-threads=1`, exited
zero, reported one pass and 337 filtered definitions, and had empty stderr.

| Original `audio::tests::` suffix | Durable run | Stdout bytes |
| --- | --- | ---: |
| `downmix_stereo_to_mono_averages` | `bcca0aeb-2316-4b9b-b012-25470466442b` | 172 |
| `resample_upsamples_length` | `9ed19772-d8c1-49fd-aa11-2f02fddad1de` | 166 |
| `sample_conversions_round_trip_near_unity` | `cf51e8aa-6e31-4ba6-98ca-14b846a2ff23` | 181 |
| `level_stats_flag_a_pure_silence_window` | `33ae2fa4-8eb4-4135-8e89-3c6b81b11d46` | 179 |
| `opus_stream_frames_and_survives_a_roundtrip` | `4a2ba208-59b3-423e-9a0c-362e5c159b9c` | 184 |
| `playback_ring_trims_back_to_the_target_depth` | `58e03757-93f2-48cb-8411-27e7d17f6d35` | 185 |

This is retained-binary baseline evidence, not a rebuild of `e6340b31` or a pass
for the extracted audio package. Complete records remain in manager
`target/audio-validation-20260922-01/manager-plan.json`; independent source,
registry and baseline reviews remain in C2 `target/audio-extraction`.

## Initial central assembly and graph evidence

Integration `695ade75-3160-4d9d-ba8d-84aa24d93810` imported the original
baseline and production commits, exited zero in 0.518 s and retained complete
2,386/0-byte stdout/stderr. The resulting production assembly is
`a94612984228d110c00b9ee275c3626b6d1baae5`. The following manager-owned Windows
x64 records use `C:\Users\Admin\.cargo\bin\cargo.exe` from that worktree.
C2 read each terminal status, exact arguments and complete untruncated streams.

| Cargo arguments | Durable run | Result | Stdout/stderr bytes |
| --- | --- | --- | ---: |
| `tree -p allmystuff-audio --target x86_64-pc-windows-msvc -e normal,build --locked --offline` | `5c2cf9c1-55a4-4ee2-8b48-603a0dc67f4b` | Exit 0; 7.700 s | 119/0 |
| `tree -p allmystuff-audio --features codec --target x86_64-pc-windows-msvc -e normal,build --locked --offline` | `fd79a4a9-05d5-4ae4-a7f0-a67aec83a6a4` | Exit 0; 2.261 s | 2,865/0 |

The default tree contains only the audio package. The codec tree has direct
normal dependencies on the existing session model, Opus and tracing; session
retains its graph/protocol closure, including `dirs`. No node or device-I/O
dependency appears in these two selected normal/build trees. These are scoped
Windows graph results, not compilation, test execution, universal feature
minimality or proof of which native libopus implementation will be linked.

## Initial formatting feedback

Integration `453adad8-ba35-419b-acc4-8dfd98c6979a` added the original fixture
commit, reviewed includes and accepted master list. It exited zero in 0.348 s
with complete 1,631/0-byte streams, producing
`56f1e7fb4490e1ae71741e7468fe9cc20f2f8a98`. Root and node formatting checks
then failed; these initial failures are retained:

| Cargo arguments | Durable run | Result | Stdout/stderr bytes |
| --- | --- | --- | ---: |
| `fmt --all -- --check` | `8be8a82f-c863-4ee1-bba9-c8731f41b05c` | Exit 1; 9.563 s | 56,212/0 |
| `fmt --manifest-path node/Cargo.toml --all -- --check` | `580ec92c-24f5-4dbb-829c-2cd7a65c1530` | Exit 1; 4.688 s | 56,212/0 |

C2 read the complete byte-identical reports and independently reconstructed
their 70 suggested hunks across ten files. They wrap two production expressions,
format authored fixtures/adapters and order imports. String/numeric literal
sequences, test names and assertion/panic counts are unchanged in the expected
reconstruction. Frozen reference modules and baseline literals have no
suggested changes. C2 then compared every actual candidate byte with the
independent reconstruction, checked all 34 baseline/oracle/manifest/lock paths
unchanged and accepted the exact correction. C1 committed only those ten
paths as `3a88e62fcc335f967765deb429534a34688806db` (+531/-149). Committed
identities match the accepted candidate. The later successful root/node reruns
are recorded with the extracted-package execution below.

## Complete Windows feature-graph captures

The independently reviewed capture helper SHA256 is
`a6706e4ea0bbd86269b0dc9b746282446c94ea1b5376f7ef58cc154a68445f14`
(5,738 LF bytes). It runs only the fixed Cargo metadata command, requires an
exact reviewed HEAD and initially clean tracked worktree, and writes fresh
full metadata and guard JSON files. It compares all tracked raw file hashes,
HEAD, tracked status and four raw/LF lock hashes before and after capture.

Six initial `-01` attempts stopped before helper or Cargo execution because
the helper file had not been copied successfully. Each exited 2 with empty
stdout and the same complete 215-byte Python missing-file error:

| Profile | Retained setup failure |
| --- | --- |
| Root default | `a4dcc8ba-9365-4229-a4b0-74f42f7fd237` |
| Root codec | `c6a92077-0134-4309-9cc9-7278562b73e3` |
| Root audio-io | `9b7c7dc0-ea82-4ed9-8585-81dc9a2ac8d8` |
| Node default | `f5750b53-5b3c-4a34-a4c4-10e1ff829e81` |
| Node no-default | `fdf978bd-a661-4608-83c0-36c23c508da3` |
| Node no-default + audio-io | `aaabe6b3-7e41-404f-a5e8-34ff62d8b080` |

After the manager copied and rehashed the exact accepted helper, all six fresh
`-02` captures passed at unchanged `56f1e7fb`. C2 read all twelve complete
terminal records, rehashed all six metadata files and recomputed the guards.
All 892 tracked files match the recorded exact Git source (359 byte-identical,
533 differing only by checkout CRLF). All six before/after snapshots agree;
the four normalized lock hashes match the accepted commit.

Each command uses `C:\Users\Admin\.cargo\bin\cargo.exe metadata
--format-version 1 --filter-platform x86_64-pc-windows-msvc --locked --offline`
with the profile flags below. The manager invoked the helper using
`C:\Python313\python.exe`, a fresh `metadata-<profile>-02.json` path, full
expected HEAD `56f1e7fb4490e1ae71741e7468fe9cc20f2f8a98` and the profile name.
All results below exited zero with empty stderr and untruncated streams.

| Profile / extra flags | Durable run | Seconds | Stdout bytes | Normal/build closure, including audio |
| --- | --- | ---: | ---: | ---: |
| Root default / none | `007a2c47-eae2-40e8-b6b8-04583a0fe49c` | 29.877 | 2,988 | 1 |
| Root codec / `--features allmystuff-audio/codec` | `c594d7d8-2e16-4810-afb9-1ac685846ecf` | 3.598 | 3,561 | 37 |
| Root audio-io / `--features allmystuff-audio/audio-io` | `f65cc7ad-1fe0-4d99-a3e2-09943584864b` | 4.043 | 3,884 | 53 |
| Node default / `--manifest-path node/Cargo.toml` | `719e6c24-1906-4492-977f-df8044f7f73d` | 10.116 | 3,822 | 53 |
| Node no-default / `--manifest-path node/Cargo.toml --no-default-features` | `80146958-5b41-459c-a1d4-8c87f19a4f43` | 4.374 | 3,573 | 40 |
| Node no-default + audio-io / `--manifest-path node/Cargo.toml --no-default-features --features audio-io` | `1f44cd1a-3e5c-4811-bf70-d993c4e31921` | 4.191 | 3,960 | 53 |

The full JSON captures are retained under manager
`target/audio-validation-20260922-01`:

| Profile | Metadata bytes | SHA256 |
| --- | ---: | --- |
| `root-default` | 1,043,888 | `ef40196484d5aabc3954cd73bd85f8747498bf8fc6ec3ef4b1950592e5e71e60` |
| `root-codec` | 1,053,309 | `714fc66b9ceda749aaf18d2ca269b0905ad0bc67ebc575614308ac9bf4737527` |
| `root-audio-io` | 1,113,276 | `140d9422106480d8e84ecd9760032632ea8452e97e7f2b9e171866bb1360d276` |
| `node-default` | 1,671,806 | `b583e773c0fc2a7c5e3d4d3ffb21a7e0caf7f09dbacebfbb9d9949a2fd66cd4c` |
| `node-no-default` | 1,366,577 | `43519dacd909cad5183ecb888cb1511afe7735085bf29db2d5be20d452ba1c64` |
| `node-no-default-audio-io` | 1,417,581 | `a676f4e656df5f86a6abbe90ce10b806d7283c617fd9516c275ff634d0ce129d` |

Independent normal/build traversal confirms no node, video, inventory or
terminal package in the audio closure. Default audio has no normal/build
edge; codec has direct session/Opus/tracing edges; IO adds CPAL/parking_lot on
Windows. CPAL is reached only in the IO profiles. The codec and IO local
closure consists of audio, session, graph and protocol. Every reached package
identity matches the corresponding frozen root or node lock.

These are workspace-resolved graphs: dev/workspace feature unification can
affect reachable package features, explaining why the selected root and node
closures need not match. The root metadata includes test-only `serde_json`;
the closure counts exclude dev-only edges. No standalone-minimum, compiler,
runtime or native libopus-origin claim follows from metadata. The manager's
`metadata-closures-reviewed-01.json` SHA256 is
`b698a33ce01ea7ebccc3345545180fdcc3dde1cb180ab639416af9ae2c66a80d`;
C2 retains its independent raw-record and content review separately.

## Focused Windows extracted-package execution

The manager assembled the accepted formatting correction and reviewed macOS
runner updates at `b86c9fb31baed34a2feeb55243a90120ec669088`. C2 independently
compared the scoped audio source/test bytes to accepted `3a88e62f`; the package
and node adapter content agree. The following records use Windows x64,
`C:\Users\Admin\.cargo\bin\cargo.exe`, the manager's private
`target/audio-validation-20260922-01/tmp-native-01` for `TEMP`/`TMP`, two build
jobs, disabled incremental compilation and debug information, and
`CMAKE_POLICY_VERSION_MINIMUM=3.5`. The exact environment and all 892 tracked
file hashes are retained in `native-before-01.json`, alongside the explicit
four application-lock snapshot. Two earlier snapshot scripts stopped on
lock-path/count assertions before any compiler or test execution; their
corrected setup distinguishes the vendored lock from those four application
locks.

C2 read each exact command and complete untruncated stdout/stderr to EOF.
These formatting reruns exited zero and emitted no output:

| Cargo arguments | Durable run | Seconds |
| --- | --- | ---: |
| `fmt --all -- --check` | `eaea9b5b-6d05-4201-a5a9-2b5da36b89a4` | 3.123 |
| `fmt --manifest-path node/Cargo.toml --all -- --check` | `e73363d9-bbb0-4bf6-aef2-fdff30720c64` | 2.431 |

The package commands share `test -p allmystuff-audio --locked --offline`,
followed by the feature flags and libtest arguments shown here:

| Feature flags | Arguments after `--` | Durable run | Passed | Seconds | Stdout/stderr bytes |
| --- | --- | --- | ---: | ---: | ---: |
| None | `--test-threads=1` | `2ed0062f-0b76-41ba-900d-aa8fdaf2d5d5` | 14 | 7.106 | 1,439/668 |
| `--features codec` | `--test-threads=1` | `d3c6ca1f-4be0-4fa2-a144-d3e1a322a4d6` | 33 | 40.797 | 3,197/1,872 |
| `--features audio-io` | `--test-threads=1 --skip io::tests::capture_and_playback_for_one_route_coexist` | `600b3a8e-0d68-4396-8846-db622a880e0d` | 52 | 26.403 | 5,002/981 |

All three exited zero with no failed or ignored tests and no compiler
warning/error diagnostics. The default result is 4 unit + 1 public + 9 PCM
tests; codec is 13 unit + 11 public + 9 PCM; IO is 32 unit + 11 public + 9 PCM.
The real-device coexistence definition is the sole filtered IO test. All
three doctest stages contained zero tests.

C2 parsed every passing name, matched the sets to the accepted formatted
source and verified default is contained in codec, which is contained in IO.
Thus 99 feature executions represent 52 distinct definitions: 46 new
compatibility definitions and six retained hardware-free definitions. These
counts exclude the earlier six retained-binary baseline executions. The
complete records and independent name reconciliation are retained in C2
`target/audio-extraction/package-native-records-01.json` and
`package-native-independent-review-01.json`.

The 9,831-byte native build output at manager
`target/debug/build/audiopus_sys-32e80ac21076bd05/output` records the
`audiopus_sys-0.2.2/opus` bundled source path, CMake build, MSVC 19.41.34123.0
C compiler and `cargo:rustc-link-lib=static=opus`, with the installed library
under that build directory's `out/lib`. This establishes bundled static Opus
for these Windows runs. The retained build-script stderr contains nonfatal
CMake deprecation, unavailable package-version and unused-variable warnings;
these are distinct from the clean Cargo compiler/test diagnostic streams.
The build's `Opus project version: 0` line is not treated as a semantic libopus
release identifier or a cross-platform packet golden.

C2 independently rehashed all nine package test executables and the six
native linkage artifacts listed in C1 `native-linkage-reviewed.json`.
Key original-path identities beneath
`target/debug/build/audiopus_sys-32e80ac21076bd05` are:

| Artifact | Bytes | SHA256 |
| --- | ---: | --- |
| `output` | 9,831 | `5acea85f0f17a3d7922e022181a8c6b26507f303beb0c0955866c1ed4bcd33cd` |
| `stderr` | 1,932 | `5dae52514adcd8a523b349067bfaa611ca8cfa1fbbc90dd88c0eac45e752e178` |
| `root-output` | 119 | `4114d29373b5dc78fd2cf8b8e6b5787441a82c7edbf0dc043fbf5b12f5dc634f` |
| `out/build/CMakeCache.txt` | 20,253 | `4748b7d42c929a14415c0e3264da660465768bc88b9977731a7ab3c7bbd9a19e` |
| `out/lib/opus.lib` | 1,006,870 | `1d2697e04ec6767b43fcf9fb880a6eae592c1f18305d4c7bc8089ed950cc7ea5` |
| `out/lib/pkgconfig/opus.pc` | 1,014 | `fa1f7baf26d1df96dd33654005bc1dbbcf8365710f5bd53c642ad05558c7189b` |

The cache records the Debug configuration, Visual Studio 17 2022 x64
generator, shared-library option off, float API on and fixed-point option off.
The reviewed cleanup script excludes all six files from deletion in their
original locations. The actual cleanup audit below rehashed these files and
six additional native linkage files from the later node build unchanged.

## Initial strict lint result and narrow correction

The three package lint commands share
`clippy -p allmystuff-audio --all-targets --locked --offline`, with optional
features below, followed by `-- -D warnings`. C2 read every complete record.

| Feature flags | Durable run | Result | Seconds | Stdout/stderr bytes |
| --- | --- | --- | ---: | ---: |
| None | `b203b9d7-257b-4222-9f55-8b7188c04f3b` | Exit 0 | 4.595 | 0/337 |
| `--features codec` | `4fccf42c-5507-4217-9dbe-58aac921fba0` | Exit 0 | 10.149 | 0/1,137 |
| `--features audio-io` | `3dc48957-0e0b-4781-9596-38d272fbb5da` | Exit 101 | 16.149 | 0/1,095 |

The IO failure reports only `clippy::duplicate_mod` because both original and
extracted private model modules instantiate `io_harness.rs`. Sharing one module
by re-export would make both observation paths use the same private model
implementation and invalidate the differential test boundary.

C1's minimal correction adds two explanatory comments and one
`#[allow(clippy::duplicate_mod)]` to the extracted harness module item.
C2 independently read the candidate, verified that removing those three lines
restores the exact accepted parent, and checked the sole changed path. The
1,179-byte candidate is SHA256
`da2e5dc73b1c066b217dd04b353d3a9bfaed7f6495159912a9ef1e89690db61b`,
Git blob `434c575e65183e3e12a693639da0655f9cafcdac`.
C1 committed this sole three-line addition as
`9ee9382ea81e94d610c14bba6bec886e7714ea7f`, parent `3a88e62f`; C2 verified
the exact committed bytes and all four unchanged application locks.
All assertions, shared harness and frozen bodies, production and dependency
bytes remain unchanged. The exact
[Clippy 1.97.0 lint implementation](https://raw.githubusercontent.com/rust-lang/rust-clippy/rust-1.97.0/clippy_lints/src/duplicate_mod.rs)
records each module item's lint level and excludes allowed occurrences before
checking for at least two remaining occurrences. This supports the single-item
scope. Manager integration `bcd2ca21-f3dd-4a68-ae73-7526f7e24e07` exited
zero with complete 168/0-byte streams and produced
`ad5962d3db096e1ee42d3a1ebeae184c606cab7d`. C2 verified that its entire delta
from tested `b86c9fb` is the accepted three-line test-module annotation.
Production, all assertions, frozen bodies and dependencies remain identical.

The final affected gates passed at `ad5962d3`; C2 read both exact commands and
complete untruncated records:

| Cargo arguments | Durable run | Seconds | Stdout/stderr bytes |
| --- | --- | ---: | ---: |
| `fmt --all -- --check` | `268dcb33-8f16-4b53-80ff-3f97b27f788b` | 3.419 | 0/0 |
| `clippy -p allmystuff-audio --all-targets --features audio-io --locked --offline -- -D warnings` | `7ca6bf66-d41b-47e5-ba56-c65cd9ae1261` | 1.647 | 0/192 |

Both exited zero. The strict lint emits no diagnostic, confirming the
single-item scope resolves the recorded failure. The unchanged test behavior
was already exercised by the complete package runs at `b86c9fb`; those results
are not relabeled as recompilation or execution of `ad5962d3`.

## Node integration checkpoint

At unchanged `b86c9fb31baed34a2feeb55243a90120ec669088`, all three node
compile/lint configurations passed. C2 read the complete untruncated streams
to EOF; they contain no warning/error diagnostics. These commands compile or
check the node integration and execute no node tests. All use the same private
environment as the package gates above.

| Cargo arguments | Durable run | Seconds | Stdout/stderr bytes |
| --- | --- | ---: | ---: |
| `clippy --manifest-path node/Cargo.toml --all-targets --locked --offline -- -D warnings` | `63286fb4-bff6-42a4-b736-69c5ec88a650` | 311.931 | 0/11,396 |
| `check --manifest-path node/Cargo.toml --all-targets --no-default-features --locked --offline` | `2b2af04a-c7ee-4966-ad8e-f9c72630820c` | 92.819 | 0/2,067 |
| `check --manifest-path node/Cargo.toml --all-targets --no-default-features --features audio-io --locked --offline` | `26532085-4456-4bf5-9a07-24a207169569` | 21.936 | 0/305 |

Two exact caller definitions separately check feature-based capability
advertisement and route-to-capture-source selection. C2 read their source:
they construct in-memory fixtures and call pure selection helpers, without
creating a Mesh instance or starting an audio device. Each invocation uses
`--exact --test-threads=1`; the complete records inspected so far are:

| Feature/build source | Exact `mesh::tests::` suffix | Durable run | Result | Seconds | Stdout/stderr bytes |
| --- | --- | --- | --- | ---: | ---: |
| Default, Cargo build at `b86c9fb` | `advertised_capabilities_audio_requires_audio_io` | `391a581f-ba7e-41ea-83bf-0e93aaeb0c75` | 1 pass / 330 filtered | 217.342 | 187/10,529 |
| Same default binary, checkout now `ad5962d3` | `system_audio_routes_capture_the_machines_own_output` | `998acfd9-531d-4934-ae8d-aa431b45b011` | 1 pass / 330 filtered | 0.077 | 191/0 |
| No defaults, Cargo build at `ad5962d3` | `advertised_capabilities_audio_requires_audio_io` | `fb7e7388-fbcf-435d-8168-e2c9f4888783` | 1 pass / 312 filtered | 96.776 | 187/2,131 |
| Same no-default binary at `ad5962d3` | `system_audio_routes_capture_the_machines_own_output` | `8468b11f-bc17-437e-a617-fb47be3101c9` | 1 pass / 312 filtered | 0.089 | 191/0 |

The Cargo commands use `test --manifest-path node/Cargo.toml --locked
--offline --lib`, the exact test name and the optional
`--no-default-features`. The default route case directly reuses
`allmystuff_node-96ed5898887eecd6.exe` from the preceding default build; it
does not represent a new build at the checkout's later SHA. The no-default
build produced `allmystuff_node-e94baa70d851a9e1.exe`, reused for its route
case. All four cases exited zero with no failed/ignored tests or compiler
diagnostics. They add four executions of two distinct caller definitions,
separate from the 99 package feature executions. No broad Mesh suite ran.

## macOS audio qualification within the cumulative run

[GitHub Actions run 35763211936](https://github.com/nathanfraske/AllMyStuffEncoderFraske/actions/runs/35763211936)
completed successfully on both native architectures at exact
`b86c9fb31baed34a2feeb55243a90120ec669088`, before the later test-lint
annotation qualified on Windows. Each job passed 338 selected executions across 38 suites.
The audio subset is 14 default + 33 codec + 52 IO executions on each
architecture, covering the same 52 distinct hardware-free definitions as
Windows. The cumulative [macOS report](macos-validation.md) owns the full
cross-library matrix, earlier incomplete attempt and platform limitations.

| Architecture / runner | Recorded platform and Rust | Job ID | Audio executions | Full selected executions |
| --- | --- | --- | ---: | ---: |
| x86_64 / `macos-15-intel` | macOS 15.7.9 (24G830), Rust/Cargo 1.98.0 | `106866157491` | 99 | 338 |
| arm64 / `macos-15` | macOS 15.7.9 (24G830), Rust/Cargo 1.98.1 | `106866157880` | 99 | 338 |

C2 inspected the downloaded run/job/cleanup records and independently
reconciled all 97 command records per architecture against complete retained
stdout/stderr byte counts, zero exits and cleanup outcomes. It matched each
of the 38 suites' discovered names to its passing names and independently
matched the audio profile sets to the accepted Windows source inventory.
All four LF lock hashes agree before/after and with tested Git source.
Artifact summaries are retained under manager
`target/audio-validation-20260922-01/macos-35763211936/artifacts`:

| Summary directory | Bytes | SHA256 |
| --- | ---: | --- |
| `modular-macos-x86_64-35763211936-1` | 110,610 | `ce7b143d68068bd21b59756e18825963c9163cb3f84092a761104395914fc1e1` |
| `modular-macos-arm64-35763211936-1` | 110,622 | `730cd98b4c0fd7795e73d0bd3113061c93bfc1a5e0ecc11b47ae767691da00de` |

The runner compiles package test binaries with `--locked`, then separately
discovers and runs explicit selections. IO uses the 18 compatibility cases
and the exact retained LevelStats case; it never selects the real-device
coexistence definition. PCM/public/codec cases repeat under the selected
feature profiles. All selected audio pass names agree across the three
platforms; repeated feature/platform executions are not additional distinct
definitions.

Both codec and IO Cargo JSON build records show `static=opus` with a native
search path beneath the private `audiopus_sys` build's `out/lib`. They retain
the observed linkage configuration, not a semantic native-library version
or an independently preserved library-image hash. The successful jobs removed
their private build roots after test execution, so binary hashes recorded by
the runner are not described as independently rehashed local binaries.

Both final process-cleanup receipts report clean scans, no remaining
recorded cleanup errors, and the separate always-cleanup receipts report
`private_root_removed: true` with the supervisor lease acquired. These are
routine successful-run outcomes; they do not demonstrate recovery after a
forced workflow cancellation. No microphone, speaker, desktop GUI, mobile,
live fleet or real audio backend runtime qualification follows from these
hardware-free cases. C2's detailed reconciliation is retained in
`target/audio-extraction/macos-audio-independent-review-01.json`.

## Reviewed cleanup and actual preservation

C1 prepared a bounded adaptation of the prior terminal cleanup script, and
C2 independently read both complete versions and their full diff. The exact
audio script is 12,852 LF bytes / 210 lines, raw and LF SHA256
`385b96dfb6cedd4272f2cddfb2bb83fe50bbcb2e70591514992110957174a65b`.
Its fixed manager root, path/reparse checks, three Cargo directory scopes,
five direct dependency-file extensions and no-replay plan/result guard remain.
The new deletion loop removes only checked regular files followed by empty
allowlisted directories, preserving native Opus evidence before constructing
the removal list. All other target files, including test executables and prior
proofs, are hashed before and after. Tracked source, four locks, HEAD and Git
status are also checked, and logical byte accounting includes the newly
written plan.

The accepted command requires the final reviewed full `ExpectedHead` and must
run centrally only after all local native gates finish, with no concurrent
build or evidence writes. It rejects any existing cleanup plan or result.
Preparation acceptance and native artifact verification are retained in C2
`target/audio-extraction/cleanup-source-independent-review.json`.

After all local gates were terminal, the manager ran the exact accepted script
with `-ExpectedHead ad5962d3db096e1ee42d3a1ebeae184c606cab7d` in durable run
`1f653544-c931-4925-9ba2-77c4c066ddf1`. It succeeded with exit 0 in 74.605 s;
C2 inspected both cursor pages through EOF, totaling 100,973 stdout bytes and
zero stderr bytes, without truncation. The complete stdout JSON matches the
retained result receipt.

| Actual cleanup measure | Result |
| --- | --- |
| Removed regenerable regular files | 6,374, including 1,882 direct dependency intermediates |
| Removed empty allowlisted directories | 1,071 |
| Removed summed logical file bytes | 2,577,006,776 (about 2.40 GiB) |
| Target bytes before cleanup | 3,478,119,611 |
| New plan receipt bytes | 704,099 |
| Target bytes after cleanup, before result receipt | 901,816,934 |
| Result receipt bytes | 100,976, including its UTF-8 BOM |
| Target bytes including those two receipts | 901,917,910 (about 0.840 GiB) |
| Protected target files independently rehashed unchanged | 2,140, totaling 901,112,835 bytes |
| Tracked source and application locks independently rehashed unchanged | 892 source files, including all four application lockfiles |
| Native linkage evidence preserved in original paths | 12 files across the package and node build outputs |

The arithmetic is
`3,478,119,611 - 2,577,006,776 + 704,099 = 901,816,934` before writing the
100,976-byte result. These are sums of logical file lengths, not a measurement
of freed filesystem blocks. The target totals describe the receipt checkpoint;
later audit receipts are additional retained evidence.

The manager evidence root `target/audio-validation-20260922-01` retains
`cleanup-plan.json`, SHA256
`660062350975027f823ac1fc1001dea16bd780806b2298b3ac7886561ad76540`, and
`cleanup-result.json`, SHA256
`f0c0f16094d63c786e2bfda4b1842a407bde40da53c5d2111ee10f652b1e22c9`.
C2 independently rehashed every protected target file, source file, lock and
native evidence file against that plan; checked all 1,882 direct dependency
paths and 1,071 removed directories absent; and confirmed that only the 12
protected native evidence files remained under `debug/build`.
`debug/.fingerprint` and `debug/incremental` were absent. The script itself
checked every planned file absent before emitting success. HEAD and tracked
source remained unchanged, with only the managed `AGENTS.md` untracked.
Original test EXEs, DLLs, PDBs, prior proof/log artifacts and scratch were
preserved. The independent receipt is
`target/audio-extraction/cleanup-actual-independent-review.json`. The cleanup
is complete and must not be replayed.

## Remaining limits

GitHub macOS validation is bounded to the selected matrix and recorded runner
images; its tested source is `b86c9fb`, before the separately Windows-qualified
three-line test lint annotation. No other Unix audio target, MSRV, live fleet,
real device, application IPC or GUI/mobile runtime pass is claimed. The
successful ordinary Mac cleanup does not establish forced-cancellation recovery.
