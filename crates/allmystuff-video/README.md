# allmystuff-video

Video processing shared by AllMyStuff callers. The default feature set provides
metadata, timing, pacing, access-unit classification, closing markers and
receive/handoff policy without a native codec, capture device or transport.

| Feature | Contents |
| --- | --- |
| default (empty) | `metadata`, `timing`, `pacing`, `codec`, `framing`, `receive`, `ingress`, and `handoff` |
| `decode` | Per-route software receive workers, RGBA output policy, OS performance helpers, and the existing capture-less video control surface |
| `host` | `decode` plus the existing capture/encode group and platform hardware backends |
| `hwenc` | `host` plus the existing optional FFmpeg encoder ladder |

Node enables `decode` in every build and forwards its existing `host` and
`hwenc` flags. The capture and encode code remains grouped as before; these
features do not introduce new backend priorities or independent hardware
capability claims. Windows `host` builds retain AV1 dispatch through NVDEC and
D3D11VA, whose `open` and `decode` methods currently return not-yet-implemented
errors. Other `decode` builds retain the unsupported-platform AV1 fallback.

`framing::split_annexb_paced_host` and `framing::split_annexb_paced_stub` preserve
the two existing walks, including their different malformed-prefix behavior.
`codec::sniff_codec` classifies the original byte patterns; it is not a validator
or a declaration that a decoder backend exists.

The caller owns peer authentication, route binding, IPC headers, queue admission,
recovery requests and process supervision. `receive::accept_paced_fragment`
retains the route fallback's existing chunk/byte limits and lack of age expiry;
`ingress::Freshness` retains the separate canonical peer/lane policy, including
its age checks, first discontinuity reason, and Reset/Gradual admission rules.
`ingress::Sink` performs one synchronous attempt on the application's existing
queue and reports the actual sent/full/closed outcome. A suppressed dependent
frame can return success without attempting a send, even if the queue closed.
Node retains the daemon protocol frame, authenticated binding, channel and
transport loop. Its conversions move the existing payload and peer string.

`handoff::VideoHandoff<P>` retains residence and byte budgets, whole key suffix
selection and missing-reference fences. Its application policy supplies the
existing key test and batch envelope; node uses the original `[2,1]` key prefix
and four-byte little-endian packet length. Queue packet layout and accounting
remain unchanged, including `replace()` taking its own monotonic timestamp.

With `decode`, `decode::DecodeBridge<RgbaOutput>` produces `RgbaFrame` values
without an application header. A caller can supply `DecodeOutput` to allocate
its final frame object and expose its writable RGBA region. Node's compatibility
adapter retains the exact header allocation, resize and tail slice, so decode
writes into one final `Vec` without an intermediate full-frame allocation.

With `host`, `video::VideoBridge<D>` accepts an application `DesktopFollower`
type. Windows capture constructs it at the original point inside its pump
thread. No desktop handle moves across threads and the trait adds no `Send`
requirement. Node delegates to its existing privilege implementation.
`os_perf` and `wake` each have one implementation: node reexports that same
state, including the media-thread registry and awake/timer guards. Moved logging
sites keep their original explicit tracing targets and environment controls.

The package has no dependency on `allmystuff-node`, a GUI, or a Mesh runtime.
Existing timing, pacing, metadata and pixels packages remain implementation
dependencies. Platform capture discovery still uses its original application
model/path helpers. Runtime capability negotiation remains caller-owned.

The [compatibility review](../../docs/reviews/modular-foundation/video-library-extraction.md)
records source comparisons, central validation and remaining qualifications.
Hardware tests may open real capture/GPU resources and are not covered
by an unqualified whole-library test command; the reviewed central test plan
selects isolated policies and software codec paths explicitly.
