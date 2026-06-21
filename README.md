<div align="center">

<img src="docs/design/logo.png" width="116" alt="AllMyStuff logo" />

# AllMyStuff

### All my stuff. Works. — _yours can too._

One app turns the gear you already own into one quiet system: **see any
screen, open any shell, reach any file**, from whichever device is in your
hand. Peer-to-peer between machines you own. Encrypted end to end. And when
something is truly stuck, a real human is one tap away.

<br/>

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Built on MyOwnMesh](https://img.shields.io/badge/mesh-MyOwnMesh-6c5ce7.svg)](https://github.com/mrjeeves/MyOwnMesh)
[![Platforms](https://img.shields.io/badge/platforms-macOS%20·%20Linux%20·%20Windows-informational.svg)](#install)
[![Built with](https://img.shields.io/badge/built%20with-Rust%20·%20Tauri%20·%20Svelte-orange.svg)](#architecture)
[![Price](https://img.shields.io/badge/price-%240%20forever-F11EA1.svg)](#the-bigger-picture)
[![Releases](https://img.shields.io/github/v/release/mrjeeves/AllMyStuff?label=release&color=success)](https://github.com/mrjeeves/AllMyStuff/releases)

**[The idea](#the-idea)** · **[Install](#install)** · **[See it. Drive it. Share it. Own it.](#see-it-drive-it-share-it-own-it)** · **[Yours vs shared](#yours-vs-shared)** · **[Headless](#headless-serve--service)** · **[Architecture](#architecture)** · **[Status](#status)** · **[Build](#build--run)**

<br/>

<img src="docs/design/allmystuff-graph.png" width="760" alt="The AllMyStuff graph: your machines as nodes, devices wired between them" />

<sub><b>Home is a graph.</b> Every machine is a node; tap a device's dot, tap where it should go, and the wire exists.</sub>

</div>

---

## The idea

Most "connect your devices" tools are built for network engineers.
AllMyStuff is built for everyone else. It does three things:

1. **Finds your stuff.** It scans each machine for *everything plugged in* —
   CPU, GPU, RAM, storage, networks, and the things you actually care about:
   displays, microphones (including beam-forming arrays), speakers, cameras,
   keyboards, mice, and the rest of the USB bus.

2. **Draws it as a graph.** Every machine is a node. Click one and you see its
   stats and its devices, each with a little **connect dot** you drag to wire
   it somewhere else on the mesh.

3. **Keeps it safe without keys or jargon.** The mesh underneath proves *who*
   each device is, cryptographically — you never see a key. AllMyStuff only
   asks the question a human actually has: **is this mine, or am I sharing
   with someone?**

> **Is this a VPN? A Tailscale?** Same itch, different animal. A VPN answers
> *"how do my machines reach each other?"* by building a network — a subnet
> with IPs, ports, and rules to manage. AllMyStuff answers it with **routes**:
> your devices carry capabilities (a screen, a shell, a folder), and the graph
> connects exactly the pairs you've granted, for exactly as long as a route is
> live.

## Install

One command — detects your platform, fetches the binaries from
[GitHub Releases](https://github.com/mrjeeves/AllMyStuff/releases),
verifies SHA-256, drops `allmystuff` **and** the `allmystuff-gui`
desktop app on your PATH (so a bare `allmystuff` opens the app), and
brings the mesh along with it.

```sh
# macOS / Linux
curl -fsSL https://allmystuff.works/install.sh | sh
```

```powershell
# Windows
irm https://allmystuff.works/install.ps1 | iex
```

No account. No card. Not even at install. The app opens straight into a
populated **demo graph** even with no mesh at all — so you can click around the
whole experience offline before anything touches your real machines.

<details>
<summary><b>What the installer actually does</b> (the mesh comes along, no second command)</summary>

<br/>

Live machines run on a [MyOwnMesh](https://github.com/mrjeeves/MyOwnMesh)
daemon, and the installer sorts that out too — there is no second install
command:

- a `myownmesh` that's already installed and recent enough (at or above the
  version pinned in [`.myownmesh-rev`](.myownmesh-rev)) is **used as-is**;
- an older one is **asked to update itself** (`myownmesh update`);
- none at all → the daemon is **installed next to the app**, downloaded and
  SHA-256-verified from MyOwnMesh's releases.

The app starts and manages the daemon by itself, and keeps it current at
launch, so a stale mesh never quietly costs you features. Pass `--no-mesh`
(Unix) / `-NoMesh` (Windows) to leave the daemon alone.

The installer writes to `/usr/local/bin` (or `~/.local/bin` if not writable)
on Unix and `%LOCALAPPDATA%\Programs\AllMyStuff` on Windows, adding the
directory to PATH if needed. Every install also drops `allmystuff-serve` —
the **headless node** that `allmystuff serve` runs (see
[Headless](#headless-serve--service)). Pass `--no-gui` (Unix) / `-NoGui`
(Windows) for a headless box — that skips only the webview app. For full OS
integration (menu entry, icon, the daemon bundled inside the package) grab the
`.deb` / `.AppImage` / `.dmg` / `.msi` bundle from
[Releases](https://github.com/mrjeeves/AllMyStuff/releases) instead. Prefer a
tarball? Portable binaries (`allmystuff-<platform>.{tar.gz,zip}` + `.sha256`)
are on Releases for the
[same five platforms as MyOwnMesh](https://github.com/mrjeeves/MyOwnMesh#platforms).

</details>

## See it. Drive it. Share it. Own it.

The whole app is one canvas. Tap a node for a drawer of its hardware, its live
connections, and its devices. Tap a device's dot, then tap where it should go,
and AllMyStuff picks the matching endpoint, draws a glowing wire, and pops the
console that manages that session. Edges are colour-coded by what flows through
them — <b>audio</b>, <b>video</b>, <b>screen</b>, <b>controls</b>, <b>files</b> —
so the graph reads at a glance.

Four moves cover everything:

### 👁️ Console — _you can always see it._

Every machine gets a console — its screens, its cameras, its audio, in tabs.
**H.264 at native resolution up to 4K** when the path is fast, a stubborn MJPEG
floor when it isn't. Turn on control and your keyboard and mouse *are* its
keyboard and mouse. One tab per screen; multi-monitor machines get a tab each,
and every tab pops out into its own OS window or goes fullscreen. Smooth enough
to drive your tower from a thin laptop. Only the recorded **owner** can type.

### ⌨️ Shell & Files — _you can always act._

A real shell and a real file browser for every machine you own — **no sshd to
configure, no port forwarding, no cloud drive in the middle.** Open a terminal
tab and the far side spawns your actual shell in a real PTY (`openpty` on
Linux/macOS, ConPTY on Windows). Browse, preview, rename, fetch — downloads
stream straight to disk, never through a server. Flow control end-to-end,
exactly like ssh — because it behaves like ssh. Gated to the device's owner; a
guest can't even ask.

### 🤝 Sharing — _you choose who else can._

Sharing is a grant to a **person, not a password to a machine.** *"Alex can
receive my screen"* — that's the whole grant. It follows Alex to whichever of
their devices is handy, it covers exactly one thing, and it revokes with one
tap. A blocked connection tells you the **one grant** that would unblock it. New
devices verify with a code both sides can read aloud. **Nothing is shareable by
default. Nothing.**

### 🧦 Fleet — _it stays yours._

Claim a device and it joins your **fleet** — a set of machines that trust each
other under one key you mint, hold, and can walk away with. A box can't be
taken: claiming only works when the device itself was put in claim mode. Your
data lives on your devices — **there is no server-side "your stuff."** Internet
down? Everything inside the walls keeps working. MIT-licensed, self-hostable
end to end — even the relay servers.

> **Rooms.** Want a zoom-like call between machines you pick? Make a **room** —
> you host the ones you make, open or invite-only, each opening in its own
> full-screenable window. Joining wires *nothing*: mic, camera and screen share
> all start off, sharing is scoped to the room and stream-only (nothing is
> stored), and it never hands members standing permissions. Be in as many at
> once as you like.

## Yours vs shared

This is the heart of it — **authorization, not authentication.** When you add a
connection, you pick one of two things:

| | |
|---|---|
| 🧦 **A device that's mine** | Something you own or manage. It joins your fleet and connects freely with everything else you own. No ceremony. |
| 🧑‍🤝‍🧑 **Someone I'm sharing with** | A friend or family member. *Nothing* flows until you allow it, and every grant is scoped: a direction (they send / they receive), a kind (just audio, just your screen…), and optionally a single device. |

Try to wire your screen to a friend before you've allowed it and you don't get
an error — you get a friendly sheet: *"Let Alex receive your screen?"* Approve
it and **only that** becomes reachable. Revoke it later and any connection that
depended on it stops immediately.

A share is with the **person, not one machine**: what you allow Alex works to
whichever of Alex's devices is handy, and a device of theirs that appears later
joins the same connection. Everything you're sharing lives under **Settings →
Sharing**, where any single grant — or the whole connection — can be taken
back.

> The mesh handles identity. You handle permission. **That split is the whole
> product.**

## Headless: serve & service

A box with no screen is still worth putting on the mesh — a home server whose
disk you want to reach, a desktop you'll remote into, a machine that just hosts
a room's screen share. `allmystuff serve` runs the node with **no GUI**: it
shows up on the graph, advertises this machine's capabilities, and serves them
to peers. Same engine the desktop app links; the only thing missing is the
window.

```sh
allmystuff serve                         # run this machine on the mesh, headless
ALLMYSTUFF_CLAIMABLE=1 allmystuff serve  # …and let one of your machines adopt it
```

To keep it running across logout and reboot, install it as an OS service
(systemd / launchd / Windows SCM):

```sh
allmystuff service install                  # install + start; runs at login
sudo allmystuff service --system install    # …or at boot, system-wide
allmystuff service status                   # installed / enabled / running
allmystuff service stop | restart | uninstall
```

Because the node supervises the daemon, **one service runs both** — no second
unit to install. A service box may run for months without a restart, so the
headless node **self-updates unattended**: it applies a permitted release and
relaunches straight onto it. The desktop app exposes all of this under
**Settings → Always On** (start-with-computer, start-minimized, one-click
install-as-service, close/minimize-to-tray), handling the Windows
administrator elevation for you.

## Architecture

A Cargo workspace of small, focused crates plus a Tauri + Svelte app — the same
shape as MyOwnMesh and MyOwnLLM, and a **client of the `myownmesh` daemon**
rather than an embedder of the engine.

```
allmystuff-inventory   # lib — cross-platform device scan (Linux/macOS/Windows)
allmystuff-graph       # lib — capabilities, routes, and the own-vs-share authorization model
allmystuff-protocol    # lib — mirror of the myownmesh control socket + AllMyStuff's peer messages
allmystuff-bridge      # lib — turns an Inventory into routable graph capabilities (+ presence)
allmystuff-session     # lib — live presence + the route offer/accept handshake + media frame types
allmystuff-updater     # lib — self-update: release feed, SHA-256 verify, stage-then-apply
allmystuff-cli         # bin — `allmystuff` scan / capabilities / update / serve / service, headless
node/                  # lib + bin — the mesh node engine + `allmystuff-serve` (headless, no webview)
gui/                   # app — Tauri 2 backend + Svelte 5 front-end (the graph)
```

- **The library workspace builds and tests with nothing but `cargo`** — no
  webview, no daemon, no network. The graph model and authorization rules are
  pure and heavily tested.
- **One engine, two front ends.** The whole node — presence, the route
  handshake, and every media plane (screen / camera / audio / input / terminal
  / files / clipboard) — lives in `node/`. The GUI links it and feeds events to
  its webview; `allmystuff-serve` links the same code and runs it headless.
  Either way it's a **sidecar client** of a running `myownmesh serve`, talked
  to over the daemon's line-delimited JSON control socket.
- **Two sources of truth, one set of rules.** The routing/authorization logic
  is ported to TypeScript so the graph is fully interactive on its own; the
  Rust `Catalog` enforces the identical rules before anything hits the wire.

See **[`ARCHITECTURE.md`](ARCHITECTURE.md)** for the full tour.

## Status

This repository is a **working foundation**, honest about what's real. The
deep per-platform detail behind each row lives in
[`ARCHITECTURE.md`](ARCHITECTURE.md).

| Piece | State | |
|---|---|---|
| **Device scanner** | ✅ Working | Linux (`/proc`+`/sys`), macOS (`system_profiler`), Windows (CIM) — fixture-tested decoders, compiled + tested on all three in CI; flags each category's current default |
| **Graph model + authorization** | ✅ Working | Pure Rust, fully unit-tested; mirrored in TS for the UI |
| **Device ownership** | ✅ Working | Every device advertises its owner and whether it's *claimable*; you can only adopt a box started in claim mode |
| **Desktop graph UI** | ✅ Working | Builds, typechecks, interactive on demo *and* live data |
| **Remote console** | ✅ Working | A pikvm-style session in its own OS window per machine: live screen, video-inputs tab bar, audio passthrough, keyboard/mouse — every tab pops out, hover fullscreen |
| **Remote terminal** | ✅ Working | A real shell on any of your machines, **no sshd anywhere** — tabbed xterm.js, one mesh route per tab to a PTY the far side spawns |
| **Remote files** | ✅ Working | A finder-like manager, **no smb/sftp anywhere** — browse, preview, upload, download to Downloads, rename, delete, new folder |
| **Presence + route handshake** | ✅ Working | Peers appear via presence; routes negotiate offer/accept/teardown over the mesh |
| **Live audio streaming** | ✅ Working | Opus over MyOwnMesh's RTP audio lane (48 kHz, ~96 kbps), PCM fallback for older peers; system-audio loopback or mic capture |
| **Live screen + input** | ✅ Working | openh264 screen-content encode → RTP up to 4K/30 fps, MJPEG fallback; persistent capture per OS; hosting forces a sleeping display awake; chords resolve by physical key so modifiers never stick |
| **Live camera streaming** | ✅ Working | Same negotiated pipe as screens (H.264 + MJPEG fallback) via nokhwa; wires from the graph, tiles a room call |
| **Storage streaming** | 🚧 Next | The routes wire and show the session; the one scanned media still needing its transport over the proven pipe |

## Build & run

The desktop app opens straight into a populated **demo graph** even with no
mesh, so you can explore the whole experience — clicking nodes, drawing
connections, the share sheet, rooms — offline. A bare `allmystuff` opens it;
the subcommands (`scan`, `capabilities`, `update`, `serve`, `service`) are for
headless boxes and scripts.

From a source checkout it's two commands:

```sh
just setup    # one-time: build deps, Rust, Node, pnpm, GUI deps
just dev      # run the app with hot reload
```

The headless node is its own workspace under `node/`:

```sh
cargo build --release --manifest-path node/Cargo.toml   # builds allmystuff-serve
```

The full CLI reference, the desktop-app dependencies, the live-mesh setup, and
**how to help test on macOS / Windows / Pi** all live in
**[CONTRIBUTING.md](CONTRIBUTING.md)**.

<details>
<summary><b>What a scan sees</b> (every probe degrades gracefully)</summary>

<br/>

A bare container shows compute + storage + network; a loaded laptop shows the
lot — displays, mic arrays, cameras, the works.

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

This machine would expose 7 capabilities on the mesh:
  display screen      → out   Screen  [this:screen]
  input   control     in  →   Keyboard & mouse control  [this:control]
  input   controller  → out   Keyboard & mouse  [this:keyboard-mouse]
  audio   system      ↔       System audio  [this:system-audio]
  clipboard clipboard ↔       Clipboard  [this:clipboard]
  video   viewer      in  →   Video in  [this:video-in]
  storage storage     ↔       /dev/vda  [this:disk:/]

The graph in action (on this machine's real devices):
  ✓ casting your screen to a friend is blocked until you allow it:
      your friend isn't allowed to receive your display yet
```

</details>

## The bigger picture

AllMyStuff sits on **three pillars** — only one of them is this repo, and the
app never needs the other two.

| | | |
|---|---|---|
| **01 · The software** | **Free, and missing nothing.** | $0 forever — open source, every feature, peer-to-peer, encrypted end to end. *(You are here.)* |
| **02 · The hardware** | **When the OS is dead, software can't save it. Hardware can.** | The [Access line](https://allmystuff.works/hardware/) — an out-of-band witness that watches a machine's video and presses its buttons, so it stays on your graph even with its OS gone. |
| **03 · The service** | **A human on call. A network of your own.** | Strictly optional. A real technician one tap away (your yes first, logged, revocable), or a private relay venue of your own. |

The free app is **complete on day one — not a trial, not a tier.** Where the
software ends, the other pillars start. Read the whole story at
**[allmystuff.works](https://allmystuff.works)**.

## Family

AllMyStuff is the third app in the family — it borrows their patterns and points
them at *every* device, not just the GPU.

- **[MyOwnMesh](https://github.com/mrjeeves/MyOwnMesh)** — the pure-Rust
  peer-to-peer mesh. AllMyStuff sidecars it for identity, discovery, and
  transport.
- **[MyOwnLLM](https://github.com/mrjeeves/MyOwnLLM)** — detect hardware, run
  the best local model. AllMyStuff borrows its hardware-detection and
  sidecar-the-daemon patterns.

---

<div align="center">

**[allmystuff.works](https://allmystuff.works)** · [Releases](https://github.com/mrjeeves/AllMyStuff/releases) · [Architecture](ARCHITECTURE.md) · [Contributing](CONTRIBUTING.md)

Tech that works when you turn it on. **Yours, with or without us.**

[LICENSE](LICENSE) — MIT · A [Critical Error Computing](https://allmystuff.works) product

</div>
