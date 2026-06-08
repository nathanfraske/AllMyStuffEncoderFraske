# AllMyStuff — one-command operations.
# Install `just` (https://just.systems) then run `just setup` to get going.
#
# `set shell` is used on Linux/macOS. On Windows the global
# `windows-shell` override routes recipes through PowerShell so they find
# `pnpm.cmd` / `node.exe` via the Windows PATH. Recipes with bash-specific
# syntax need a `[windows]` variant; recipes that just call cross-platform
# tools (cargo, pnpm, git) work in both shells unmodified.
set shell := ["bash", "-cu"]
set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command"]

default: help

help:
    @just --list

# Install dev prerequisites: the WebKitGTK/GTK + ALSA libs the Tauri +
# cpal backend links against, plus Rust, Node, and pnpm. After this,
# `just dev` runs the app; `just mesh-install` adds the mesh daemon for
# live (non-demo) mode.
[unix]
[doc("Install dev prerequisites (WebKit/GTK/ALSA, Rust, Node, pnpm).")]
setup:
    @./scripts/bootstrap.sh

[windows]
[doc("Install dev prerequisites (Rust, Node, pnpm, WebView2).")]
setup:
    @& .\scripts\bootstrap.ps1

# Run the desktop app (Tauri + Svelte) with hot reload. The app opens
# into a populated demo graph with no mesh; for live peers + streaming,
# run `just mesh-install` once (or have `myownmesh` on PATH) — the GUI
# then auto-spawns `myownmesh serve` for you.
[unix]
[doc("Run the app with hot reload (demo graph; live mesh if a daemon is present).")]
dev *ARGS:
    @cd gui && pnpm install --silent && pnpm tauri dev {{ARGS}}

[windows]
[doc("Run the app with hot reload (demo graph; live mesh if a daemon is present).")]
dev *ARGS:
    @cd gui; pnpm install --silent; pnpm tauri dev {{ARGS}}

# Install the pinned MyOwnMesh daemon (the mesh sidecar) onto your PATH,
# so the GUI can auto-spawn it and live peers + audio streaming work. The
# version is pinned in .myownmesh-rev. First build pulls the WebRTC stack
# and is slow; it's cached after that.
[unix]
[doc("Install the pinned myownmesh daemon (live mesh). Slow first build.")]
mesh-install:
    @cargo install --git https://github.com/mrjeeves/MyOwnMesh --tag "$(cat .myownmesh-rev)" myownmesh --locked

[windows]
[doc("Install the pinned myownmesh daemon (live mesh). Slow first build.")]
mesh-install:
    @cargo install --git https://github.com/mrjeeves/MyOwnMesh --tag (Get-Content .myownmesh-rev).Trim() myownmesh --locked

# Run the mesh daemon in the foreground with debug logging. `just dev`
# connects to it over the control socket (it also auto-spawns one if you
# don't run this). Requires the daemon on PATH — see `just mesh-install`.
[unix]
[doc("Run the myownmesh daemon in foreground with debug logging.")]
serve *ARGS:
    @MYOWNMESH_LOG="debug,myownmesh=debug" myownmesh serve {{ARGS}}

[windows]
[doc("Run the myownmesh daemon in foreground with debug logging.")]
serve *ARGS:
    @$env:MYOWNMESH_LOG = "debug,myownmesh=debug"; myownmesh serve {{ARGS}}

# Build the library workspace (no webview / Tauri compile).
build:
    @cargo build --workspace

build-release:
    @cargo build --workspace --release

# Build the desktop bundle (.deb / .AppImage / .dmg / .msi).
[unix]
[doc("Build the desktop bundle.")]
gui-build:
    @cd gui && pnpm install --silent && pnpm tauri build

[windows]
[doc("Build the desktop bundle.")]
gui-build:
    @cd gui; pnpm install --silent; pnpm tauri build

# Scan this machine from the CLI.
scan:
    @cargo run -p allmystuff-cli -- scan

# Show the capabilities this machine would expose on the mesh.
caps:
    @cargo run -p allmystuff-cli -- capabilities

fmt:
    @cargo fmt --all

fmt-check:
    @cargo fmt --all --check

lint:
    @cargo clippy --workspace --all-targets -- -D warnings

test:
    @cargo test --workspace --no-fail-fast

# Typecheck + build the Svelte front-end (no webview needed).
[unix]
[doc("Typecheck + build the front-end.")]
gui-check:
    @cd gui && pnpm install --frozen-lockfile && pnpm check && pnpm build

[windows]
[doc("Typecheck + build the front-end.")]
gui-check:
    @cd gui; pnpm install --frozen-lockfile; pnpm check; pnpm build

# Everything CI runs: Rust fmt + clippy + test, then the GUI typecheck/build.
check: fmt-check lint test gui-check
