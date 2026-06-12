# Architecture

AllMyStuff is a consumer-facing app for wiring your devices together over a
private mesh. It's deliberately split so the **model is pure and testable**
and the **mesh is a sidecar**, never an embedded dependency.

## One picture

```
                    ┌──────────────────────────────────────────┐
                    │            AllMyStuff GUI (gui/)          │
                    │  Svelte 5 graph  ──invoke──►  Tauri (Rust)│
                    └───────┬───────────────────────────┬──────┘
            scan_self()     │                           │  control socket
        (inventory+bridge)  │                           │  (line-delimited JSON)
                            ▼                           ▼
        ┌───────────────────────────┐        ┌────────────────────────┐
        │   allmystuff-inventory    │        │   myownmesh serve       │
        │   allmystuff-bridge       │        │   (separate process,    │
        │   allmystuff-graph        │        │    pinned in            │
        │   allmystuff-protocol     │        │    .myownmesh-rev)      │
        └───────────────────────────┘        └────────────────────────┘
              the library workspace               the mesh, sidecarred
```

The library workspace (`crates/`) compiles and tests with nothing but
`cargo` — no webview, no daemon, no network. The GUI is its own Cargo
workspace (`gui/src-tauri`) so a root `cargo build --workspace` never drags
in Tauri. Same split MyOwnMesh uses.

## Crates

```
crates/
├── allmystuff-inventory   # scan a machine for everything plugged in
├── allmystuff-graph       # the device graph + authorization model
├── allmystuff-protocol    # wire types: myownmesh control mirror + app messages
├── allmystuff-bridge      # Inventory ──► graph Capabilities (+ presence summary)
├── allmystuff-session     # live presence + the route offer/accept handshake + AudioFrame
├── allmystuff-updater     # self-update: release feed, SHA-256 verify, stage-then-apply
└── allmystuff-cli         # `allmystuff` (opens the GUI) + scan / capabilities / update
```

### allmystuff-inventory

Cross-platform hardware scan. Returns one `Inventory` value with stable
device ids.

- **Linux is the reference platform** and reads `/proc` + `/sys` directly:
  `/sys/class/drm` + EDID for displays, `/proc/asound` for audio (with USB
  stream channel counts → mic-array detection), `/sys/class/video4linux` for
  cameras, `/proc/bus/input/devices` for keyboards/mice, `/sys/bus/usb` for
  peripherals, `sysinfo` for CPU/RAM/disks/networks.
- **Every probe degrades to "nothing here"** on a missing file, so a scan in
  a locked-down container returns the same well-formed shape as a loaded
  desktop — just with fewer devices.
- **The fiddly decoders are pure functions with fixture tests** — EDID timing
  descriptors, ALSA capture channels, input-device classification — so
  correctness doesn't depend on the hardware being present. (The MyOwnLLM
  pattern, generalised.)
- macOS / Windows reuse the `sysinfo` host basics and scaffold the device
  classes (`system_profiler`, CIM).

### allmystuff-graph

The conceptual core, and the part that makes sharing safe. Pure data + pure
rules, no I/O.

- **Capability** — one routable thing on one node: a `(media, flow)` pair.
  `media` ∈ {audio, video, display, input, storage, generic}; `flow` ∈
  {source, sink, duplex}. Carries a `default` flag — whether it's the node's
  **current default** for its category — which the UI badges and
  `match_endpoint` prefers (after a synthetic machine endpoint) when
  auto-picking where a connection lands.
- **Route** — wires a source capability to a sink of compatible media. Only
  ever minted by `Catalog::propose_route`, which is where media, flow, and
  authorization are all checked.
- **Relationship** — `Mine` (a device you own or manage) or `Shared { person,
  grants }`. A `Grant` authorizes a shared endpoint to play one role
  (`Provide` = they source, `Consume` = they sink) for one media, optionally
  pinned to one capability. A grant is **to the person, not one machine**:
  authorization unions the grants across every node shared with the same
  person (people bring fleets — what you allow works to whichever of their
  devices is handy), and the GUI keys the person by the *owner* the devices
  advertise, so a machine of theirs that appears later folds into the same
  share. `required_grants` returns the minimal grant that would unblock a
  denied route — the "one-tap allow."
- **Ownership** (in `allmystuff-protocol` + the GUI) — distinct from a
  relationship: a device *advertises* who owns it and whether it's
  *claimable*. You can't flat-take a box — a claim only lands if the device
  was started in **claim mode** (the `ALLMYSTUFF_CLAIMABLE` flag, or its own
  "allow adoption" toggle) and is still unowned. The recorded owner is the
  authenticated claimer the mesh delivered, persisted next to the identity.

