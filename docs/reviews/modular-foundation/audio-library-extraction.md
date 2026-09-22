# Audio library extraction evidence

This is the source checkpoint for extraction of `allmystuff-audio` from
`e6340b31daa6d9c2058c4ee345564d3e0c0ecebf`. Production source and independent
baseline review are complete; final fixture review, inclusion, native build,
formatting, lint and extracted-package execution are pending. It does not claim
audio-device qualification or a macOS pass.

The operator separately requested an inventory of extraction locations,
optimization opportunities and future MyOwnMesh adaptation. Those proposals
remain separate from this behavior-preserving change; no pin upgrade or
speculative optimization is part of this implementation.

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
consumer version differences. Central `--locked` graph/build execution is
still needed to qualify this static seed. No registry version refresh is
authorized by the extraction.

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

## Pending qualification

The new 46-definition fixture set, six retained safe definitions and exact test
includes still require peer acceptance followed by manager-owned discovery,
formatting, native build, lint and focused execution. GitHub macOS validation
belongs to the separately reviewed cumulative runner. No macOS architecture,
other Unix target, MSRV, live fleet, device, IPC or GUI/mobile runtime pass is
claimed at this checkpoint. Final outcomes, corrections and cleanup will be
appended from actual retained evidence.
