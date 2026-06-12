<div align="center">

# AllMyStuff

### Map everything you own. Wire it together. Share a piece with a friend — and nothing else.

A friendly desktop app that finds every device on your machines — screens,
mics (even 4-mic arrays), speakers, cameras, keyboards, drives — lays them
out as one graph across a private mesh you own, and lets you connect them
with a tap. Your mic to the studio PC. The living-room TV showing your
laptop. A **virtual room** holding the kitchen, the office and the garage
in one call — screens, sound and files shared only when you switch them on.

Built on [MyOwnMesh](https://github.com/mrjeeves/MyOwnMesh). Pure-Rust core,
Tauri + Svelte app, auto-updating — the same family as
[MyOwnLLM](https://github.com/mrjeeves/MyOwnLLM).

[The idea](#the-idea) · [Install](#install) · [The graph](#the-graph) · [Yours vs shared](#yours-vs-shared--authorization-not-authentication) · [Architecture](#architecture) · [Run it](#build--run) · [Releases](https://github.com/mrjeeves/AllMyStuff/releases)

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

## Install

One command — detects platform, fetches the binaries from
[GitHub Releases](https://github.com/mrjeeves/AllMyStuff/releases),
verifies SHA-256, drops `allmystuff` **and** the `allmystuff-gui`
desktop app on your PATH (so a bare `allmystuff` opens the app), and
brings the mesh along with it.

```sh
# macOS / Linux
curl -fsSL https://raw.githubusercontent.com/mrjeeves/AllMyStuff/main/scripts/install.sh | sh
```

```powershell
# Windows
irm https://raw.githubusercontent.com/mrjeeves/AllMyStuff/main/scripts/install.ps1 | iex
```

**The mesh comes along.** Live machines run on a
[MyOwnMesh](https://github.com/mrjeeves/MyOwnMesh) daemon, and the
installer sorts that out too — there is no second install command:

- a `myownmesh` that's already installed and recent enough (at or
  above the version pinned in [`.myownmesh-rev`](.myownmesh-rev)) is
  **used as-is**;
- an older one is **asked to update itself** (`myownmesh update`);
- none at all → the daemon is **installed next to the app**,
  downloaded and SHA-256-verified from MyOwnMesh's releases.

The app starts and manages the daemon by itself — and from then on
keeps it current: an installed daemon that's fallen behind the app's
pin is asked to update itself at launch, before it's started, so a
stale mesh never quietly costs you features (a too-old daemon lacks
the media lanes that screens and audio ride). The app opens into a
populated demo graph even with no mesh at all, so a failed or skipped
daemon never blocks you from exploring. Pass `--no-mesh` (Unix) /
`-NoMesh` (Windows) to leave the daemon alone.

The installer writes to `/usr/local/bin` (or `~/.local/bin` if not
writable) on Unix and `%LOCALAPPDATA%\Programs\AllMyStuff` on
Windows, and adds the directory to PATH if it isn't already there.
The desktop app goes in by default; pass `--no-gui` (Unix) or
`-NoGui` (Windows) for a CLI-only install on a headless box — that
skips the daemon too, since only the desktop app uses it. The GUI
binary relies on the system webview (libwebkit2gtk / WebView2 /
WKWebView); for full OS integration (menu entry, icon, the mesh
daemon bundled inside the package) grab the `.deb` / `.AppImage` /
`.dmg` / `.msi` bundle from Releases instead.

Prefer a tarball directly? The portable binaries
(`allmystuff-<platform>.{tar.gz,zip}` + `.sha256` sidecar) are on
[Releases](https://github.com/mrjeeves/AllMyStuff/releases) for the
[same five platforms as MyOwnMesh](https://github.com/mrjeeves/MyOwnMesh#platforms).

## The graph

The whole app is one canvas, with two ways to read it: the radial view
(your machines orbit *this device*, fleets seated together) and a **grouped
grid** — one labelled band per fleet (yours, each person's, and an
"unknown fleet" for devices that advertise no owner) — switched from the
zoom controls.

- **Click a node** → a drawer with its hardware, the session buttons
  (**Remote Control · Open Files · Open Terminal**), the live connections
  running through it, and its devices folded under a count.
- **Connect a device** → tap its dot, then tap where it should go. AllMyStuff
  picks the matching endpoint (your mic → that PC's audio-in; that PC's
  screen → your monitor), draws a glowing wire — and pops the console that
  manages that kind of session on the far machine.
- **Make a room** → a zoom-like call between machines you pick — or just
  this one, inviting others later: **you host the rooms you make** (their
  identity, roster and name are yours; closing one ends it for everyone),
  named after your fleet's owner by default. Joining wires *nothing*: your
  **mic** (the call — your voice), camera and screen share all start off,
  and the panel adds **share sound** (what the machine is *playing* —
  deliberately never your mic), **share control** (let members drive this
  machine), chat, and file sending. Members' screen shares show up as live
  tiles — and it's a *real* room: sharing there is scoped to the room,
  stream-only (nothing is stored), and never hands its members standing
  permissions. Be in as many rooms at once as you like; each room's
  toggles are its own.

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

A share is with the **person, not one machine**: what you allow Alex works
to whichever of Alex's devices is handy (their fleet shares one owner), and
a device of theirs that appears later joins the same connection. Everything
you're sharing — every person, every grant — lives under **Settings →
Sharing**, where any single grant, or the whole connection, can be taken
back.

The mesh handles identity. You handle permission. That split is the whole
product.

## Architecture

A Cargo workspace of small, focused crates plus a Tauri + Svelte app — the
same shape as MyOwnMesh and MyOwnLLM, and a **client of the `myownmesh`
daemon** rather than an embedder of the engine.

```
allmystuff-inventory   # lib — cross-platform device scan (Linux live; macOS/Windows scaffolded)
allmystuff-graph       # lib — capabilities, routes, and the own-vs-share authorization model
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
connections, the share sheet, rooms — offline. A bare `allmystuff` opens
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
| Remote console | **Working** — a pikvm-style session in its **own OS window per machine** (open as many as you like): live screen, a video-inputs tab bar, audio passthrough and keyboard/mouse control — both up from the moment the console opens, with the toggles as off-switches — wiring the real routes underneath |
| Remote terminal | **Working** — a real shell on any of your machines, **no sshd anywhere**: the node drawer's **Open Terminal** opens a tabbed xterm.js window per machine; each tab is its own mesh route to a PTY the far side spawns (`portable-pty`: openpty on Linux/macOS, ConPTY on Windows — same behavior on every OS). Gated to the device's **owner/fleet** exactly like keyboard injection, enforced host-side; advertised via presence `features`, so older peers simply never see the button |
| Remote files | **Working** — a finder-like file manager on any of your machines, **no smb/sftp anywhere**: the node drawer's **Open Files** (between Remote Control and Open Terminal) opens a window per machine — browse, preview text/images in place, upload, download straight into this machine's Downloads, rename, delete, new folder. One mesh route per window, request/response frames over the same media channel; gated to the device's **owner/fleet** exactly like the terminal, enforced host-side, advertised via presence `features` |
| Presence + route handshake | **Working** — peers appear via presence; routes negotiate offer/accept/teardown over the mesh |
| Live audio streaming | **Working** — an audio route streams what its source capability names: **system audio** captures the machine's own playback (WASAPI loopback on Windows, the pulse server's monitor source on Linux — PulseAudio or PipeWire; macOS degrades to the default input until a virtual-device path lands), a mic capability captures the default input. The transport is negotiated per route like video's: **Opus over MyOwnMesh's RTP audio track lane** (48 kHz, 20 ms frames, ~96 kbps) when both daemons speak it, with the v1 PCM-frames-over-channel stream as the automatic fallback for older peers. The console's audio passthrough is **listen-only by design**: the remote's system audio plays on your speakers and nothing flows back — injected audio would ride the remote's loopback straight back as an echo (echo cancellation is the follow-up; wiring a mic stays a deliberate act on the graph). Default devices, mono in v1 |
| Live screen + input streaming | **Working** — a display route streams the remote's screen (the routed monitor: every screen is its own console tab) over MyOwnMesh's **H.264 video track lane** (openh264 screen-content encode at a 1920 edge → RTP, ~30 fps) negotiated per route, with the v1 MJPEG-over-channel stream as the automatic fallback for older peers. Decode is covered on every platform: WebCodecs where the webview has it, the backend's **native openh264 decoder** (ready-to-paint RGBA over IPC) where it doesn't or when WebCodecs stalls. Capture is a persistent session (in-house DXGI on Windows, the **ScreenCast portal with restore tokens** on Wayland — consent is a once-per-machine dialog, every start after it is silent and unattended — AVFoundation on macOS, paced grabs on X11), unchanged frames skipped, drop-on-backpressure. **Hosting a stream forces a sleeping display back on and holds it awake** for the session — the documented per-OS force-on calls (`SC_MONITORPOWER` + one-shot `ES_DISPLAY_REQUIRED` on Windows, `IOPMAssertionDeclareUserActivity` on macOS, DPMS `ForceLevel(On)` + screensaver `SimulateUserActivity` on Linux) pulsed for as long as the stream is dark, a synthetic F15 tap that survives the input filtering a mouse wiggle doesn't, and the keep-awake inhibitors for the session's lifetime; the viewer clicking at a dark console fires the same wake, so driving it works like a remote login. The host's capture state reaches the viewer **in-band** — "waiting for consent", "display asleep", "no monitor" — so a black stage explains itself. (A *locked* Windows console can't be relit from a user-session app — that takes a system service, a planned follow-up.) Device changes under a running app (a monitor waking, detaching, plugging in) re-broadcast presence within ~10 s. A control route forwards normalized keyboard/mouse events (sourced from the synthetic per-machine **keyboard & mouse**, so any platform can drive any other) injected with `enigo`, gated to the device's owner/fleet. Default audio devices in v1 |
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
