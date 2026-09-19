# Remaining modularization opportunities

**Status update (2026-09-19).** The [local IPC client extraction](ipc-client-extraction.md)
is complete. The encoded-video, receive-policy and decoder/backend boundaries
now live in one `allmystuff-video` package; the [video extraction report](video-library-extraction.md)
tracks its current validation and qualifications. The ranking, proposals and
source anchors below are preserved as a historical audit at the stated revision.

Source review by C2 at `6602922dfa7c4eba937e1c8fa4d8eda200da010b`.
This is a ranked proposal after the timing, byte-queue, pacing, update-policy,
inventory-model and video-metadata extractions. It changes no application
code and records no new build or runtime qualification. RISC-V remains an
[experimental checkpoint](../portability/openh264-riscv64.md); further codec
diagnostics and doctest work are stopped for this slice.

Start with the node IPC client. It has two existing implementations and real
consumers on both sides of the current crate boundary. The other candidates
separate media policy and storage decisions from the node runtime. Moving
`mesh.rs` methods into sibling files would improve navigation, but would not
let another application use them without the node's dependencies.

| Rank | Boundary | Concrete benefit | First bounded change |
| --- | --- | --- | --- |
| 1 | Local node wire/client | One client for the desktop GUI and `amst`, usable without the node engine. | Extract framing, request/response/event shapes, address resolution and client methods; leave server dispatch and process supervision in the node. |
| 2 | Encoded-video byte rules, then receive policy | Shared host/receive-only byte logic and reusable reference-aware buffering without codecs, capture or Mesh I/O. | Finish the existing video-metadata library's pure marker/split/inspection boundary before extracting receive state. |
| 3 | Fleet storage-plan model and merge policy | Typed plans and deterministic convergence usable without SQLite, file watching or a live node. | Extract records and validation first; then separate in-memory transitions from durable commit. |
| 4 | Video decoder workers and backend interface | A viewer can use native decoding without the node's storage, scanning, daemon control or GUI runtime. | Remove dependencies on `mesh` packet formatting and host-video configuration before packaging the worker/backends. |

**1. Local node wire/client — strongest next extraction.**

