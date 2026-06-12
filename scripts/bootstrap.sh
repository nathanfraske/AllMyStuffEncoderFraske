#!/usr/bin/env bash
# Install dev prerequisites for AllMyStuff on Linux / macOS.
#
# The library workspace is pure Rust, but the desktop app under gui/ is a
# Tauri + Svelte app whose backend also links the cpal audio stack, so a
# working dev setup needs the WebKitGTK / GTK libraries, ALSA, Node, and
# pnpm. `just dev` runs `pnpm tauri dev` in gui/; without Node + pnpm that
# step dies with "pnpm: command not found". This mirrors MyOwnMesh /
# MyOwnLLM so the three apps share one setup story.
#
# The `myownmesh` daemon is not a prerequisite: the GUI's build.rs
# fetches and bundles it automatically on the first `just dev` (and the
# app runs fine in demo mode without any mesh at all).
#
# Idempotent: re-running is cheap and safe — anything already present is
# skipped.

set -euo pipefail

log()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m!!!\033[0m %s\n' "$*" >&2; }

have() { command -v "$1" >/dev/null 2>&1; }

OS="$(uname -s)"

# ---------------------------------------------------------------------------
# Platform packages — the WebKitGTK / GTK stack Tauri's webview links
# against, plus ALSA for the cpal audio bridge. The library crates need
# none of this; the GUI backend won't compile without it.
# ---------------------------------------------------------------------------

install_linux_deps() {
  if [[ -f /etc/os-release ]]; then
    # shellcheck disable=SC1091
    . /etc/os-release
  fi

  case "${ID:-}" in
    ubuntu|debian|pop|linuxmint|raspbian)
      log "Installing Tauri + audio build deps (apt)…"
      sudo apt-get update -qq
      # xdg-utils backs Tauri's AppImage bundler; libasound2-dev is the
      # ALSA dev headers cpal links against; pipewire + xkbcommon + gbm
      # back the console's screen capture (xcap) and input injection
      # (enigo). clang + libclang-dev are bindgen's: the PipeWire
      # bindings (libspa-sys) parse the C headers with libclang, and
      # without clang's own resource headers the parse dies on
      # "'stdbool.h' file not found" (CI never sees it — GitHub
      # runners ship clang preinstalled).
      sudo apt-get install -y --no-install-recommends \
        libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev \
        librsvg2-dev libssl-dev libasound2-dev xdg-utils curl wget file \
        libpipewire-0.3-dev libxkbcommon-dev libgbm-dev \
        clang libclang-dev \
        build-essential pkg-config
      ;;
    fedora|rhel|centos)
      log "Installing Tauri + audio build deps (dnf)…"
      sudo dnf install -y \
        webkit2gtk4.1-devel gtk3-devel libappindicator-gtk3-devel \
        librsvg2-devel openssl-devel alsa-lib-devel curl wget file gcc \
        pipewire-devel libxkbcommon-devel mesa-libgbm-devel \
        clang clang-devel \
        gcc-c++ make pkgconf-pkg-config
      ;;
    arch|manjaro)
      log "Installing Tauri + audio build deps (pacman)…"
      sudo pacman -S --needed --noconfirm \
        webkit2gtk-4.1 gtk3 libayatana-appindicator librsvg openssl \
        alsa-lib curl wget file base-devel clang
      ;;
    *)
      warn "Unrecognised Linux distro (${ID:-?}). Install Tauri deps manually:"
      warn "  https://tauri.app/start/prerequisites/#linux"
      warn "…plus the ALSA dev headers (libasound2-dev / alsa-lib-devel)"
      warn "and clang + libclang (bindgen builds the PipeWire bindings)."
      ;;
  esac
}

install_macos_deps() {
  if ! xcode-select -p >/dev/null 2>&1; then
    log "Installing Xcode Command Line Tools (you may be prompted)…"
    xcode-select --install || true
  fi
}

