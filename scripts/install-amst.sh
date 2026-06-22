#!/bin/sh
# AMSTerm (`amst`) installer — the AllMyStuff mesh terminal, as its own program.
#
#   curl -fsSL https://allmystuff.works/install-amst.sh | sh
#
# `amst` is a small, self-contained client of this machine's AllMyStuff node
# (the `allmystuff-serve` engine the desktop app and `allmystuff serve` run): it
# opens a real shell on any machine you own, over the mesh. This script installs
# *just* `amst` and its desktop integration — separate from the main AllMyStuff
# install, which it neither needs nor touches. It does rely on an AllMyStuff
# node being present to actually reach machines; `amst` starts one itself if
# `allmystuff-serve` is installed, and tells you how to get it if not.
#
# POSIX sh-compatible (dash / busybox / bash). Avoid bash-only constructs.

set -eu
if (set -o pipefail) 2>/dev/null; then
  set -o pipefail
fi

REPO="${ALLMYSTUFF_REPO:-mrjeeves/AllMyStuff}"
DRY_RUN=false
FORCE_SOURCE=false
NO_DESKTOP=false
UNINSTALL=false
PREFIX_DIR="${ALLMYSTUFF_PREFIX:-}"

usage() {
  cat <<'EOF'
AMSTerm installer — installs `amst`, the AllMyStuff mesh terminal.

USAGE:
    install-amst.sh [OPTIONS]

OPTIONS:
    --prefix=DIR    Install into DIR (default: /usr/local/bin or ~/.local/bin)
    --from-source   Build amst from source with cargo instead of a release
    --no-desktop    Don't create the app-menu launcher (binary only)
    --uninstall     Remove amst and its desktop launcher
    --dry-run       Show what would happen without changing anything
    --help          Show this help

After install:  amst            a shell on this machine (your fleet can attach)
                amst <machine>  a shell on another machine you own
                amst --list     the machines you can reach
EOF
}

for arg in "$@"; do
  case "$arg" in
    --dry-run)     DRY_RUN=true ;;
    --from-source) FORCE_SOURCE=true ;;
    --no-desktop)  NO_DESKTOP=true ;;
    --uninstall)   UNINSTALL=true ;;
    --prefix=*)    PREFIX_DIR="${arg#*=}" ;;
    -h|--help)     usage; exit 0 ;;
    *) ;;
  esac
done

log()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m!!!\033[0m %s\n' "$*" >&2; }
err()  { printf '\033[1;31mxxx\033[0m %s\n' "$*" >&2; }

OS_RAW="$(uname -s | tr '[:upper:]' '[:lower:]')"
case "$OS_RAW" in
  darwin) OS="macos" ;;
  linux)  OS="linux" ;;
  *)      OS="$OS_RAW" ;;
esac
ARCH_RAW="$(uname -m)"
case "$ARCH_RAW" in
  x86_64|amd64)  ARCH="x86_64" ;;
  aarch64|arm64) ARCH="aarch64" ;;
  *)             ARCH="$ARCH_RAW" ;;
esac
ASSET="amst-${OS}-${ARCH}.tar.gz"

# Where the Linux app-menu launcher (and its uninstall) live.
DESKTOP_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
DESKTOP_FILE="$DESKTOP_DIR/amst.desktop"

# Pick the install prefix — /usr/local/bin when writable (or sudo is cached),
# else ~/.local/bin so a no-sudo install still lands somewhere on PATH.
if [ -z "$PREFIX_DIR" ]; then
  if [ -w /usr/local/bin ] || sudo -n true 2>/dev/null; then
    PREFIX_DIR="/usr/local/bin"
  else
    PREFIX_DIR="$HOME/.local/bin"
  fi
fi

install_amst_binary() {
  src="$1"
  mkdir -p "$PREFIX_DIR" 2>/dev/null || sudo mkdir -p "$PREFIX_DIR"
  if [ -w "$PREFIX_DIR" ]; then
    install -m 0755 "$src" "$PREFIX_DIR/amst"
  else
    sudo install -m 0755 "$src" "$PREFIX_DIR/amst"
  fi
  log "Installed: $PREFIX_DIR/amst"
}

# A freedesktop launcher so AMSTerm shows up in the app menu and is pinnable to
# the dock/taskbar. `Terminal=true` runs amst inside the user's terminal
# emulator (it's a TUI client); StartupWMClass helps the WM group it.
write_desktop_entry() {
  [ "$OS" = "linux" ] || return 0
  [ "$NO_DESKTOP" = "true" ] && return 0
  if [ "$DRY_RUN" = "true" ]; then
    log "(dry-run) would write $DESKTOP_FILE"
    return 0
  fi
  mkdir -p "$DESKTOP_DIR"
  cat > "$DESKTOP_FILE" <<EOF
[Desktop Entry]
Type=Application
Name=AMSTerm
GenericName=AllMyStuff Terminal
Comment=Open a shell on any machine you own, over the AllMyStuff mesh
Exec=amst
Icon=utilities-terminal
Terminal=true
Categories=System;TerminalEmulator;Network;
Keywords=terminal;shell;mesh;allmystuff;amst;remote;
StartupWMClass=amst
EOF
  chmod 0644 "$DESKTOP_FILE"
  command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$DESKTOP_DIR" 2>/dev/null || true
  log "Installed app launcher: $DESKTOP_FILE"
}

