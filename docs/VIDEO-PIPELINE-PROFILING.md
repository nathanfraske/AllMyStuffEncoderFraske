# Video pipeline profiling

The development profiler measures the video pipeline's local busy and wait
boundaries without changing any media or control payload. It is strictly
opt-in, disabled by default, bounded in memory and trace size, and never sends
profiling data to a peer or signaling service.

## Enable it

Set the variables before starting `allmystuff-serve` or the desktop app:

```powershell
$env:ALLMYSTUFF_VIDEO_PROFILE = "1"
$env:ALLMYSTUFF_VIDEO_PROFILE_TRACE = "D:\allmystuff-video-profile.jsonl"
```

`ALLMYSTUFF_VIDEO_PROFILE=1` enables five-second p50/p95/p99 log summaries.
The trace variable additionally writes exact local `duration_ns` samples to a
bounded asynchronous JSONL file. It also enables the profiler by itself.

Optional bounds:

- `ALLMYSTUFF_VIDEO_PROFILE_INTERVAL_MS`: summary interval, clamped to
  1,000-60,000 ms; default 5,000.
- `ALLMYSTUFF_VIDEO_PROFILE_MAX_SERIES`: route/stage series, clamped to
  16-2,048; default 256.
- `ALLMYSTUFF_VIDEO_PROFILE_TRACE_EVENTS`: file event limit, clamped to
  100-1,000,000; default 20,000.

The writer queue is fixed at 2,048 events and flushes at least every 250 ms.
If profiling cannot keep up, it drops profiler events and logs the count; it
never backpressures video.

## Stages

The capture/encode side records capture cadence and source-age gauges,
conversion work, encoder queue wait, codec work, and packet callback delivery.
The transport side records the route handoff queue, deliberate slice-pacing
waits, payload serialization/copy work, and local media-pipe lock,
connect/handshake, and write waits. The receive/decode side records local pipe
cadence/read, binary parsing, dispatch admission backpressure, actual dispatch
queue residence, decoder queue and paced-fragment coalesce waits, decoder
preparation, fused codec/pixel-conversion work, and decoded-frame callback
time. The local viewer boundary then records decoded/encoded packet residence,
consumer poll cadence, watcher-lock contention, batch preparation, and the
backend's non-empty local IPC write. Desktop node-socket video polls use a
segmented writer, so `viewer_batch_busy` is framing/length preparation only;
the mobile/in-process compatibility path still includes its contiguous copy.

H.264 keeps one process-local frame id from encoder input through the outbound
pipe. Asynchronous hardware encoders use a bounded accepted-input FIFO, so an
oldest-first output backlog is attributed to the input that produced it rather
than the newest encode call. On receive, paced samples sharing peer, stream,
and RTP timestamp reuse one local AU id before native decode. These ids are
Rust-only fields on in-process wrappers; none is encoded into RTP, media,
control, signaling, ICE, STUN, or TURN bytes. MJPEG begins a fresh id at its
route handoff because its public packet wrapper deliberately remains unchanged.

Each JSONL event carries a `kind`: `busy`, `queue_wait`, `io_wait`,
`pace_wait`, `delivery`, `cadence`, or `gauge`. These are wall-time
classifications, not CPU-sampler claims. For example, `decode_busy` can contain
a synchronous GPU wait and mandatory pixel conversion. `capture_age` is a
gauge, while `capture_wait` and `inbound_pipe_wait_read` include normal frame
cadence; the analyzer never adds those to a local stage sum.
`frame_delivery` stops when the local callback returns; browser or native
presentation needs a separate paint marker to claim glass-to-glass. Both
delivery callbacks are reported standalone and excluded from additive sums
because they wrap downstream work. `viewer_poll_cadence` is likewise a cadence,
not latency to add to a frame.

The viewer stages stop at the backend side of local IPC. A native 1440p frame
is roughly 14 MiB of RGBA, so WebView transfer, JavaScript parsing, canvas
upload, and browser paint can dominate even when NVDEC itself sustains 60 fps.
Those opaque frontend stages are intentionally not presented as measured by
this backend profiler.

## Reproducible Windows field run

The runner stops only the local GUI/backend, reuses the pinned running mesh
daemon (preserving established ICE sessions), starts the requested candidate
with profiling enabled, waits until the exact authenticated peer or source is
advertised, runs the pinned production probe, flushes the trace, and restores
the installed GUI. Add `-RestartMesh` only for an intentional cold transport
test. It refuses an unreviewed probe hash and never uses the probe's diagnostic
no-ICE-proof bypass.

```powershell
.\scripts\run-video-profile.ps1 `
  -Peer '<remote-node-id>' `
  -Mode game `
  -Decoder nvdec `
  -Seconds 8 `
  -Cycles 1 `
  -NoRewatch `
  -Label 'game-nvdec-1440'
```

Use `-Source '<exact-screen-capability-id>'` instead of `-Peer` for a monitor
switch matrix. Keep the same backend alive while switching by adding a
semicolon-separated `-SwitchSources '<second-id>;<third-id>'`; every route gets
its own probe log in the same correlated trace. Use `-ResizeEdge 1280` for a
720p-scale run. Each result lands
under `artifacts\profiles\<label>` with the exact trace, probe/backend logs,
summary, hashes, and run manifest. The probe's route and tune messages use the
existing authenticated ICE data channel; profiler data remains on local disk.

For the reverse direction, `-RemoteScript '<reviewed-script.ps1>'` runs that
script through the pinned `p2_remote_transport` terminal harness while the
local profiled backend stays up. Terminal commands and resulting video both
remain on established authenticated ICE data channels; no payload is placed on
the signaling layer.

## Summarize a trace

```powershell
python scripts\summarize_video_profile.py D:\allmystuff-video-profile.jsonl
```

The analyzer prints exact-sample average/p50/p95/p99/max values and the largest
selected non-overlapping process-local stage sums. The sums deliberately omit
cadence/age gauges and both overlapping delivery callbacks. They are useful for
finding local outliers, but are not labeled end-to-end latency. Start with
summary-only profiling for a representative run, then use a short JSONL capture
around the slow interaction to minimize observer overhead.

For a two-box run, collect one trace on each box and summarize both:

```powershell
python scripts\summarize_video_profile.py sender.jsonl receiver.jsonl
```

Do not subtract the files' monotonic timestamps. Their clocks have different
origins. RTP timestamps are correlation labels only. Exact cross-box network
transit or glass-to-glass latency requires an external synchronized capture or
an explicit data-plane timing experiment; the production profiler does not add
wire metadata merely to make that number look exact.
