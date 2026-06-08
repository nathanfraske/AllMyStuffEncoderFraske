# Contributing to AllMyStuff

Everything you need to build, run, and hack on AllMyStuff. For *what it is
and why*, see the [README](README.md); for *how it fits together*, see
[ARCHITECTURE.md](ARCHITECTURE.md).

## Quick start

```sh
just setup     # one-time: WebKit/GTK/ALSA libs, Rust, Node, pnpm, GUI deps
just dev       # run the desktop app with hot reload
```

`just dev` opens the app on a populated demo graph, so you get the full UI
with no mesh running. For live peers + audio streaming, also run
`just mesh-install` once (see [Live mesh](#live-mesh-real-peers--streaming)).

Install [`just`](https://github.com/casey/just) first (`cargo install just`,
`brew install just`, `winget install Casey.Just`, or your package manager).
Everything below also runs as plain `cargo` / `pnpm` if you'd rather skip it.

## Prerequisites

`just setup` installs these for you (via `scripts/bootstrap.{sh,ps1}`):

- **Rust** (stable) — toolchain pinned in `rust-toolchain.toml`.
- **Node 22 + pnpm 10** — for the desktop front-end.
- **Tauri + audio libs** — WebKitGTK/GTK + ALSA on Linux; Xcode CLT on
  macOS; WebView2 + MSVC Build Tools on Windows.

## The library workspace

The crates under `crates/` build and test with nothing but `cargo` — no
webview, no daemon, no network:

```sh
just            # fmt-check + clippy + test + GUI typecheck/build
just test       # cargo test --workspace
just build      # cargo build --workspace
just lint       # cargo clippy --workspace --all-targets -- -D warnings
just fmt        # cargo fmt --all
```

## The CLI

```sh
allmystuff                 # open the desktop app (allmystuff-gui)
allmystuff scan            # pretty inventory of this machine
allmystuff scan --json     # the same, as JSON
allmystuff capabilities    # what this machine would expose on the mesh
allmystuff update          # update to the latest release
allmystuff update status   # version, channel, policy, feed, staged update
```

From a source checkout, `cargo run -p allmystuff-cli -- <cmd>`. A real
inventory of the machine this was built on:

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
```

## The desktop app

```sh
just dev         # hot-reload dev (pnpm install + pnpm tauri dev)
just gui-build   # build a .deb / .AppImage / .dmg / .msi bundle
just gui-check   # svelte-check + vite build (front-end only, no webview)
```

The front-end builds anywhere; the **Tauri backend links the system webview
and the audio stack** — `just setup` installs those deps:

| Platform | Deps (installed by `just setup`) |
|---|---|
| Linux | `libgtk-3-dev`, `libwebkit2gtk-4.1-dev`, `libsoup-3.0-dev`, `libasound2-dev` |
| macOS | Xcode command-line tools (WKWebView + CoreAudio are built in) |
| Windows | WebView2 + the MSVC toolchain |

The app opens straight into a populated **demo graph** even with no mesh, so
the whole experience — clicking nodes, drawing connections, the share sheet,
groups — works offline (a plain `cd gui && pnpm dev` in a browser too).

### Live mesh (real peers + streaming)

The mesh daemon ships **with the app** — there's nothing to install. The
GUI's `build.rs` bundles `myownmesh` as a Tauri sidecar
(`binaries/myownmesh-<triple>`, listed in `tauri.conf.json` →
`externalBin`), fetching the prebuilt binary for the rev pinned in
`.myownmesh-rev` from MyOwnMesh's Releases (with a `cargo install` fallback)
on the first build. The GUI auto-spawns it at runtime.

So `just dev` gives you live mesh out of the box. Override the bundled
daemon during development by pointing `MYOWNMESH_BIN` at your own build, or
keep a sibling `../MyOwnMesh` checkout built (`cargo build -p myownmesh`) —
both `build.rs` and the runtime prefer those. Set `ALLMYSTUFF_SKIP_SIDECAR=1`
to skip the fetch (offline builds; the runtime then falls back to a
sibling/PATH daemon).

For two machines to see each other, both need the daemon joined to the
**same** network (AllMyStuff uses the first network it finds). Then:

- presence makes each machine appear on the other's graph,
- connecting an audio capability (e.g. a mic → another machine's speakers)
  negotiates a route and starts a live `cpal` audio stream.

v1 of the media plane uses the **default** input/output device and
transports mono; mapping a specific scanned device to a `cpal` device, and
video/screen/input transport, are the next milestones.

## Help us test on your hardware

The maintainers develop on Linux, and CI compiles + unit-tests the
macOS/Windows probes — but **runtime output is best confirmed on real
machines**. If you have a Mac, a Windows PC, or a Raspberry Pi, the most
useful thing you can do is:

```sh
allmystuff scan            # does it find your displays/mics/cameras/etc.?
allmystuff capabilities    # are the capabilities sensible?
```

…and open an issue with the output (redact anything private). Mic arrays
(4+ channels), external monitors, and unusual audio setups are especially
valuable.

## CI

Every PR runs (`.github/workflows/ci.yml`):

- **Rust** on Linux + macOS + Windows — fmt, clippy `-D warnings`, the full
  test suite (so the platform scanners + updater are verified where they
  run), and a `capabilities` smoke test.
- **GUI front-end** — `svelte-check` + `vite build`.
- **GUI backend** on Linux + macOS + Windows — `cargo check` of the Tauri +
  `cpal` backend (the streaming code the dev container can't link).

## Conventions

- `cargo fmt` clean, `clippy -D warnings` clean — CI enforces both.
- **Pure parsers, fixture tests.** Anything that decodes a system format
  (EDID, ALSA streams, `/proc` files, `system_profiler`/CIM JSON) is a pure
  function with a unit test, so correctness doesn't depend on the hardware
  being present.
- The graph model + authorization rules are mirrored in Rust
  (`allmystuff-graph`) and TypeScript (`gui/src/catalog.ts`) — change both
  together.
- Match the surrounding comment density and naming.