See `crates/allmystuff-graph/src/lib.rs` tests for the full behaviour,
including the person-wide grant union and the endpoint auto-pick.

### allmystuff-protocol

Everything AllMyStuff puts on a wire, with no dependency heavier than
`serde`:

- **`control`** — a hand-kept mirror of the MyOwnMesh daemon's control socket
  (`Request` / `Response` / `ServerOut`, `ClientId`). Mirroring rather than
  importing `myownmesh-core` keeps the build independent of the engine
  workspace — the exact discipline the MyOwnMesh GUI's `control_client.rs`
  documents.
- **`app`** — AllMyStuff's own peer-to-peer messages that ride *inside* the
  daemon's typed channels: a `NodeProfile` presence advert,
  `ControlMessage`s for route setup and share negotiation, the
  `OwnedRoster` fleet gossip (`CHANNEL_OWNED`), and the **virtual-rooms
  plane** (`CHANNEL_ROOMS`): `RoomMessage`s carrying a room's invites,
  join/leave presence, and chat — only the membership + chat plane; a
  room's media is ordinary routes. Peers advertise the `rooms` feature
  tag, so an older build simply never sees the channel. Authorization is
  never on the wire — a node only advertises or accepts what its local
  `Catalog` already permits.
- **The owned fleet** — claiming a device doesn't just flip a flag: the two
  machines start sharing an `OwnedRoster` on `CHANNEL_OWNED` — the set of
  devices one owner has claimed, all linked by a single shared **fleet key**.
  The owner mints the key on its first claim and hands it down on each
  adoption; every co-owned device gossips the roster and converges by version,
  exactly like a mesh roster. The roster also carries the fleet's **name**
  ("Casey") — set from the Fleet pane by any member, replacing with the
  version like membership does (an empty name is skipped on the wire, so
  older peers see the exact roster shape they always did). The name labels
  the graph's fleet section and is what new rooms are titled after. For now the key only groups devices internally
  (a later edition links it to other things). It's persisted next to the
  ownership record.

### allmystuff-bridge

The one place hardware vocabulary meets graph vocabulary. Turns an
`Inventory` into graph `Capability`s — physical devices (mic → audio source,
monitor → display sink, …) plus a synthetic per-machine set (**screen** =
display source, **control** = input sink, **keyboard & mouse** = input
source — a console forwards "whatever this machine's user does", never one
scanned device, so driving a remote works even where the input scan finds
nothing (macOS) — and **system audio** = duplex) so whole-computer flows
(screen-share, room calls, remote control) have something to land on *and*
start from. The GUI also passes its own monitor enumeration in
(`capabilities_with_screens`): each monitor beyond the primary becomes a
`screen:<id>` display source — one console tab per screen — with ids the
capture side resolves back to the same monitor. Shared by the CLI and the
Tauri backend so "what this machine exposes" is computed once.

## The GUI (`gui/`)

Tauri 2 + Svelte 5, a client of the daemon.

