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
- **Group** — an isolatable bundle of capabilities on one node, fanned out to
  a target as a unit (the RDC kit). `Catalog::connect_group` builds one route
  per member in the direction its flow implies — and aborts the whole connect
  if any leg would breach a share.
- **Relationship** — `Mine` (a device you own or manage) or `Shared { person,
  grants }`. A `Grant` authorizes a shared endpoint to play one role
  (`Provide` = they source, `Consume` = they sink) for one media, optionally
  pinned to one capability. `required_grants` returns the minimal grant that
  would unblock a denied route — the "one-tap allow."
- **Ownership** (in `allmystuff-protocol` + the GUI) — distinct from a
  relationship: a device *advertises* who owns it and whether it's
  *claimable*. You can't flat-take a box — a claim only lands if the device
  was started in **claim mode** (the `ALLMYSTUFF_CLAIMABLE` flag, or its own
  "allow adoption" toggle) and is still unowned. The recorded owner is the
  authenticated claimer the mesh delivered, persisted next to the identity.

See `crates/allmystuff-graph/src/lib.rs` tests for the full behaviour,
including the RDC fan-out and the share-breach abort.

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
  `ControlMessage`s for route setup and share negotiation, and the
  `OwnedRoster` fleet gossip (`CHANNEL_OWNED`). Authorization is
  never on the wire — a node only advertises or accepts what its local
  `Catalog` already permits.
- **The owned fleet** — claiming a device doesn't just flip a flag: the two
  machines start sharing an `OwnedRoster` on `CHANNEL_OWNED` — the set of
  devices one owner has claimed, all linked by a single shared **fleet key**.
  The owner mints the key on its first claim and hands it down on each
  adoption; every co-owned device gossips the roster and converges by version,
  exactly like a mesh roster. For now the key only groups devices internally
  (a later edition links it to other things). It's persisted next to the
  ownership record.

### allmystuff-bridge

The one place hardware vocabulary meets graph vocabulary. Turns an
`Inventory` into graph `Capability`s — physical devices (mic → audio source,
monitor → display sink, …) plus a synthetic per-machine trio (**screen** =
display source, **control** = input sink, **system audio** = duplex) so
whole-computer flows (screen-share, RDC) have something to land on. Shared by
the CLI and the Tauri backend so "what this machine exposes" is computed
once.

## The GUI (`gui/`)

Tauri 2 + Svelte 5, a client of the daemon.

- **Backend** (`src-tauri/`) — `scan_self` (inventory + bridge), the live
  `mesh::Mesh` (subscribes to the presence/control/media channels, drives the
  `allmystuff-session` state machine, emits `allmystuff://session`
  snapshots), the `audio` cpal bridge (capture → mesh → playback for active
  audio routes), `connect_route`/`disconnect_route` commands, and
  `daemon_spawn`. The `myownmesh` daemon ships **bundled with the app**:
  `build.rs` fetches the rev pinned in `.myownmesh-rev` and stages it as a
  Tauri sidecar (`binaries/myownmesh-<triple>`, `externalBin`), so the mesh
  is there for free — `daemon_spawn` resolves the bundled binary and
  auto-spawns it. `update_*` commands drive `allmystuff-updater`.
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
  screen at once; the web preview keeps the in-page popover. The stage is a
  live MJPEG sink — it registers a per-route IPC channel (`video_watch`) and
  the backend pushes each inbound frame as raw bytes (a fixed header + the
  JPEG; no JSON or base64 on the per-frame path) to exactly the window
  that's watching — and while control is on it captures pointer/key events,
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
3. If both ends are **mine**, a `Route` appears immediately.
4. If a **shared** endpoint isn't covered, `requiredGrants` raises the share
   sheet ("Let Alex receive your screen?"). Approving adds exactly that grant
   and completes the connection.
5. With a live daemon, the backend sends a `RouteControl::Offer` to the peer
   over `CHANNEL_CONTROL`. The peer accepts; both sides go `Active`. For an
   audio route, the source captures its mic (`cpal`), streams `AudioFrame`s
   over `CHANNEL_MEDIA`, and the sink plays them. A display route streams
   the source's primary screen from a persistent capture session (`xcap`'s
   recorder: PipeWire ScreenCast / DXGI / AVFoundation, with a paced
   per-frame grab as the X11 path and universal fallback), unchanged frames
   skipped, a bounded queue dropping stale packets under backpressure. The
   *transport* is negotiated per route: when the viewer's offer advertises
   `h264` (WebCodecs present) and the peer's lane is free, frames ride
   **MyOwnMesh's H.264 video track lane** — openh264 in screen-content mode
   at a 1920 edge, real RTP, no JSON/base64/64 KiB ceiling — and otherwise
   fall back to the v1 **MJPEG stream** over `CHANNEL_MEDIA` (1280 edge,
   chunked JPEGs), so any version skew degrades to working video. Either
   way the console window renders packets it *pulls* per display tick
   (raw bytes; WebCodecs decodes H.264, `createImageBitmap` the JPEGs).
   Set `ALLMYSTUFF_VIDEO_STATS=1` to print each stream's per-stage
   pipeline counters (fps, scale/encode ms, bitrate, skip/drop causes)
   every few seconds on both ends — quiet by default. An input
   route carries `InputEvent`s the other direction: normalized mouse moves /
   buttons / wheel / DOM-`key` values, injected at the sink with `enigo` —
   but only after the gate: the route must be live *and* the sender must be
   the device's recorded owner or a co-owned fleet member, so a route that
   merely auto-accepted can never type into your machine.

## Persistent state

AllMyStuff rides on MyOwnMesh's identity + roster (under `~/.myownmesh/`,
overridable via `MYOWNMESH_HOME`). Its own additions — relationships, grants,
groups, and saved routes — are app state layered on top; the mesh provides
the cryptographic identity that those grants attach to. **Device ownership**
is already persisted there (`allmystuff-ownership.json`): the recorded owner
survives restarts, while claim mode is deliberately transient (re-asserted
each start by the flag) so a box never sits silently adoptable across reboots.
That same record now also holds the **owned fleet** — the shared key and the
roster of co-owned devices — so a fleet survives restarts and re-converges via
gossip on the next start. Roster convergence is by version with *replacement*
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
  existing offer/accept/media plumbing.
- **Per-device routing** — map a specific scanned device to a `cpal` device
  or a specific monitor to the screen capture (v1 uses the default
  input/output and the primary display), and an audio codec (Opus) so the
  media channel isn't raw PCM.
- **Share-grant-gated control** — input injection currently trusts only the
  device's owner/fleet; honouring a *shared* person's explicit control grant
  rides on the share-enforcement work.
- **Persisted relationships + grants** — remember per peer whether it's
  *mine* or a *guest*, and its grants, across restarts (today a freshly
  discovered peer defaults to "mine" and is reclassified from its drawer).

Deliberately out of scope: embedding `myownmesh-core` at the source level —
AllMyStuff is a control-socket client by design, matching the rest of the
family.
