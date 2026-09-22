# allmystuff-audio

Reusable audio processing extracted from the node, with routing and application
authority kept in the caller.

| Feature | Contents |
| --- | --- |
| Default (`[]`) | PCM conversion, mono downmix, linear resampling, playback-ring trimming and the existing lane constants. No normal dependencies or audio devices. |
| `codec` | The existing mono Opus encoder/framer, a stateful decoder, the disabled capture/playback surface and the shared `AudioFrame` wire type. |
| `audio-io` | `codec` plus the existing CPAL capture/playback threads and platform system-audio capture paths. |

The `pcm` functions preserve the original arithmetic, including partial channel
groups, conversion rounding, integer samples-per-millisecond thresholds and
zero-rate handling. Resampling restarts its interpolation phase for each input
buffer. Playback trims the oldest samples only after exceeding the maximum;
the target and maximum remain 80 ms and 200 ms.

`codec::OpusStream` resamples mono input to 48 kHz and emits 960-sample packets
synchronously at the existing 96 kbps setting. It retains incomplete frames;
an encoding error consumes that frame and continues. `codec::OpusDecoder`
owns one 48 kHz mono decoder and uses the original non-FEC decode call. The
caller supplies the output buffer and retains the decoder across packets.
Errors keep their original types or strings.

`AudioFrame` remains the type defined by `allmystuff-session`. Its JSON fields,
little-endian PCM and base64 encoding are unchanged. The audio library does
not select routes, authenticate peers, advertise capabilities or send packets.

`io::AudioBridge<S>` retains the existing separate capture/playback maps,
dedicated threads, device selection, callback behavior and stop/join ordering.
`S: StatsPolicy` supplies only the decision to log periodic statistics at info;
the default `DebugStats` keeps them at debug. The policy is called at the
original emission point. Moved logging retains the `allmystuff_node::audio`
target and existing messages.

System capture preserves the implemented platform behavior: Windows uses
WASAPI loopback with input fallback; Linux loads PulseAudio's simple monitor
API at runtime with input fallback when opening it fails; macOS and other
platforms use the existing logged default-input fallback. This extraction adds
no new device or loopback backend.

The node always enables `codec`, including without its `audio-io` feature.
Its real adapter supplies the existing shared video-statistics policy. Its
disabled adapter explicitly selects `disabled::AudioBridge` and the original
encoder refusal even if another consumer enables device I/O on this package.
Mesh keeps its decoder map, lazy creation, route teardown, 120 ms output
allocation, authorization checks, lane selection, queue limits and transport.
Desktop and mobile feature selections remain unchanged.

The seven original test bodies are retained across `pcm`, `codec` and `io`.
`io::tests::capture_and_playback_for_one_route_coexist` opens real audio
devices and must be excluded from device-free test runs. The private output
callback macro is shared with test fixtures so its ring-draining behavior can
be exercised without CPAL device access.

Windows x64 and macOS 15 on arm64 and x86_64 each passed the same 52 distinct
hardware-free audio tests: 14 with default features, 33 with `codec` and 52 with
`audio-io`, totaling 99 executions per platform. The tests compare independently
stateful old/new codecs using the same linked native library; they do not define
universal Opus packet goldens. Windows formatting and strict library lint checks
also passed. The [extraction report](../../docs/reviews/modular-foundation/audio-library-extraction.md)
records exact source commits, feature graphs, caller checks, native linkage and
the cumulative Mac run. Real device capture/playback, loopback, permissions,
live-fleet transport, GUI/mobile integration and Linux audio execution remain
outside this runtime qualification.