ensure_on_path() {
  case ":$PATH:" in
    *":$PREFIX_DIR:"*) return 0 ;;
  esac
  warn "$PREFIX_DIR is not on your PATH. Add it, e.g.:"
  warn "  export PATH=\"$PREFIX_DIR:\$PATH\""
}

# ---- uninstall -------------------------------------------------------------

do_uninstall() {
  log "Removing amst…"
  for dir in "$PREFIX_DIR" /usr/local/bin "$HOME/.local/bin"; do
    if [ -e "$dir/amst" ]; then
      if [ "$DRY_RUN" = "true" ]; then
        log "(dry-run) would remove $dir/amst"
      elif [ -w "$dir" ]; then
        rm -f "$dir/amst" && log "Removed $dir/amst"
      else
        sudo rm -f "$dir/amst" && log "Removed $dir/amst"
      fi
    fi
  done
  if [ -e "$DESKTOP_FILE" ]; then
    if [ "$DRY_RUN" = "true" ]; then
      log "(dry-run) would remove $DESKTOP_FILE"
    else
      rm -f "$DESKTOP_FILE" && log "Removed $DESKTOP_FILE"
    fi
  fi
  log "Done. (The AllMyStuff node, if any, was left untouched.)"
}

# ---- release download (mirrors install.sh) ---------------------------------

_TMP=""
_cleanup() { [ -n "$_TMP" ] && [ -d "$_TMP" ] && rm -rf "$_TMP"; _TMP=""; }

try_release() {
  command -v curl >/dev/null 2>&1 || { warn "curl missing; can't fetch a release."; return 1; }
  api="https://api.github.com/repos/${REPO}/releases/latest"
  log "Looking up latest release: $api"
  json="$(curl -fsSL "$api" 2>/dev/null)" || { warn "GitHub releases unreachable (or no release yet)."; return 1; }
  url="$(printf '%s' "$json" | grep -Eo "https://[^\"]+/${ASSET}" | head -n1 || true)"
  [ -n "$url" ] || { warn "No release asset matched ${ASSET}."; return 1; }
  log "Downloading $url"
  if [ "$DRY_RUN" = "true" ]; then log "(dry-run) would download + install $url"; return 0; fi
  _TMP="$(mktemp -d)"
  trap _cleanup EXIT INT TERM
  curl -fsSL "$url" -o "$_TMP/$ASSET"
  if curl -fsSL "${url}.sha256" -o "$_TMP/$ASSET.sha256" 2>/dev/null; then
    if ! (cd "$_TMP" && (sha256sum -c "$ASSET.sha256" 2>/dev/null || shasum -a 256 -c "$ASSET.sha256")); then
      err "SHA256 verification failed for $ASSET — not installing it."
      exit 1
    fi
  else
    warn "No SHA256 sidecar; skipping integrity check."
  fi
  tar -xzf "$_TMP/$ASSET" -C "$_TMP"
  install_amst_binary "$_TMP/amst"
  _cleanup
  trap - EXIT INT TERM
  return 0
}

build_from_source() {
  log "Building amst from source…"
  command -v cargo >/dev/null 2>&1 || { err "cargo not found. Install Rust via https://rustup.rs first."; exit 1; }
  if [ -f Cargo.toml ] && [ -d crates/allmystuff-term ]; then
    repo_dir="$(pwd)"
  else
    command -v git >/dev/null 2>&1 || { err "git is required to build from source."; exit 1; }
    repo_dir="$(mktemp -d)/AllMyStuff"
    log "Cloning into $repo_dir"
    [ "$DRY_RUN" = "true" ] || git clone --depth 1 "https://github.com/${REPO}.git" "$repo_dir"
  fi
  if [ "$DRY_RUN" = "true" ]; then log "(dry-run) would build amst in $repo_dir"; return 0; fi
  ( cd "$repo_dir" && cargo build --release --bin amst )
  built="$repo_dir/target/release/amst"
  [ -x "$built" ] || { err "Build did not produce $built"; exit 1; }
  install_amst_binary "$built"
}

# ---- main ------------------------------------------------------------------

if [ "$UNINSTALL" = "true" ]; then
  do_uninstall
  exit 0
fi

if [ "$OS" != "linux" ] && [ "$OS" != "macos" ]; then
  err "Unsupported OS '$OS'. On Windows use install-amst.ps1."
  exit 1
fi

if [ "$FORCE_SOURCE" = "true" ]; then
  build_from_source
elif ! try_release; then
  warn "Falling back to building from source."
  build_from_source
fi

write_desktop_entry
ensure_on_path

# A working amst needs this machine's AllMyStuff node. It starts one itself if
# `allmystuff-serve` is installed; otherwise point the user at it.
if ! command -v allmystuff-serve >/dev/null 2>&1 && ! command -v allmystuff >/dev/null 2>&1; then
  warn "No AllMyStuff node found (allmystuff-serve). amst needs one to reach machines."
  warn "Install AllMyStuff:  curl -fsSL https://allmystuff.works/install.sh | sh"
fi

log "Done. Try:  amst            (a shell on this machine)"
log "            amst --list     (the machines you can reach)"
if [ "$OS" = "macos" ]; then
  log "macOS: amst is on your PATH; run it from any terminal. (A Dock/Services"
  log "wrapper app is a planned follow-up.)"
fi