case "$OS" in
  Linux)  install_linux_deps ;;
  Darwin) install_macos_deps ;;
  *)      warn "Unsupported OS: $OS — proceeding anyway." ;;
esac

# ---------------------------------------------------------------------------
# Rust — channel + components come from rust-toolchain.toml. `rustup show`
# installs the pinned toolchain on first run; a no-op once present.
# ---------------------------------------------------------------------------

if ! have rustup && ! have cargo; then
  log "Installing rustup (Rust toolchain manager)…"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path
  # shellcheck disable=SC1090
  . "$HOME/.cargo/env"
fi
if have rustup; then
  log "Ensuring the pinned Rust toolchain is installed…"
  rustup show
fi

# ---------------------------------------------------------------------------
# Node + pnpm — `just dev` runs `pnpm tauri dev` inside gui/, and Vite 6
# needs Node 20+. Install a current Node when it's missing or too old, then
# get pnpm through corepack.
# ---------------------------------------------------------------------------

NODE_MAJOR=22  # LTS line to install when Node is absent or too old.

node_major() { node -v 2>/dev/null | sed 's/^v//; s/\..*//'; }

install_node_linux() {
  [[ -f /etc/os-release ]] && . /etc/os-release
  case " ${ID:-} ${ID_LIKE:-} " in
    *" debian "*|*" ubuntu "*|*" raspbian "*)
      log "Installing Node ${NODE_MAJOR}.x via NodeSource…"
      curl -fsSL "https://deb.nodesource.com/setup_${NODE_MAJOR}.x" | sudo -E bash -
      sudo apt-get install -y nodejs
      ;;
    *" fedora "*|*" rhel "*|*" centos "*)
      log "Installing Node via dnf…"
      sudo dnf install -y nodejs npm
      ;;
    *" arch "*)
      log "Installing Node via pacman…"
      sudo pacman -S --needed --noconfirm nodejs npm
      ;;
    *)
      warn "Don't know how to install Node on this distro (${ID:-?})."
      warn "Install Node ${NODE_MAJOR}+ from https://nodejs.org, then re-run \`just setup\`."
      exit 1
      ;;
  esac
}

ensure_node() {
  if have node; then
    local maj
    maj="$(node_major)"
    if [[ -n "$maj" && "$maj" -ge 20 ]]; then
      return
    fi
    warn "Node $(node -v 2>/dev/null) is older than v20 (Vite 6 needs 20+) — installing v${NODE_MAJOR}."
  fi
  if [[ "$OS" == "Darwin" ]]; then
    if have brew; then
      log "Installing Node via brew…"
      brew install node
    else
      warn "Homebrew not found. Install Node ${NODE_MAJOR}+ from https://nodejs.org, then re-run."
      exit 1
    fi
  else
    install_node_linux
  fi
  hash -r 2>/dev/null || true
}

ensure_pnpm() {
  if have pnpm; then
    return
  fi
  if have corepack; then
    log "Enabling pnpm via corepack…"
    corepack enable 2>/dev/null || sudo corepack enable 2>/dev/null || true
    corepack prepare pnpm@latest --activate || true
    hash -r 2>/dev/null || true
  fi
  if ! have pnpm && have npm; then
    log "Installing pnpm via npm…"
    sudo npm install -g pnpm 2>/dev/null || npm install -g pnpm || true
    hash -r 2>/dev/null || true
  fi
  if ! have pnpm; then
    warn "Could not put pnpm on PATH automatically. Install it manually:"
    warn "  https://pnpm.io/installation"
    exit 1
  fi
}

ensure_node
ensure_pnpm

# Pre-install the GUI deps so the first `just dev` is fast. The
# @tauri-apps/cli used by `pnpm tauri …` is a gui/ devDependency, so no
# global cargo install is needed.
log "Installing GUI dependencies…"
( cd gui && pnpm install --silent )

log "✓ setup complete — \`just dev\` runs the app."
log "  (The first build fetches + bundles the mesh daemon automatically.)"
