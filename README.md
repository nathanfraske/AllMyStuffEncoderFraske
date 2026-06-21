<div align="center">

<img src="docs/design/logo.png" width="116" alt="AllMyStuff logo" />

# AllMyStuff

### All my stuff. Works. — _yours can too._

One app turns the machines you already own into a single private system:
**see any screen, open any shell, reach any file** — from whatever device is
in your hand. Peer-to-peer, encrypted end to end, free.

<br/>

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Built on MyOwnMesh](https://img.shields.io/badge/mesh-MyOwnMesh-6c5ce7.svg)](https://github.com/mrjeeves/MyOwnMesh)
[![Platforms](https://img.shields.io/badge/platforms-macOS%20·%20Linux%20·%20Windows-informational.svg)](#install)
[![Built with](https://img.shields.io/badge/built%20with-Rust%20·%20Tauri%20·%20Svelte-orange.svg)](#how-it-works)
[![Price](https://img.shields.io/badge/price-%240%20forever-F11EA1.svg)](#the-bigger-picture)
[![Releases](https://img.shields.io/github/v/release/mrjeeves/AllMyStuff?label=release&color=success)](https://github.com/mrjeeves/AllMyStuff/releases)

**[What it is](#what-it-is)** · **[Install](#install)** · **[Features](#what-you-can-do)** · **[Yours vs shared](#yours-vs-shared)** · **[How it works](#how-it-works)** · **[Status](#status)** · **[Build](#build)**

<br/>

<img src="docs/design/allmystuff-graph.png" width="760" alt="The AllMyStuff graph: your machines as nodes, devices wired between them" />

<sub><b>Home is a graph.</b> Tap a device's dot, tap where it goes, and the wire exists.</sub>

</div>

---

## What it is

AllMyStuff scans each of your machines for everything plugged in — screens,
mics, cameras, disks, keyboards — and lays them out as **one graph** across a
private mesh you own. Tap a device, tap where it should go, and the connection
exists. No IPs, no ports, no keys to manage.

Most "connect your devices" tools are built for network engineers. This one is
built for everyone else.

> **Is it a VPN?** Same itch, different animal. A VPN builds a *network* — a
> subnet with IPs and rules. AllMyStuff builds *routes*: your devices carry
> capabilities (a screen, a shell, a folder) and the graph connects only the
> pairs you've granted, only while a route is live.

## Install

One command — detects your platform, verifies SHA-256, and puts `allmystuff`
and the desktop app on your PATH. No account, no card.

```sh
# macOS / Linux
curl -fsSL https://allmystuff.works/install.sh | sh
```

```powershell
# Windows
irm https://allmystuff.works/install.ps1 | iex
```

It opens straight into a populated **demo graph** — no mesh required — so you
can try the whole thing offline before it touches your real machines.

<details>
<summary>The mesh comes along (no second command), and other install notes</summary>

<br/>

Live machines run on a [MyOwnMesh](https://github.com/mrjeeves/MyOwnMesh)
daemon. The installer handles it: a recent enough `myownmesh` (≥ the version in
[`.myownmesh-rev`](.myownmesh-rev)) is used as-is, an older one updates itself,
and a missing one is installed next to the app. The app then keeps it current
at launch. Pass `--no-mesh` / `-NoMesh` to leave the daemon alone.

It installs to `/usr/local/bin` (or `~/.local/bin`) on Unix and
`%LOCALAPPDATA%\Programs\AllMyStuff` on Windows. `--no-gui` / `-NoGui` skips the
desktop app for a headless box. For full OS integration grab the `.deb` /
`.AppImage` / `.dmg` / `.msi` from [Releases](https://github.com/mrjeeves/AllMyStuff/releases),
where portable tarballs (`+ .sha256`) live for the
[same five platforms as MyOwnMesh](https://github.com/mrjeeves/MyOwnMesh#platforms).

</details>

## What you can do

### 👁️ See it — Console

A live console for every machine: its screens, cameras and audio in tabs,
**H.264 up to 4K**. Flip on control and your keyboard and mouse become its own —
smooth enough to drive your tower from a thin laptop.

### ⌨️ Act on it — Shell & Files

A real shell and file browser for any machine you own — **no sshd, no port
forwarding, no cloud drive.** Terminal tabs spawn a real PTY; files preview in
place and downloads stream straight to disk.

### 🤝 Share it — by person, not password

*"Alex can receive my screen"* is the whole grant. It follows Alex to whatever
device they're on, covers exactly one thing, and revokes with a tap. **Nothing
is shareable by default.**

### 🧦 Own it — your fleet, your keys

Claim a machine and it joins your **fleet** under one key you hold and can walk
away with. Your data lives on your devices — there's no server-side copy — and
it all keeps working with the internet down.

> **Rooms** turn any machines you pick into a zoom-like call in its own window —
> scoped to the room, stream-only, nothing stored. Joining wires *nothing*; you
> turn on mic, camera or screen yourself.

## Yours vs shared

The heart of it: **authorization, not authentication.** The mesh proves *who*
each device is (cryptographically — you never see a key). AllMyStuff only asks
the question a human actually has: **is this mine, or am I sharing it?**

So you never hit an error wiring something you haven't allowed — you get a
friendly sheet (*"Let Alex receive your screen?"*). Approve it and **only that**
becomes reachable; revoke it and any connection depending on it stops at once.
Every grant lives under **Settings → Sharing**.

The mesh handles identity. You handle permission. That split is the whole
product.

## How it works

A Cargo workspace of small Rust crates plus a Tauri 2 / Svelte 5 app, riding as
a **client of the `myownmesh` daemon** rather than embedding it.

- **The graph model is pure, heavily-tested Rust**, mirrored to TypeScript so
  the UI is interactive on its own — and the Rust `Catalog` enforces every
  authorization rule before anything hits the wire.
- **One engine, two front ends.** Presence, the route handshake and every media
  plane (screen / camera / audio / input / terminal / files) live in `node/`.
  The desktop app links it; `allmystuff serve` links the same code, headless.
- **The mesh** handles identity, discovery and transport — traffic runs
  directly between your machines, encrypted end to end.

See **[`ARCHITECTURE.md`](ARCHITECTURE.md)** for the full tour and the crate map.

### Headless

A box with no screen still belongs on the graph — a home server, a machine that
hosts a room. `allmystuff serve` runs the node with no window; one service runs
both it and its daemon, and it self-updates unattended.

```sh
allmystuff serve            # this machine on the mesh, headless
allmystuff service install  # …and keep it running across reboots
```

The desktop app drives all of this from **Settings → Always On**.

## Status

A **working foundation**, honest about what's real. Deep per-platform detail
lives in [`ARCHITECTURE.md`](ARCHITECTURE.md).

| Piece | State | |
|---|---|---|
| Device scanner | ✅ | Linux / macOS / Windows, fixture-tested decoders, compiled + tested on all three in CI |
| Graph model + authorization | ✅ | Pure Rust, fully unit-tested; mirrored to TS for the UI |
| Device ownership | ✅ | Every device advertises its owner; you can only adopt a box started in claim mode |
| Desktop graph UI | ✅ | Builds, typechecks, interactive on demo *and* live data |
| Remote console | ✅ | pikvm-style window per machine: live screen, camera tabs, audio, keyboard/mouse |
| Remote terminal | ✅ | A real shell on any of your machines — no sshd; one mesh route per PTY tab |
| Remote files | ✅ | A finder-like manager — no smb/sftp; browse, preview, upload, download, edit |
| Presence + route handshake | ✅ | Peers appear via presence; routes negotiate offer/accept/teardown |
| Live audio | ✅ | Opus over the mesh's RTP lane, PCM fallback; system-audio loopback or mic |
| Live screen + input | ✅ | openh264 → RTP up to 4K/30 fps, MJPEG fallback; chords resolve so modifiers never stick |
| Live camera | ✅ | Same pipe as screens (H.264 + MJPEG fallback) via nokhwa |
| Storage streaming | 🚧 Next | Routes wire and show the session; transport over the proven pipe is in flight |

## Build

```sh
just setup    # one-time deps: Rust, Node, pnpm, GUI libs
just dev      # run with hot reload
```

The full CLI reference and how to help test on macOS / Windows / Pi are in
**[CONTRIBUTING.md](CONTRIBUTING.md)**.

## The bigger picture

AllMyStuff stands on **three pillars** — only one is this repo, and the app
never needs the other two.

| | | |
|---|---|---|
| **01 · Software** | _Free, and missing nothing._ | $0 forever, open source, every feature. **You are here.** |
| **02 · Hardware** | _When the OS is dead, hardware can still answer._ | The [Access line](https://allmystuff.works/hardware/) — an out-of-band witness that keeps a crashed machine on your graph. |
| **03 · Service** | _A human on call. A network of your own._ | Strictly optional: a real technician one tap away, or a private relay of your own. |

The free app is **complete on day one — not a trial, not a tier.** The whole
story is at **[allmystuff.works](https://allmystuff.works)**.

## Family

- **[MyOwnMesh](https://github.com/mrjeeves/MyOwnMesh)** — the pure-Rust
  peer-to-peer mesh AllMyStuff sidecars for identity, discovery and transport.
- **[MyOwnLLM](https://github.com/mrjeeves/MyOwnLLM)** — detect hardware, run
  the best local model. AllMyStuff borrows its sidecar-the-daemon patterns.

---

<div align="center">

**[allmystuff.works](https://allmystuff.works)** · [Releases](https://github.com/mrjeeves/AllMyStuff/releases) · [Architecture](ARCHITECTURE.md) · [Contributing](CONTRIBUTING.md)

Tech that works when you turn it on. **Yours, with or without us.**

[MIT](LICENSE) · A [Critical Error Computing](https://allmystuff.works) product

</div>