- **Backend** (`src-tauri/`) — `scan_self` (inventory + bridge), the live
  `mesh::Mesh` (subscribes to the presence/control/media channels, drives the
  `allmystuff-session` state machine, emits `allmystuff://session`
  snapshots), the `audio` bridge (capture → mesh → playback for active
  audio routes: cpal for mics and playback, the OS loopback — WASAPI /
  pulse monitor — when the source is the machine's `system-audio`),
  `connect_route`/`disconnect_route` commands, and
  `daemon_spawn`. The `myownmesh` daemon ships **bundled with the app**:
  `build.rs` fetches the rev pinned in `.myownmesh-rev` and stages it as a
  Tauri sidecar (`binaries/myownmesh-<triple>`, `externalBin`), so the mesh
  is there for free — `daemon_spawn` resolves the bundled binary and
  auto-spawns it. (That covers source builds and the OS bundles; the
  portable `curl | sh` install is a bare binary with no sidecar inside, so
  there the *installer* guarantees the daemon instead — reusing an
  installed `myownmesh` that meets the pin, asking an older one to update
  itself, or installing one next to the app, which `daemon_spawn` finds in
  the same next-to-the-binary slot.) `update_*` commands drive
  `allmystuff-updater`.
- **Front-end** (`src/`) — the graph. `catalog.ts` is a faithful TypeScript
  port of the graph crate's rules, so the canvas is fully interactive on
  demo data with no backend; when the backend is present it validates the
  same way in Rust and fires the real route over the mesh. Live presence +
  route snapshots merge into the catalog so the graph fills with real peers.
  A node only known from the daemon's roster (not running AllMyStuff) is
  shown but quieted and un-targetable, since it exposes no capabilities. The
  **remote console** (`Console.svelte`) is the pikvm-style session handle for
  a machine: a video-inputs tab bar over its screen + cameras, plus audio and
  control toggles, each owning the real route it set up. On the desktop each
  console opens as its **own OS window** (`open_console_window` →
  `?console=<node>` → `ConsoleHost.svelte`), so several machines can be on
  screen at once; the web preview keeps the in-page popover. The **remote
  terminal** (`Terminal.svelte`) is its sibling: the drawer's **Open
  Terminal** opens a tabbed xterm.js window per machine
  (`open_terminal_window` → `?terminal=<node>` → `TerminalHost.svelte`),
  where every tab is its own mesh route to a PTY the far side spawns — see
  the terminal paragraph under the data flow below. The **remote files**
  window (`Files.svelte`) completes the console family: the drawer's **Open
  Files** (between Remote Control and Open Terminal) opens a finder-like
  window per machine (`open_files_window` → `?files=<node>` →
  `FilesHost.svelte`) — browse, preview, upload, download, rename, delete
  over one mesh route — see the files paragraph below. The stage is a
  live video sink — it registers a per-route IPC channel (`video_watch`) and
  the backend queues each inbound packet as raw bytes (a fixed header + the
  payload; no JSON or base64 on the per-frame path) for exactly the window
  that's watching. Decode walks a ladder: WebCodecs with hardware
  preference, rebuilt on software preference after a stall, and — after a
  second stall, or from the start in a webview with no WebCodecs at all
  (Linux WebKitGTK) — the watch re-registers with `decode: true` and the
  **backend decodes natively** (`video_decode.rs`: openh264, one thread per
  route), queueing ready-to-paint RGBA frames freshest-wins that the canvas
  just blits. The tab bar shows one tab per video input the remote
  advertises — its screen, each extra monitor (`screen:<id>`), and its
  cameras. While control is on the stage captures pointer/key events,
  normalizes coordinates onto the streamed frame, and forwards them down
  the control route via `send_input`. The top bar's gear
  opens a unified **Settings panel** (`SettingsPanel.svelte`) with Networks,
  Fleet (the owned roster's shared key + members), and Updates (the
  `allmystuff-updater` controls). The **Networks** tab is itself split into
  sub-tabs (MyOwnLLM-style): **Status** (identity, create/join, approvals,
  add-a-device), **Servers** (per-network signaling / STUN / TURN, defaulting
  to MyOwnMesh's reference servers), and **Devices** (every machine and which
  network(s) it's on). Multiple networks are first-class — you're joined to
  however many, a peer may share only some, and the graph, drawer, and top bar
  all show the per-device network membership rather than pretending it's one
  flat mesh. A device asking to join surfaces as an outlined, pulsing nudge
  that opens the **approvals popup** (`ApprovalsPopup.svelte`) — the bilateral
  code grid (each side's suffix + verification code) with Approve and Decline
  (a cancel, not a deny).

  **Virtual rooms** (`RoomsBar.svelte` + `RoomPanel.svelte`, bottom-left)
  are the multi-machine layer over the same plumbing: a room is a named,
  locally-persisted member list (invites ride `CHANNEL_ROOMS`), and joining
  opens a zoom-like call panel where **everything starts off** — mic,
  camera, screen share. A fresh room defaults to being named after the
  fleet's owner ("Casey's room"); its maker is its owner and renames it
  inline from the panel title (members converge via the re-stated invite).
  Each toggle fans ordinary routes out to the members — but **room sharing
  is scoped to the room**: membership is the consent, so room legs validate
  structurally via `propose_room_route` / `proposeRoomRoute` without the
  share-grant gate, **no standing grant is ever minted**, and leaving tears
  every room-wired leg down. What happens in a room changes nothing about
  what its members may do to each other outside it. (Input injection keeps
  its backend owner/fleet gate regardless — a guest's control events are
  still dropped.) The toggles: **Mic** is the call
  (your voice → their speakers); **Share sound** is deliberately separate —
  the machine's loopback (`system-audio`), never the mic; **Share screen**
  streams to each member's display, rendered in *their* panel as live tiles
  (`RoomTile.svelte`, the console pipeline's native-decode rung); **Share
  control** wires each member's keyboard & mouse to this machine so they
  can drive it over your tile — injection still gated by the far side's
  owner/fleet rule; file sending opens the existing files session
  (owner/fleet gated); chat is a `RoomMessage` to every member. The graph
  itself has two layouts switched from the zoom controls: the radial
  default and a **grouped grid** — one labelled section per fleet (yours,
  each owner's, "unknown fleet" for devices advertising no owner). The
  top bar's **network pill** opens a menu listing every network with an
  on/off switch — disable parks the network's full config in
  `allmystuff-networks.json` (under `~/.myownmesh`) and leaves it
  daemon-side; enable re-joins from the parked config; roster files
  survive on disk in between, so approvals aren't lost (the daemon has no
  dormant-network notion — `network_set_enabled` in the Tauri backend is
  what holds the ticket).

  New/joined networks default their signaling relay + STUN + TURN to
  MyOwnMesh's semi-public reference servers (`wss://myownmesh.com`,
  `stun:stun.myownmesh.com:3478`, `turn:turn.myownmesh.com:3478` with the
  shared `guest` credential) so two devices rendezvous on the *same* relay and
  traverse NAT out of the box — and any of them is editable per network. The
  backend learns which network each peer lives on (from the channel a frame
  arrives on) and addresses control/media there, so a connection follows a
  device onto whichever network you actually share with it.

## Data flow: connecting a device

1. User taps a capability's connect dot → `store.startCapConnect(capId)`.
2. User taps a target node → `store.connectCapToNode` finds the matching
   endpoint and calls `catalog.proposeRoute`.
3. If both ends are **mine**, a `Route` appears immediately — and the
   console that manages that kind of session pops for the far machine
   (`popConsoleFor`: remote control for screen/audio/control media, the
   file manager for storage), so sending something *to* a node hands you
   its session.
4. If a **shared** endpoint isn't covered, `requiredGrants` raises the share
   sheet ("Let Alex receive your screen?"). Approving adds exactly that grant
   and completes the connection.
5. With a live daemon, the backend sends a `RouteControl::Offer` to the peer
   over `CHANNEL_CONTROL`. The peer accepts; both sides go `Active`. For an
   audio route, the source captures what its capability names — the
   machine's own playback for the synthetic `system-audio` (WASAPI
   loopback on Windows, the pulse server's monitor of the default sink on
   Linux, the default input as macOS's honest stand-in), the default
   input for a scanned mic — and streams it
   to the sink — as Opus on **MyOwnMesh's RTP audio track lane** (48 kHz
   mono, 20 ms frames) when the offer asked for it and both daemons
   speak the lane (myownmesh ≥ 0.2.4), as PCM `AudioFrame`s over
   `CHANNEL_MEDIA` otherwise, so any version skew degrades to working
   sound exactly like video's MJPEG floor. The sink's playout ring aims
   ~80 ms behind the live edge and trims itself, so audio keeps step
   with the video stream. Both ends log which device they opened, a
   *mic* capture whose first seconds are pure zeros names the OS
   microphone permission (macOS/Windows deny with silence, not an error —
   a silent system capture is just a quiet desktop), and a playback
   that's never fed says so once. The console's audio passthrough is
   deliberately **listen-only** — the remote's `system-audio` to your
   speakers, nothing back. It's a console, not a call: the far side's
   loopback captures *everything* it plays, so any audio the console
   injected (a mic leg) would ride that loopback straight back and land
   one round trip later as a trailing echo. Until echo cancellation
   exists, the console simply never opens a microphone — wiring a mic
   somewhere stays a deliberate act on the graph. A display route
   streams the routed screen — the primary for the synthetic `screen`, the
   named monitor for a `screen:<id>` capability — from a persistent capture
   session (in-house DXGI Output Duplication on Windows — xcap's recorder
   duplicates every output it walks past and leaks the handle, so only the
   first session per process ever worked — and `xcap`'s recorder elsewhere:
   PipeWire ScreenCast / AVFoundation, retried once with a fresh monitor
   handle before settling for the paced per-frame grab that is the X11 path
   and universal fallback), unchanged frames skipped, a bounded queue
   dropping stale packets under backpressure. The *transport* is negotiated
   per route: the viewer always offers `h264` (decode is covered everywhere
   — see below); when the peer's lane is free — a lane whose holding route
   already ended is taken over, and the console serializes its tab switches
   so the teardown precedes the next offer — frames ride **MyOwnMesh's
   H.264 video track lane**: openh264 in screen-content mode at native
   resolution up to 4K, bitrate budgeted from the monitor's true pixel
   count (~0.16 bpp, 8–40 Mbps), real RTP, no JSON/base64/64 KiB ceiling.
   Otherwise they fall back to the v1 **MJPEG stream** over `CHANNEL_MEDIA`
   (1280 edge, chunked JPEGs), so any version skew degrades to working
   video. Either way the console window renders packets it *pulls* per
   display tick (raw bytes): WebCodecs decodes H.264 where the webview has
   it, `createImageBitmap` the JPEGs, and otherwise the backend's **native
   openh264 decoder** hands the window RGBA frames ready to blit — maximum
   frames and minimum latency don't depend on the webview's codec support.
   The console footer's **quality pills** (Resolution / FPS / Rate /
   Codec, each defaulting to Auto) ride `RouteControl::Tune` to the
   streaming side, which restarts its capture with the picks; the codec
   pill re-offers the route on the chosen transport and picks where H.264
   is decoded. A decoder that loses its place — a corrupt unit, a rebuilt
   WebCodecs instance, a dumped native-decoder backlog — sends
   `RouteControl::Refresh` and gets an IDR in ~one round trip instead of
   sitting out the periodic interval (both asks are rate-limited and
   silently dropped by older peers). Stream integrity itself is the
   daemon's job: myownmesh ≥ 0.2.2 reassembles access units
   sequence-aware, so packet loss or a late NACK retransmit costs one
   frame, never a corrupt unit in a decoder.
   Set `ALLMYSTUFF_VIDEO_STATS=1` to print each stream's per-stage
   pipeline counters (fps, scale/encode/decode ms, bitrate, audio levels,
   skip/drop causes) every few seconds on both ends — quiet by default;
   `ALLMYSTUFF_VIDEO_FPS` / `ALLMYSTUFF_VIDEO_MAX_EDGE` /
   `ALLMYSTUFF_VIDEO_BITRATE` dial the H.264 stream without a rebuild. An input
   route carries `InputEvent`s the other direction: normalized mouse moves /
   buttons / wheel / DOM-`key` values — each move naming which remote screen
   it's normalized over, so control follows the console's selected tab —
   injected at the sink with `enigo` (plus a hand-raised
   `MOUSEEVENTF_VIRTUALDESK` move on Windows, where enigo's absolute
   coordinates can't reach past the primary monitor) — but only after the
   gate: the route must be live *and* the sender must be the device's
   recorded owner or a co-owned fleet member, so a route that merely
   auto-accepted can never type into your machine.

**A terminal session** is one more route on the same plumbing — and no sshd
anywhere. A node that can host shells advertises `"terminal"` in its
presence `features` (an additive field older peers ignore; they never show
the button). Opening a tab offers a **generic** route from the host's
virtual `…:terminal` endpoint to a viewer endpoint minted per tab — these
are deliberately *not* catalog capabilities (generic would match every
auto-wiring picker), so the graph and "Connected now" render them through a
display-only stand-in (`capabilityForDisplay`). Because a shell is exactly
as privileged as input injection, the same rule guards it **before
auto-accept can answer**: a terminal offer from a sender who isn't the
device's owner/fleet is `Reject`ed in the control handler without the
session ever seeing it, the spawn re-checks, and every inbound byte
re-checks. On accept the host spawns the user's shell in a real PTY
(`terminal.rs`, `portable-pty`: openpty on Unix, **ConPTY** on Windows;
`$SHELL -l` with fallbacks, `pwsh` → `powershell` → `cmd` on Windows) and
pumps output as `"term"`-tagged frames over `CHANNEL_MEDIA` (≤16 KiB
chunks, base64 like every media frame; unknown tags are dropped by older
peers, never errors). Flow control is end-to-end by construction: reader
thread → bounded channel → awaited sends — a slow viewer fills the queue,
blocks the reader, fills the kernel PTY buffer, and stalls the shell,
exactly like ssh. The viewer window *pulls* bytes with the video plane's
poke-then-pull watcher (`term_watch`/`term_poll` + `allmystuff://term-ready`
— the queue is created eagerly when the route activates, so the prompt that
races the window boot is buffered, not dropped) and feeds xterm.js;
keystrokes and resizes ride back as the same frames via `term_send`. The
shell's exit (a dedicated wait-thread — ConPTY readers don't EOF until the
master drops) reports `Exit { code }` to the viewer's overlay and tears the
route down; a host whose viewer vanished silently kills the session after
60 s of failed sends.

**A files session** rides the very same plumbing, request/response instead
of byte-stream. A node that can serve its disk advertises `"files"` in
presence `features`; opening the window offers a generic route from the
host's virtual `…:files` endpoint to a per-window viewer endpoint (display
stand-ins again — never catalog capabilities). Handing over a disk is as
privileged as a shell, so the identical owner/fleet gate screens the offer
before auto-accept, and every inbound request re-checks. Requests (`list`,
`read`, `write`, `mkdir`, `rename`, `delete`) and responses (`entries`,
`chunk`, `ok`, `err`) travel as `"file"`-tagged frames on `CHANNEL_MEDIA`,
each carrying a viewer-minted `req` id so a listing, a preview and an
upload never tangle. Host-side (`files.rs`) each op runs on its own
blocking thread feeding a bounded channel — a big `read` is throttled by
the mesh send, with a per-route cancel flag so teardown ends it mid-stream;
upload pieces are the one inline op (they must apply in arrival order, and
each is one small append). Viewer-side the window pulls frames with the
same poke-then-pull watcher (`file_watch`/`file_poll` +
`allmystuff://file-ready`) — except **downloads, which never cross the
webview**: `file_download` registers a backend sink first, the chunks
stream straight into the local Downloads folder (unique-ified names,
partials deleted on failure or teardown), and the window just renders
`allmystuff://file-progress` / `file-saved` events.

