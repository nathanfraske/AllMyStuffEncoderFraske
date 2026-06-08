# AllMyStuff task runner. `just` with no args runs the full check.
set shell := ["bash", "-uc"]

default: check

# One-time setup: Rust toolchain + GUI deps.
setup:
    rustup show >/dev/null 2>&1 || rustup toolchain install stable
    cd gui && pnpm install

# Build the library workspace (no GUI / webview compile).
build:
    cargo build --workspace

# Run the whole Rust test suite.
test:
    cargo test --workspace

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all --check

lint:
    cargo clippy --workspace --all-targets -- -D warnings

# Everything CI runs for the Rust side, plus the GUI typecheck + build.
check: fmt-check lint test gui-check

# Scan this machine from the CLI.
scan:
    cargo run -p allmystuff-cli -- scan

# Show the capabilities this machine would expose on the mesh.
caps:
    cargo run -p allmystuff-cli -- capabilities

# Typecheck + build the Svelte front-end (no webview needed).
gui-check:
    cd gui && pnpm install --frozen-lockfile && pnpm check && pnpm build

# Run the desktop app with hot reload. Needs a running `myownmesh serve`
# and the Tauri Linux deps (libgtk-3-dev, libwebkit2gtk-4.1-dev).
gui-dev:
    cd gui && pnpm tauri dev

# Build the desktop bundle (.deb / .AppImage / .dmg / .msi).
gui-build:
    cd gui && pnpm tauri build
