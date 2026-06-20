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
# `just dev` runs the app — the mesh daemon is bundled automatically by
# the GUI's first build (it fetches the rev pinned in .myownmesh-rev).
[unix]
[doc("Install dev prerequisites (WebKit/GTK/ALSA, Rust, Node, pnpm).")]
setup:
    @./scripts/bootstrap.sh

[windows]
[doc("Install dev prerequisites (Rust, Node, pnpm, WebView2).")]
setup:
    @& .\scripts\bootstrap.ps1

# Run the desktop app (Tauri + Svelte) with hot reload. We build the
# `allmystuff-serve` node binary first: the GUI is a thin client that spawns it
# (one node per machine), and the GUI's build.rs bundles whatever's in
# `node/target` as a sidecar — so without this the app would ship a stub and
# spawn nothing. The mesh daemon underneath is still bundled automatically
# (build.rs fetches the rev pinned in .myownmesh-rev the first time), and the
# node spawns it. So `just dev` gives you the full app with live mesh.
[unix]
[doc("Run the app with hot reload (node + mesh daemon bundled automatically).")]
dev *ARGS:
    @cargo build --manifest-path node/Cargo.toml --bin allmystuff-serve
    @cd gui && pnpm install --silent && pnpm tauri dev {{ARGS}}

[windows]
[doc("Run the app with hot reload (node + mesh daemon bundled automatically).")]
dev *ARGS:
    @cargo build --manifest-path node/Cargo.toml --bin allmystuff-serve
    @cd gui; pnpm install --silent; pnpm tauri dev {{ARGS}}

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

# Run this machine on the mesh, headless (the node `allmystuff serve` runs).
# Builds from the node workspace and spawns the myownmesh daemon itself.
serve *ARGS:
    @cargo run --manifest-path node/Cargo.toml --bin allmystuff-serve -- {{ARGS}}

# Stop this machine's whole mesh stack: the AllMyStuff node (`allmystuff-serve`)
# and the `myownmesh` daemon it spawns. Use it for a clean slate between `just
# dev` runs, or to clear an *orphaned* daemon — on macOS a hard Ctrl-C out of
# `just dev` can leave the daemon running (no kernel parent-death signal there),
# and the next run silently reuses it. The node is SIGTERM'd first so it shuts
# down cleanly (which kills its own daemon child); then any leftover daemon is
# swept. Killing the daemon alone is pointless in dev — the node doesn't respawn
# it — so this always takes down both; restart with `just dev`.
[unix]
[doc("Kill this machine's mesh stack (node + myownmesh daemon).")]
kill:
    @pkill -TERM -f '[a]llmystuff-serve' 2>/dev/null; pkill -f '[m]yownmesh.* serve' 2>/dev/null; echo 'stopped the node + mesh daemon (whatever was running)'

[windows]
[doc("Kill this machine's mesh stack (node + myownmesh daemon).")]
kill:
    @Get-Process allmystuff-serve,myownmesh,myownmesh-* -ErrorAction SilentlyContinue | Stop-Process -Force; Write-Output "mesh stack stopped (allmystuff-serve + myownmesh)"; exit 0

# Clean restart: kill the mesh stack, then start the app fresh.
[doc("Kill the mesh stack, then `just dev`.")]
restart *ARGS: kill
    @just dev {{ARGS}}

fmt:
    @cargo fmt --all

fmt-check:
    @cargo fmt --all --check

lint:
    @cargo clippy --workspace --all-targets -- -D warnings

test:
    @cargo test --workspace --no-fail-fast

# The headless node engine lives in its own workspace (heavy media deps), so
# its fmt/clippy/test don't ride the root `--workspace` flags — run them here.
node-check:
    @cd node && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test

# Typecheck + build the Svelte front-end (no webview needed).
[unix]
[doc("Typecheck + build the front-end.")]
gui-check:
    @cd gui && pnpm install --frozen-lockfile && pnpm check && pnpm build

[windows]
[doc("Typecheck + build the front-end.")]
gui-check:
    @cd gui; pnpm install --frozen-lockfile; pnpm check; pnpm build

# Everything CI runs: Rust fmt + clippy + test (library workspace + the node
# engine), then the GUI typecheck/build.
check: fmt-check lint test node-check gui-check

# Cut a release: bump every crate's version (+ the GUI sub-workspace),
# commit, push, trigger the workflow. Mirrors MyOwnMesh / MyOwnLLM — the
# user runs `just release 0.2.0` and the release.yml workflow verifies the
# manifests, builds the per-platform bundles + portable binaries, and
# publishes the GitHub release. Bash script — the release flow runs from a
# Linux/macOS box.
[unix]
[doc("Cut a release: bump versions, commit, push, trigger the workflow.")]
release VERSION:
    @./scripts/bump-version.sh {{VERSION}}
    @if ! git diff --quiet Cargo.toml Cargo.lock gui/src-tauri/Cargo.toml gui/src-tauri/Cargo.lock gui/package.json node/Cargo.toml node/Cargo.lock; then \
        git add Cargo.toml Cargo.lock crates/*/Cargo.toml gui/src-tauri/Cargo.toml gui/src-tauri/Cargo.lock gui/package.json node/Cargo.toml node/Cargo.lock; \
        git commit -m "chore(release): {{VERSION}}"; \
    fi
    @git push
    @gh workflow run release.yml -f tag=v{{VERSION}}
