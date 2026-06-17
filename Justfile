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

# Run the desktop app (Tauri + Svelte) with hot reload. The mesh daemon is
# bundled automatically (build.rs fetches the rev pinned in .myownmesh-rev
# the first time — slowest part of the first run), and the GUI auto-spawns
# it. So `just dev` gives you the full app with live mesh, no extra steps.
[unix]
[doc("Run the app with hot reload (mesh daemon bundled automatically).")]
dev *ARGS:
    @cd gui && pnpm install --silent && pnpm tauri dev {{ARGS}}

[windows]
[doc("Run the app with hot reload (mesh daemon bundled automatically).")]
dev *ARGS:
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
