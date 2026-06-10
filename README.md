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

## What a scan sees

Every probe degrades gracefully, so a bare container shows compute +
storage + network while a loaded laptop shows the lot — displays, mic
arrays, cameras, the works. On the box this was built on:

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

## Build & run

The desktop app opens straight into a populated demo graph even with no
mesh, so you can explore the whole experience — clicking nodes, drawing
connections, the share sheet, groups — offline. A bare `allmystuff` opens
it; the subcommands (`scan`, `capabilities`, `update`) are for headless
boxes and scripts.

From a source checkout it's two commands:

```sh
just setup    # one-time: build deps, Rust, Node, pnpm, GUI deps
just dev      # run the app with hot reload
```

The full CLI reference, the desktop-app dependencies, the live-mesh setup,
and **how to help test on macOS / Windows / Pi** all live in
**[CONTRIBUTING.md](CONTRIBUTING.md)**.

## Status

This repository is a **working foundation**, honest about what's real:

| Piece | State |
|---|---|
| Device scanner | **Working** on Linux (`/proc` + `/sys`), macOS (`system_profiler`), and Windows (CIM) — fixture-tested decoders, compiled + tested on all three in CI. Flags each category's **current default** (the mic it captures from, the screen it drives first) |
| Graph model + authorization | **Working** — pure Rust, fully unit-tested; mirrored in TS for the UI. Routing prefers a node's default device when auto-picking an endpoint |
| Device ownership | **Working** — every device advertises its owner and whether it's *claimable*; you can only adopt a box that was started in claim mode, never flat-take one that's already owned |
| Desktop graph UI | **Working** — builds, typechecks, interactive on demo data and live data. Devices on the mesh that aren't running AllMyStuff are shown but quieted and un-targetable |
| Remote console | **Working** — a pikvm-style session in its **own OS window per machine** (open as many as you like): live screen, a video-inputs tab bar, audio passthrough and keyboard/mouse control, wiring the real routes underneath |
| Presence + route handshake | **Working** — peers appear via presence; routes negotiate offer/accept/teardown over the mesh |
| Live audio streaming | **Working** — a mic → speakers route (and the console's audio passthrough) opens a real `cpal` audio stream across the mesh (default devices, mono in v1) |
| Live screen + input streaming | **Working** — a display route streams the remote's screen (the routed monitor: every screen is its own console tab) over MyOwnMesh's **H.264 video track lane** (openh264 screen-content encode at a 1920 edge → RTP, ~30 fps) negotiated per route, with the v1 MJPEG-over-channel stream as the automatic fallback for older peers. Decode is covered on every platform: WebCodecs where the webview has it, the backend's **native openh264 decoder** (ready-to-paint RGBA over IPC) where it doesn't or when WebCodecs stalls. Capture is a persistent `xcap` session (PipeWire / DXGI / AVFoundation, paced grabs on X11), unchanged frames skipped, drop-on-backpressure. A control route forwards normalized keyboard/mouse events (sourced from the synthetic per-machine **keyboard & mouse**, so any platform can drive any other) injected with `enigo`, gated to the device's owner/fleet. Default audio devices in v1 |
| Camera video / storage streaming | **Next** — the routes wire and show the session; these media still need their capture backends over the proven pipe |

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