## Persistent state

AllMyStuff rides on MyOwnMesh's identity + roster (under `~/.myownmesh/`,
overridable via `MYOWNMESH_HOME`). Its own additions — relationships, grants,
rooms, and saved routes — are app state layered on top; the mesh provides
the cryptographic identity that those grants attach to. Two of them are
durable today: rooms (each device keeps its own list, localStorage-side)
and **disabled networks** (`allmystuff-networks.json` — the parked configs
the network pill's off-switch holds). **Device ownership**
is already persisted there (`allmystuff-ownership.json`): the recorded owner
survives restarts, while claim mode is deliberately transient (re-asserted
each start by the flag) so a box never sits silently adoptable across reboots.
That same record now also holds the **owned fleet** — the shared key, the
fleet's display name, and the roster of co-owned devices — so a fleet survives
restarts and re-converges via gossip on the next start. Roster convergence is by version with *replacement*
on a strictly newer copy (that's how a **leave or kick** propagates — a union
could only ever add), equal versions union, and a newer roster that no longer
lists this device means it was kicked: the fleet drops locally and ownership
is released. Membership is the permission: the Fleet pane offers **Leave**
(and per-member **Kick**) only while this device is in the roster — you can't
kick devices from a fleet you aren't in. A **claim-status check** (sanitize
stale fleet residue → re-stamp the live profile → re-assert presence + roster)
runs at session start, after every claim/release/fleet change, and *targeted*
at each peer the moment its connection is approved — so two machines agree on
who owns what within a handshake, not a polling interval. Gossip is
**event-driven, with no heartbeat**: presence carries a per-run `boot` id, and
a peer seeing a boot it hasn't recorded (your app restarted while the daemon
link stayed up) answers with its own presence + roster directly — the mesh
carries traffic when something happens, never on a timer. The fleet roster
(it holds the grouping key) is only ever *handed* to fleet members; presence
goes to everyone.

## Next milestones

- **Camera video + storage transport** over the same route pipe that audio,
  screen, and input already use — each needs its capture backend feeding the
  existing offer/accept/media plumbing. (Cameras already show as console
  tabs; selecting one says the transport is coming.)
- **Per-device audio routing** — map a specific scanned device to a `cpal`
  device (audio still uses the default input/output; monitors are routed
  per-screen now), and an audio codec (Opus) so the media channel isn't
  raw PCM.
- **Share-grant-gated control** — input injection currently trusts only the
  device's owner/fleet; honouring a *shared* person's explicit control grant
  rides on the share-enforcement work.
- **Persisted relationships + grants** — remember per peer whether it's
  *mine* or a *guest*, and its grants, across restarts (today a freshly
  discovered peer defaults to "mine" and is reclassified from its drawer).

Deliberately out of scope: embedding `myownmesh-core` at the source level —
AllMyStuff is a control-socket client by design, matching the rest of the
family.
