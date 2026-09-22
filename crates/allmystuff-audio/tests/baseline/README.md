# Frozen audio compatibility baseline

Source commit: `e6340b31daa6d9c2058c4ee345564d3e0c0ecebf`.

The [manifest](manifest.json) records all 22 complete input identities, 14 exact
Git-source snapshots and five independent literal data files. These were frozen
before reading the extracted implementation. Snapshot files are data, not Cargo
test targets; disjoint ranges in a snapshot are concatenated without rewriting
the original bytes. The manifest gives each inclusive source range.

The complete real and disabled implementations, `AudioFrame` wire definition,
relevant Mesh encoding/decoding/routing/cleanup, feature selection, logging
policy and four application/workspace manifests are retained. All four original
lockfile identities are recorded, preserving external-version provenance for
the later extraction review.

The literals preserve the source's actual arithmetic and ordering:

- Float capture scales by 32767 after clamping; output conversion divides by
  32768. Float inputs are stored as IEEE 754 binary32 bits, including infinities
  and NaN.
- Downmix treats zero channels as mono, includes a partial last channel group
  and truncates signed integer averages toward zero.
- Resampling uses a fresh interpolation phase and floored output length for
  each buffer. Zero rates return the original samples. Splitting a fractional
  conversion into buffers can change the output; no streaming carry is implied.
- Playback trims only after exceeding the maximum, keeps the newest target
  tail and computes samples per millisecond with integer division. Empty input
  can trim an already oversized ring; rates below 1000 retain at most one sample
  without trimming and discard a larger ring to a zero-length target.
- PCM JSON uses little-endian signed samples and standard padded base64. A
  decoded trailing odd byte is ignored. All five frame fields remain required;
  zero channels and rate are accepted by the frame model.
- Encoder framing is 960 samples at 48 kHz, with synchronous emission and a
  retained remainder. Callback panic happens before offset advancement/drain;
  encoding errors consume a frame and continue. Exact packet bytes require
  paired original/new encoders linked to the same libopus, not a universal
  cross-platform packet golden file.
- Capture and playback have separate maps. `stop` removes both records before
  dropping them outside the guards; `stop_all` clears and joins under each map
  guard. Duplicate-start check/insert race windows are part of this baseline.
- The original video stats dial is shared only by a host build; audio-io without
  host uses the video stub's false policy. It must remain lazy at the old call.

The six helper/codec/buffer tests in the original real module are hardware-free.
`capture_and_playback_for_one_route_coexist` calls the real CPAL starts despite
its CI comment. Keep its source, but exclude it from device-free test commands.
New lifecycle coverage must use explicit fake route resources instead of
depending on whether the machine happens to have a microphone or speaker.

This checkpoint establishes source and data identities only. No worker executed
Rust, a formatter, compiler, test, codec or audio device. Manager-run baseline
and extracted-package outcomes are reported separately when actual evidence is
available.
