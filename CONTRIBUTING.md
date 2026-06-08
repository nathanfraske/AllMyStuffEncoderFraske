# Contributing to AllMyStuff

Everything you need to build, run, and hack on AllMyStuff. For *what it is
and why*, see the [README](README.md); for *how it fits together*, see
[ARCHITECTURE.md](ARCHITECTURE.md).

## Prerequisites

- **Rust** (stable) — `rustup` recommended; the toolchain is pinned in
  `rust-toolchain.toml`.
- **Node 22 + pnpm 10** — for the desktop front-end.
- **[`just`](https://github.com/casey/just)** (optional) — the task runner.

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
cd gui
pnpm install
pnpm check          # svelte-check (types)
pnpm build          # vite production build (no webview needed)
pnpm tauri dev      # the full app
pnpm tauri build    # a .deb / .AppImage / .dmg / .msi bundle
```

The front-end builds anywhere. The **Tauri backend links the system
webview and the audio stack**, so building the desktop app needs:

| Platform | Deps |
|---|---|
| Linux | `libgtk-3-dev`, `libwebkit2gtk-4.1-dev`, `libsoup-3.0-dev`, `libasound2-dev` |
| macOS | Xcode command-line tools (WKWebView + CoreAudio are built in) |
| Windows | WebView2 + the MSVC toolchain (both standard) |

The app opens straight into a populated **demo graph** even with no
backend, so the whole experience — clicking nodes, drawing connections, the
share sheet, groups — works offline (`pnpm dev` in a browser too).

### Live mesh (real peers + streaming)

For two machines to see each other and stream, both need a running
`myownmesh serve` daemon, joined to the **same** network (set that up with
[MyOwnMesh](https://github.com/mrjeeves/MyOwnMesh); AllMyStuff uses the
first network it finds). Then:

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