The node's [frame codec](../../../node/src/node_control.rs#L65),
[request type](../../../node/src/node_control.rs#L138),
[events](../../../node/src/node_control.rs#L254),
[socket address](../../../node/src/node_control.rs#L297) and
[NodeClient](../../../node/src/node_control.rs#L346) share a module with
`Mesh` dispatch, socket serving and binary supervision. The terminal has an
explicit [independent mirror](../../../crates/allmystuff-term/src/client.rs#L1)
because it must avoid linking the engine. Its framing begins at line 83 and
its second `NodeClient` at line 177. The desktop
[imports the node implementation](../../../gui/src-tauri/src/main.rs#L48),
and its [manifest enables node defaults](../../../gui/src-tauri/Cargo.toml#L54).
This binary application IPC contract is distinct from the MyOwnMesh
[control/media transport](../../../node/src/control_client.rs#L1) and the
mobile-core [control-message builders](../../../crates/allmystuff-mobile-core/src/control.rs#L25).

A small `allmystuff-node-client` package could own the existing wire and
async client, with node and terminal compatibility reexports/adapters.
Expected dependencies are Serde/JSON, Tokio, interprocess, error/logging
support and the current state-path helper; no reverse dependency on
`allmystuff-node`, scanners, codecs, Tauri or updater. The existing helper is
in [allmystuff-protocol](../../../crates/allmystuff-protocol/src/control.rs#L20),
which still depends on graph and `dirs`; describe that closure accurately
rather than calling it dependency-free. A separate wire-only package is
unnecessary unless a consumer actually needs to avoid the async/socket layer.

Keep `[u32 BE length][tag][payload]`, tag values, the 256 MiB read ceiling,
JSON defaults, subscribe acknowledgement and writer-half lifetime unchanged.
Retain one connection per request and the caller-specific event/error
handling: the node client logs malformed events whereas the terminal mirror
silently skips them. The current readers also return `None` for an
`UnexpectedEof` while reading the four-byte length, including a partial
length; their comments describe a stricter boundary than the implementation.
Do not change that behavior incidentally during deduplication.

The client does not acquire authority to execute commands. Keep socket
[binding/access control](../../../node/src/node_control.rs#L622),
[dispatch](../../../node/src/node_control.rs#L860), runtime ownership and
takeover, and [owned-child cleanup](../../../node/src/node_control.rs#L2140)
in their current host integration. Preserve Unix state-home/socket lookup
and Windows pipe naming. Existing framing/serde tests start at
[node_control.rs:3090](../../../node/src/node_control.rs#L3090). A future
extraction should add external-consumer fixtures for JSON/bytes/error tags,
subscribe/upgrade/restart events, truncated frames and disconnected peers,
plus isolated socket compatibility checks on supported hosts.

This first change would deduplicate `amst` and expose a reusable client;
it would **not yet remove the desktop's node dependency**. The GUI also uses
`ensure_node_running`/`NodeChild`,
[daemon installation/repair](../../../gui/src-tauri/src/main.rs#L3828),
[daemon discovery](../../../gui/src-tauri/src/main.rs#L4166) and
[diagnostics settings](../../../gui/src-tauri/src/main.rs#L4377).
Those require a later narrow host-services boundary or local RPC ownership
decision. Mobile currently embeds the
[full Mesh engine and dispatch](../../../gui/mobile/src/engine.rs#L31), so
it cannot switch to an IPC-only client as an incidental consequence.

**2. Encoded-video byte rules and receive policy — proceed in small stages.**

The pure paced-AU marker builder/parser and splitter remain in both
[host video](../../../node/src/video.rs#L4957) and the
[receive-only stub](../../../node/src/stubs/video.rs#L124). The host splitter
uses the extracted Annex-B walker; the stub still has its own byte walk.
Codec/clean-entry inspection remains in
[video_decode.rs:54](../../../node/src/video_decode.rs#L54), although it only
examines bytes. Extend `allmystuff-video-metadata` with these coherent byte
operations, with existing node paths retained as shims. This is a useful
first slice without inventing another package or changing the wire format.

Keep the exact 26-byte closing marker, including its prefix, UUID and trailer,
and its little-endian count. The parser accepts every `u16` count, including
zero, but rejects other shapes. Preserve NAL-header distinctions,
header-to-slice grouping and byte-identical concatenation. An oversized single
slice stays whole; a parameter-less HEVC delta or start-code-less AV1 input need not acquire new
split behavior. Codec recognition is not proof that a decoder is available
or that the bitstream is valid. Cross-check host/stub outputs, including
empty/truncated input, three/four-byte prefixes, the H.264 `0x41` collision,
HEVC and malformed markers. Existing vectors cover the
[splitter](../../../node/src/video.rs#L5696) and
[closing marker](../../../node/src/video.rs#L5736); preserve the separate
[real codec compatibility test](../../../node/src/video.rs#L5832) rather than
replacing it with byte-only tests.

The walkers are not behavior-identical on every input. Source tracing of
`[0,0,1,0,0,1,0x65,0,0,1,0x61]` gives host NAL offsets `[0,3,7]` but stub
offsets `[0,7]`: the [shared walker](../../../crates/allmystuff-video-metadata/src/lib.rs#L56)
examines each `0x01`, while the [stub](../../../node/src/stubs/video.rs#L149)
advances beyond the header byte. At `max_chunk = 7`, that implies host ranges
`0..7, 7..11` versus stub `0..11`. This is a source-derived malformed-input
case, not an executed test. Freeze differential fixtures before consolidating;
preserve existing differences or obtain a separate behavior-correction decision.

Next, assess a codec-free receive-policy module/package using the existing
metadata and frame-timing libraries. Its source is the
[sequence decision](../../../node/src/mesh.rs#L1864),
[paced assembly](../../../node/src/mesh.rs#L1927),
[daemon-ingress freshness state](../../../node/src/control_client.rs#L102)
and [VideoHandoff](../../../node/src/video_handoff.rs#L34). Current callers
are [the media reader](../../../node/src/control_client.rs#L590),
[route ingress](../../../node/src/mesh.rs#L5964) and
[viewer enqueue](../../../node/src/mesh.rs#L6266). Reuse is plausible for a
standalone viewer or another transport adapter; those are proposed consumers,
not current integrations.

Keep time/queue outcomes explicit and socket tasks, tracing configuration,
route lookup and recovery-message sending in adapters. Do not collapse the
different stages into the generic byte queue: encoded references cannot use
arbitrary oldest-chunk eviction, while decoded/JPEG frames can be replaced.
Preserve reset versus gradual recovery, marker-before-frame ordering, peer/
lane scoping, sequence wrap/duplicate rules and watcher-token ownership.
The ingress path's `try_send` result changes state; merely returning a list
of events without feeding back full/closed/sent outcomes would change it.
The two assemblers are not interchangeable today: ingress is keyed by
canonical peer/lane and has a one-second inactivity check; the route fallback
is keyed by route and has no equivalent age check. Preserve these differences
until a separate behavior change is justified.

`VideoHandoff` itself uses only `std`, but it recognizes key packets by their
first two local IPC bytes and packs `u32 LE` lengths. Coordinate its API with
the [28-byte video packet header](../../../node/src/mesh.rs#L20517); do not
pretend it is a format-independent queue. `replace()` also calls
`Instant::now()` internally, unlike `push_h264`'s supplied instant; a clock
boundary is proposed, not already complete. Its five tests start at
[video_handoff.rs:148](../../../node/src/video_handoff.rs#L148).
Keep the [ingress pressure/audio-interleave tests](../../../node/src/control_client.rs#L946),
[sequence tests](../../../node/src/mesh.rs#L22012) and
[paced reassembly tests](../../../node/src/mesh.rs#L22863) with their adapters.
Use externally supplied instants for additional deterministic traces.

Assembly is not authorization: the
[route/sender/media gate](../../../node/src/mesh.rs#L17275), bilateral paced
selection and route generations remain required around it. The existing
[first-fragment AU bound follow-up](myownmesh-followups.md) is a separate
behavior issue; extraction must not claim a complete 16 MiB ceiling or
silently fix it. No Mesh pin or transport contract change is needed here.

**3. Fleet storage-plan model and merge policy — useful after client/media seams.**

[storage_plan.rs](../../../node/src/storage_plan.rs#L17) combines Serde
records, per-record ordering, validation, merge rules, locking, state-path
discovery and JSON persistence. Current consumers are
[node-control argument decoding](../../../node/src/node_control.rs#L1427),
[authenticated fleet message handling](../../../node/src/mesh.rs#L5408) and
[local storage commands](../../../node/src/mesh.rs#L15871). Extracting a
Serde-based model first would let a Rust management client, plan inspector
or simulation consume typed plans without linking node/Fleetfiles/SQLite.
Those additional consumers are opportunities; the current Rust callers are
inside the node.

Keep `StoragePlanStore`'s file location and durable adapter in the host.
Then separate deterministic transitions over the ordered maps, with an
explicit commit result, from the mutex and atomic write. The core can use
`std`, Serde and JSON if it retains the current serialized digest; it needs
no daemon, scanner, rusqlite or watcher. Copying the entire store into a new
crate while retaining global path discovery would not establish that seam.

Preserve camelCase/legacy `ordinaryReplicas` decoding, the counter/actor
ordering, 512-entry limits, chunk size 16, default/validation ranges, resource
identity encoding and digest bytes. Preserve each operation's actual failed-
write behavior: [merge restores its full snapshot](../../../node/src/storage_plan.rs#L339),
whereas [setters restore affected records](../../../node/src/storage_plan.rs#L179)
after advancing a counter. A refactor must not silently turn that into a new
transaction contract. Existing seven tests begin at
[storage_plan.rs:449](../../../node/src/storage_plan.rs#L449); add public
serde/merge vectors and disposable persistence-failure fixtures when implementing.

`sender_may_manage` is a trusted caller decision, not a wire grant. Keep the
fleet-network/authenticated-sender checks before merge, manager policy edits
and relays, ordinary members' own-device/own-author restrictions, and local
volume/capacity checks. An extracted model must not mint Mesh authority or
materialize storage roots. Keeping those checks explicit matters more than
reducing this file's size.

**4. Video decoding as a selectable library — larger follow-up.**

[DecodeBridge](../../../node/src/video_decode.rs#L214) already has useful
`feed`/`stop` and frame/glitch callbacks, with one worker per route. However,
it still reaches into `mesh` for packet headers, `video` for diagnostics,
`os_perf` for thread policy and node-local hardware backends. The software
OpenH264 decoder is unconditional in the
[backend enum](../../../node/src/video_decode.rs#L336) and
[node manifest](../../../node/Cargo.toml#L196). Windows HEVC also depends on
[NVDEC frame types](../../../node/src/d3d11va.rs#L64) and
[GPU device creation](../../../node/src/d3d11va.rs#L1021).

First make decoded frame metadata/pixels and recovery outcomes independent
of the local IPC envelope; preserve the current packet bytes in a node
adapter. Supply backend construction and runtime/logging policy through a
narrow seam. Then package the worker with software and platform backends
as separately selectable implementation dependencies. A standalone viewer
could use this without storage, inventory or daemon supervision. Current
desktop and embedded-mobile engine paths would continue using the same
selected backends; their behavior and defaults must stay unchanged.

Preserve bounded queue overflow, clean-entry fencing, local automatic/software
preference, dead-worker restart, backend fallback/demotion, stop/join and
resolution changes. Retain the existing
[worker/preference tests](../../../node/src/video_decode.rs#L1132),
[software round trip](../../../node/src/video_decode.rs#L1328) and
[resize test](../../../node/src/video_decode.rs#L1388). Hardware backend
tests remain distinct platform qualification. No source move resolves the
experimental RISC-V codec abort.

Removing decoder imports alone cannot produce a media-free Serve: Opus,
application services and their dependencies remain, as do capability
advertisement and dispatch obligations. A later selectable runtime must
wire only available implementations without advertising unusable operations.
This proposal adds no application presets or speculative MyOwnMesh v1 API.

**Acceptance for a future extraction.** Keep public compatibility paths where
practical, frozen wire fixtures and existing caller behavior. Verify a small
external consumer can depend on the new boundary without the excluded node/
GUI/native dependencies, and inspect the normal/build dependency graph for
the actual selected features and target. Record application integration
checks separately from library tests. This source-only review ran none of
those future checks and authorizes no production change by itself.
