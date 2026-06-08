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
└── allmystuff-cli         # `allmystuff scan` / `capabilities`
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
  {source, sink, duplex}.
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
  daemon's typed channels: a `NodeProfile` presence advert, and
  `ControlMessage`s for route setup and share negotiation. Authorization is
  never on the wire — a node only advertises or accepts what its local
  `Catalog` already permits.

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

- **Backend** (`src-tauri/`) — `scan_self` (inventory + bridge), one-shot
  control commands, an event pump that re-emits the daemon's stream as
  `allmystuff://event`, and `daemon_spawn` to launch `myownmesh serve` if one
  isn't already up. Mirrors the MyOwnMesh GUI almost line-for-line.
- **Front-end** (`src/`) — the graph. `catalog.ts` is a faithful TypeScript
  port of the graph crate's rules, so the canvas is fully interactive on
  demo data with no backend; when the backend is present it validates the
  same way in Rust before anything touches the wire. `store.svelte.ts` holds
  the catalog + interaction state as Svelte 5 runes.

## Data flow: connecting a device

1. User taps a capability's connect dot → `store.startCapConnect(capId)`.
2. User taps a target node → `store.connectCapToNode` finds the matching
   endpoint and calls `catalog.proposeRoute`.
3. If both ends are **mine**, a `Route` appears immediately.
4. If a **shared** endpoint isn't covered, `requiredGrants` raises the share
   sheet ("Let Alex receive your screen?"). Approving adds exactly that grant
   and completes the connection.
5. With a live daemon, the backend would send a `RouteControl::Offer` to the
   peer over `CHANNEL_CONTROL`; media transport is the next milestone.

## Persistent state

AllMyStuff rides on MyOwnMesh's identity + roster (under `~/.myownmesh/`,
overridable via `MYOWNMESH_HOME`). Its own additions — relationships, grants,
groups, and saved routes — are app state layered on top; the mesh provides
the cryptographic identity that those grants attach to.

## Out of scope (today)

- Real-time media transport (the actual audio/video/input streaming over the
  established routes) — the model, protocol, and UI are in place; the codecs
  and pipes are the next milestone.
- Full macOS / Windows device enumeration beyond the `sysinfo` basics.
- Embedding `myownmesh-core` at the source level — AllMyStuff is a control-
  socket client by design, matching the rest of the family.
