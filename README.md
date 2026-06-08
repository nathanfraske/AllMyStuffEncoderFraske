<div align="center">

# AllMyStuff

### Map everything you own. Wire it together. Share a piece with a friend — and nothing else.

A friendly desktop app that finds every device on your machines — screens,
mics (even 4-mic arrays), speakers, cameras, keyboards, drives — lays them
out as one graph across a private mesh you own, and lets you connect them
with a tap. Your mic to the studio PC. The living-room TV showing your
laptop. Your whole desk — monitor, keyboard, mouse, mic, speakers — pointed
at a machine in the garage, as one bundle.

Built on [MyOwnMesh](https://github.com/mrjeeves/MyOwnMesh). Pure-Rust core,
Tauri + Svelte app, auto-updating — the same family as
[MyOwnLLM](https://github.com/mrjeeves/MyOwnLLM).

[The idea](#the-idea) · [The graph](#the-graph) · [Yours vs shared](#yours-vs-shared--authorization-not-authentication) · [Architecture](#architecture) · [Run it](#run-it)

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Built on MyOwnMesh](https://img.shields.io/badge/mesh-MyOwnMesh-6c5ce7.svg)](https://github.com/mrjeeves/MyOwnMesh)

</div>

## The idea

Most "connect your devices" tools are built for network engineers. AllMyStuff
is built for everyone else. It does three things:

1. **Finds your stuff.** It scans each machine for *everything plugged in* —
   CPU, GPU, RAM, storage, networks, and the things you actually care about:
   displays, microphones (including beam-forming arrays), speakers, cameras,
   keyboards, mice, and the rest of the USB bus.

2. **Draws it as a graph.** Every machine is a node. Click one and you see
   its stats and its devices, each with a little **connect dot** you drag to
   wire it somewhere else on the mesh.

3. **Keeps it safe without keys or jargon.** The mesh underneath proves *who*
   each device is, cryptographically — you never see a key. AllMyStuff only
   asks the question a human actually has: **is this mine, or am I sharing
   with someone?**

## The graph

The whole app is one canvas. Your machines orbit *this device*; the people
you share with sit on the outside.

- **Click a node** → a drawer with its hardware, its devices grouped by kind,
  and the live connections running through it.
- **Connect a device** → tap its dot, then tap where it should go. AllMyStuff
  picks the matching endpoint (your mic → that PC's audio-in; that PC's
  screen → your monitor) and draws a glowing wire.
- **Bundle a group** → make an *isolatable* set — your monitor + keyboard +
  mouse + mic + speakers — and beam the whole thing at one machine, turning
  your desk into its terminal (the "RDC" move). The bundle connects and
  disconnects as one thing.

Edges are colour-coded by what flows through them — <b>audio</b>,
<b>video</b>, <b>screen</b>, <b>controls</b>, <b>files</b> — so the graph
reads at a glance.

> A rendered mock of the main screen lives at
> [`docs/design/allmystuff-graph.svg`](docs/design/allmystuff-graph.svg).

## Yours vs shared — authorization, not authentication

This is the heart of it. When you add a connection, you pick one of two
things:

- **🧦 A device that's mine** — something you own or manage. It joins your
  fleet and connects freely with everything else you own. No ceremony.
- **🧑‍🤝‍🧑 Someone I'm sharing with** — a friend or family member. *Nothing*
  flows until you allow it, and every grant is scoped: a direction (they send
  / they receive), a kind (just audio, just your screen…), and optionally a
  single device.

Try to wire your screen to a friend before you've allowed it and you don't
get an error — you get a friendly sheet: *"Let Alex receive your screen?"*
Approve it and **only that** becomes reachable. Revoke it later and any
connection that depended on it stops immediately.

The mesh handles identity. You handle permission. That split is the whole
product.

## Architecture

A Cargo workspace of small, focused crates plus a Tauri + Svelte app — the
same shape as MyOwnMesh and MyOwnLLM, and a **client of the `myownmesh`
daemon** rather than an embedder of the engine.

```
allmystuff-inventory   # lib — cross-platform device scan (Linux live; macOS/Windows scaffolded)
allmystuff-graph       # lib — capabilities, routes, groups, and the own-vs-share authorization model
allmystuff-protocol    # lib — mirror of the myownmesh control socket + AllMyStuff's peer messages
allmystuff-bridge      # lib — turns an Inventory into routable graph capabilities (+ presence summary)
allmystuff-cli         # bin — `allmystuff scan` / `capabilities`, headless
gui/                   # app — Tauri 2 backend + Svelte 5 front-end (the graph)
```

- **The library workspace builds and tests with nothing but `cargo`** — no
  webview, no daemon, no network. The graph model and authorization rules are
  pure and heavily tested.
- **The GUI is a sidecar client.** It scans the machine locally and talks to
  a running `myownmesh serve` over the daemon's line-delimited JSON control
  socket — exactly how the MyOwnMesh GUI and MyOwnLLM do it. The mesh version
  it targets is pinned in [`.myownmesh-rev`](.myownmesh-rev).
- **Two sources of truth, one set of rules.** The routing/authorization logic
  is ported to TypeScript so the graph is fully interactive on its own; the
  Rust `Catalog` enforces the identical rules before anything hits the wire.

See [`ARCHITECTURE.md`](ARCHITECTURE.md) for the full tour.

## Run it

### Scan this machine (no GUI, no mesh needed)

```sh
cargo run -p allmystuff-cli -- scan          # pretty inventory
cargo run -p allmystuff-cli -- scan --json   # the same, as JSON
cargo run -p allmystuff-cli -- capabilities  # what it would expose on the mesh
```

Real output from the box this was built on:

```text
┌──────┐
│  vm  │
└──────┘
  linux 24.04  ·  x86_64
  up 25m  ·  5 devices

▓ Compute
    cpu      Intel(R) Xeon(R) Processor @ 2.80GHz (4c / 4t) @ 2.80 GHz
    ram      15.7 GiB (15.1 GiB free)
▓ Storage
    hdd      /dev/vda — 223 GiB of 252 GiB used (88%) /
▓ Network
    eth      eth0 [up] · 192.0.2.2

This machine would expose 4 capabilities on the mesh:
  display screen   → out   Screen  [this:screen]
  input   control  in  →   Keyboard & mouse control  [this:control]
  audio   system   ↔       System audio  [this:system-audio]
  storage storage  ↔       /dev/vda  [this:disk:/]

The graph in action (on this machine's real devices):
  ✓ casting your screen to a friend is blocked until you allow it:
      your friend isn't allowed to receive your display yet
```

### The desktop app

```sh
cd gui
pnpm install
pnpm tauri dev      # needs a running `myownmesh serve` for live mesh data
```

The app opens straight into a populated demo graph even with no daemon, so
you can explore the whole experience offline. The Tauri backend needs the
standard Linux webview deps to build: `libgtk-3-dev` and
`libwebkit2gtk-4.1-dev` (macOS/Windows use the system webview).

### Develop

```sh
just            # fmt-check + clippy + test + GUI typecheck/build
just test       # cargo test --workspace
just scan       # cargo run -p allmystuff-cli -- scan
just gui-check  # svelte-check + vite build
```

## Status

This repository is a **working foundation**, honest about what's real:

| Piece | State |
|---|---|
| Device scanner (Linux) | **Working** — live `/proc` + `/sys` probes, fixture-tested decoders (EDID, ALSA arrays, input devices) |
| Graph model + authorization | **Working** — pure Rust, fully unit-tested; mirrored in TS for the UI |
| Desktop graph UI | **Working** — builds, typechecks, fully interactive on demo data |
| macOS / Windows scanners | **Scaffolded** — host basics via `sysinfo`; richer classes are follow-ups |
| Live mesh routing of real A/V | **Designed** — the protocol + control wiring are in place; media transport is the next milestone |

## Lineage

AllMyStuff is the third app in the family:

- **[MyOwnMesh](https://github.com/mrjeeves/MyOwnMesh)** — the pure-Rust
  peer-to-peer mesh. AllMyStuff sidecars it for identity, discovery, and
  transport.
- **[MyOwnLLM](https://github.com/mrjeeves/MyOwnLLM)** — detect hardware, run
  the best local model. AllMyStuff borrows its hardware-detection and
  sidecar-the-daemon patterns and points them at *every* device, not just the
  GPU.

[LICENSE](LICENSE) — MIT.
