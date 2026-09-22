# Audio compatibility fixtures

These fixtures compare the extraction against source frozen at
`e6340b31daa6d9c2058c4ee345564d3e0c0ecebf`. The [baseline](baseline/README.md)
was reviewed and committed independently before implementation adaptation.
Its literal sample bits, arithmetic, ring tails, refusal text and wire bytes
are never regenerated from the extracted implementation.

| Surface | New definitions | What is checked |
| --- | ---: | --- |
| `pcm_compatibility` | 9 | Conversion asymmetry, nonfinite inputs, partial downmix groups, full channel bound, stateless resampling and exact trim tails/thresholds. |
| `contract_compatibility` | 1 default + 10 with `codec` | Public constants/source variants, explicit disabled refusal/no-op/drop/log behavior, canonical `AudioFrame` type and frozen JSON/PCM contracts. |
| `codec::compatibility` | 8 | Independent old/new encoder state, cadence, fractional resampling, callback-panic residue, persistent decoding, PLC, malformed packets, short output buffers and recovery. |
| `io::compatibility` | 18 | Seeded feed/maps, output-rate changes, trim boundaries, duplicate guards, fake-worker stop/join ordering, statistics, output callback macro, user-callback lock release and static policy bounds. |

The package also retains six hardware-free original definitions: four under
`pcm::tests::`, one under `codec::tests::`, and the exact
`io::tests::level_stats_flag_a_pure_silence_window`. Planned totals are 14 with
default features, 33 with `codec`, and 52 with `audio-io` when the device test
below is excluded. These are source inventory counts; discovery and execution
must establish actual selected counts.

**Do not run an unfiltered `audio-io` test suite.** The retained
`io::tests::capture_and_playback_for_one_route_coexist` calls real device startup
despite its historical CI comment. The narrow IO selections are
`io::compatibility::` and, in a separate invocation with `--exact`,
`io::tests::level_stats_flag_a_pure_silence_window`. A module prefix must not be
combined with `--exact`. PCM/public/codec fixtures can run separately with the
desired feature selection. Managers own native execution and outer deadlines.

`support/frozen_pcm.rs`, `support/frozen_encoder.rs` and
`support/frozen_io.rs` retain original bodies with adjacent span manifests.
Only the IO oracle's statistics-policy lookup is redirected. Its observation
suffix copies the original output macro into a device-free wrapper. The shared
`io_harness.rs` only constructs private test state, calls the implementation and
observes results. Frozen modules skip formatting so their original text remains
auditable; authored fixtures and adapters remain subject to normal formatting.

IO fixtures use no device discovery or streams. The only start calls first
assert the duplicate-map preconditions that return before native worker
creation. Fake workers use `try_lock`, observe the stop signal for at most five
seconds, and are joined by the original drop paths. Test-model drop cleans all
seeded routes. The two user callbacks use a bounded channel exchange to prove
that metering releases its statistics lock before invoking the consumer. No
global environment, application state, fleet, network or user directories are
accessed. Native capture backends, five-second permission/starvation warnings,
device fallback and races between real starts remain source-reviewed limits.

Opus packet bytes and decoded samples are compared between independent
instances using the same linked native library. They are not universal codec
goldens across platforms or libopus versions. Cargo's `audiopus_sys` pin alone
does not identify the native implementation: its build can select pkg-config,
an explicit library directory or bundled sources. Actual build evidence must
record that distinction. No fault hook forces the native encoder error branch.

At this source checkpoint the new fixtures have not been compiled or run.
The [extraction report](../../../docs/reviews/modular-foundation/audio-library-extraction.md)
separates retained baseline results from later extracted-package validation.
