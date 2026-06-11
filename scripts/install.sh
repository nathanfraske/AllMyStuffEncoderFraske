#!/bin/sh
# AllMyStuff end-user installer.
#
# Tries (in order):
#   1. Download a pre-built release binary from GitHub for the current platform.
#   2. Fall back to building from source via cargo.
#
# Installs both the `allmystuff` CLI and the `allmystuff-gui` desktop
# app (the app is small and makes a bare `allmystuff` open it — pass
# --no-gui for a CLI-only install on a headless box), then makes sure
# the `myownmesh` daemon the app's live mode runs on is in place:
#
#   * an installed daemon that's new enough (>= the version pinned in
#     .myownmesh-rev) is used as-is;
#   * an older one is asked to update itself (`myownmesh update`);
#   * none at all → the latest MyOwnMesh release is installed next to
#     the app (same download + SHA-256 verification as the app itself).
#
# Pass --no-mesh to leave the daemon entirely alone. Mesh trouble never
# fails the install — the app always opens (demo graph) without it.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/mrjeeves/AllMyStuff/main/scripts/install.sh | sh
#   curl -fsSL https://raw.githubusercontent.com/mrjeeves/AllMyStuff/main/scripts/install.sh | sh -s -- --no-gui
#   ./scripts/install.sh --dry-run
#
# POSIX sh-compatible so `curl … | sh` works under dash, ash/busybox sh, and
# bash alike. Avoid bash-only constructs ([[ ]], arrays, ${var^^}, etc.).

set -eu
if (set -o pipefail) 2>/dev/null; then
  set -o pipefail
fi

REPO="${ALLMYSTUFF_REPO:-mrjeeves/AllMyStuff}"
MESH_REPO="${MYOWNMESH_REPO:-mrjeeves/MyOwnMesh}"
DRY_RUN=false
PREFIX_DIR="${ALLMYSTUFF_PREFIX:-}"
FORCE_SOURCE=false
INSTALL_GUI=true
INSTALL_MESH=true

for arg in "$@"; do
  case "$arg" in
    --dry-run)     DRY_RUN=true ;;
    --from-source) FORCE_SOURCE=true ;;
    --no-gui)      INSTALL_GUI=false ;;
    --no-mesh)     INSTALL_MESH=false ;;
    --prefix=*)    PREFIX_DIR="${arg#*=}" ;;
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
ASSET="allmystuff-${OS}-${ARCH}.tar.gz"
GUI_ASSET="allmystuff-gui-${OS}-${ARCH}.tar.gz"
MESH_ASSET="myownmesh-${OS}-${ARCH}.tar.gz"

# Pick install prefix. Prefer /usr/local/bin if writable (or sudo is cached);
# else ~/.local/bin so a no-sudo install still lands somewhere sensible.
if [ -z "$PREFIX_DIR" ]; then
  if [ -w /usr/local/bin ] || sudo -n true 2>/dev/null; then
    PREFIX_DIR="/usr/local/bin"
  else
    PREFIX_DIR="$HOME/.local/bin"
  fi
fi

install_binary() {
  src="$1"
  mkdir -p "$PREFIX_DIR" 2>/dev/null || sudo mkdir -p "$PREFIX_DIR"
  if [ -w "$PREFIX_DIR" ]; then
    install -m 0755 "$src" "$PREFIX_DIR/allmystuff"
  else
    sudo install -m 0755 "$src" "$PREFIX_DIR/allmystuff"
  fi
  log "Installed: $PREFIX_DIR/allmystuff"
}

install_gui_binary() {
  src="$1"
  mkdir -p "$PREFIX_DIR" 2>/dev/null || sudo mkdir -p "$PREFIX_DIR"
  if [ -w "$PREFIX_DIR" ]; then
    install -m 0755 "$src" "$PREFIX_DIR/allmystuff-gui"
  else
    sudo install -m 0755 "$src" "$PREFIX_DIR/allmystuff-gui"
  fi
  log "Installed: $PREFIX_DIR/allmystuff-gui"
}

ensure_on_path() {
  case ":$PATH:" in
    *":$PREFIX_DIR:"*) return 0 ;;
  esac

  shell_name="$(basename "${SHELL:-bash}")"
  marker="# added by allmystuff installer"
  case "$shell_name" in
    zsh)
      rc="$HOME/.zshrc"
      line="export PATH=\"$PREFIX_DIR:\$PATH\"  $marker"
      ;;
    fish)
      rc="$HOME/.config/fish/config.fish"
      line="fish_add_path -g $PREFIX_DIR  $marker"
      ;;
    *)
      rc="$HOME/.bashrc"
      line="export PATH=\"$PREFIX_DIR:\$PATH\"  $marker"
      ;;
  esac

  if grep -qsF "$marker" "$rc" 2>/dev/null; then
    warn "$PREFIX_DIR not on current PATH; PATH already added to $rc — open a new terminal."
    return 0
  fi

  mkdir -p "$(dirname "$rc")"
  if printf '\n%s\n' "$line" >> "$rc" 2>/dev/null; then
    log "Added $PREFIX_DIR to PATH in $rc"
    log "Open a new terminal (or run: source $rc) for it to take effect."
  else
    warn "$PREFIX_DIR is not on PATH. Add this to your shell rc:"
    warn "  $line"
  fi
}

# Tracked for cleanup since POSIX sh has no function-scoped RETURN trap.
_TRY_RELEASE_TMP=""
_cleanup_try_release() {
  if [ -n "$_TRY_RELEASE_TMP" ] && [ -d "$_TRY_RELEASE_TMP" ]; then
    rm -rf "$_TRY_RELEASE_TMP"
  fi
  _TRY_RELEASE_TMP=""
}

try_release() {
  if ! command -v curl >/dev/null 2>&1; then
    warn "curl missing; skipping release download."
    return 1
  fi
  api="https://api.github.com/repos/${REPO}/releases/latest"
  log "Looking up latest release: $api"
  if ! json="$(curl -fsSL "$api" 2>/dev/null)"; then
    warn "GitHub releases unreachable (or no release yet)."
    return 1
  fi
  url="$(printf '%s' "$json" | grep -Eo "https://[^\"]+/${ASSET}" | head -n1 || true)"
  if [ -z "$url" ]; then
    warn "No release asset matched ${ASSET}."
    return 1
  fi
  sha_url="${url}.sha256"
  log "Downloading $url"
  if [ "$DRY_RUN" = "true" ]; then
    log "(dry-run) would download $url"
    return 0
  fi
  _TRY_RELEASE_TMP="$(mktemp -d)"
  trap _cleanup_try_release EXIT INT TERM
  curl -fsSL "$url" -o "$_TRY_RELEASE_TMP/$ASSET"
  if curl -fsSL "$sha_url" -o "$_TRY_RELEASE_TMP/$ASSET.sha256" 2>/dev/null; then
    # set -e is suspended while this function runs inside the `if !` at the
    # bottom of the script, so the check has to gate explicitly.
    if ! (cd "$_TRY_RELEASE_TMP" && (sha256sum -c "$ASSET.sha256" 2>/dev/null || shasum -a 256 -c "$ASSET.sha256")); then
      err "SHA256 verification failed for $ASSET — not installing it."
      exit 1
    fi
  else
    warn "No SHA256 sidecar; skipping integrity check."
  fi
  tar -xzf "$_TRY_RELEASE_TMP/$ASSET" -C "$_TRY_RELEASE_TMP"
  install_binary "$_TRY_RELEASE_TMP/allmystuff"
  _cleanup_try_release
  trap - EXIT INT TERM
  return 0
}

_TRY_GUI_TMP=""
_cleanup_try_gui() {
  if [ -n "$_TRY_GUI_TMP" ] && [ -d "$_TRY_GUI_TMP" ]; then
    rm -rf "$_TRY_GUI_TMP"
  fi
  _TRY_GUI_TMP=""
}

# Best-effort GUI install: fetch the portable `allmystuff-gui` tarball
# and drop it next to the CLI. Returns non-zero (without aborting the
# overall install) if the asset is missing or unreachable — an older
# release may predate the GUI binary, and the CLI is the part that
# must succeed.
try_release_gui() {
  if ! command -v curl >/dev/null 2>&1; then
    return 1
  fi
  api="https://api.github.com/repos/${REPO}/releases/latest"
  if ! json="$(curl -fsSL "$api" 2>/dev/null)"; then
    warn "GitHub releases unreachable; skipping GUI."
    return 1
  fi
  url="$(printf '%s' "$json" | grep -Eo "https://[^\"]+/${GUI_ASSET}" | head -n1 || true)"
  if [ -z "$url" ]; then
    warn "No GUI asset matched ${GUI_ASSET} in the latest release."
    return 1
  fi
  sha_url="${url}.sha256"
  log "Downloading $url"
  if [ "$DRY_RUN" = "true" ]; then
    log "(dry-run) would download $url"
    return 0
  fi
  _TRY_GUI_TMP="$(mktemp -d)"
  trap _cleanup_try_gui EXIT INT TERM
  curl -fsSL "$url" -o "$_TRY_GUI_TMP/$GUI_ASSET"
  if curl -fsSL "$sha_url" -o "$_TRY_GUI_TMP/$GUI_ASSET.sha256" 2>/dev/null; then
    # Explicit gate — set -e is suspended in this function's call context.
    if ! (cd "$_TRY_GUI_TMP" && (sha256sum -c "$GUI_ASSET.sha256" 2>/dev/null || shasum -a 256 -c "$GUI_ASSET.sha256")); then
      warn "SHA256 verification failed for $GUI_ASSET — not installing the GUI."
      _cleanup_try_gui
      trap - EXIT INT TERM
      return 1
    fi
  else
    warn "No SHA256 sidecar for GUI; skipping integrity check."
  fi
  tar -xzf "$_TRY_GUI_TMP/$GUI_ASSET" -C "$_TRY_GUI_TMP"
  install_gui_binary "$_TRY_GUI_TMP/allmystuff-gui"
  _cleanup_try_gui
  trap - EXIT INT TERM
  return 0
}

# ---------------------------------------------------------------------------
# The mesh daemon. The desktop app's live mode runs on `myownmesh`
# (demo mode needs nothing), so the installer makes sure a usable daemon
# is in place — without ever failing the app install over it:
#
#   1. installed and new enough (>= the .myownmesh-rev pin) → used as-is;
#   2. installed but older → asked to update itself (`myownmesh update`);
#   3. missing → the latest MyOwnMesh release is installed next to the
#      app, where the app finds it without any PATH refresh.

install_mesh_binary() {
  src="$1"
  mkdir -p "$PREFIX_DIR" 2>/dev/null || sudo mkdir -p "$PREFIX_DIR"
  if [ -w "$PREFIX_DIR" ]; then
    install -m 0755 "$src" "$PREFIX_DIR/myownmesh"
  else
    sudo install -m 0755 "$src" "$PREFIX_DIR/myownmesh"
  fi
  log "Installed: $PREFIX_DIR/myownmesh"
}

_TRY_MESH_TMP=""
_cleanup_try_mesh() {
  if [ -n "$_TRY_MESH_TMP" ] && [ -d "$_TRY_MESH_TMP" ]; then
    rm -rf "$_TRY_MESH_TMP"
  fi
  _TRY_MESH_TMP=""
}

# Fetch the daemon tarball from MyOwnMesh's latest release (SHA-256
# verified, like the app's own assets) and install it next to the app.
try_release_mesh() {
  if ! command -v curl >/dev/null 2>&1; then
    return 1
  fi
  api="https://api.github.com/repos/${MESH_REPO}/releases/latest"
  if ! json="$(curl -fsSL "$api" 2>/dev/null)"; then
    warn "MyOwnMesh releases unreachable."
    return 1
  fi
  url="$(printf '%s' "$json" | grep -Eo "https://[^\"]+/${MESH_ASSET}" | head -n1 || true)"
  if [ -z "$url" ]; then
    warn "No release asset matched ${MESH_ASSET} in MyOwnMesh's latest release."
    return 1
  fi
  sha_url="${url}.sha256"
  log "Downloading $url"
  _TRY_MESH_TMP="$(mktemp -d)"
  trap _cleanup_try_mesh EXIT INT TERM
  if ! curl -fsSL "$url" -o "$_TRY_MESH_TMP/$MESH_ASSET"; then
    _cleanup_try_mesh
    trap - EXIT INT TERM
    return 1
  fi
  if curl -fsSL "$sha_url" -o "$_TRY_MESH_TMP/$MESH_ASSET.sha256" 2>/dev/null; then
    if ! (cd "$_TRY_MESH_TMP" && (sha256sum -c "$MESH_ASSET.sha256" 2>/dev/null || shasum -a 256 -c "$MESH_ASSET.sha256")); then
      warn "SHA256 verification failed for $MESH_ASSET — not installing the daemon."
      _cleanup_try_mesh
      trap - EXIT INT TERM
      return 1
    fi
  else
    warn "No SHA256 sidecar for the daemon; skipping integrity check."
  fi
  if ! tar -xzf "$_TRY_MESH_TMP/$MESH_ASSET" -C "$_TRY_MESH_TMP"; then
    _cleanup_try_mesh
    trap - EXIT INT TERM
    return 1
  fi
  install_mesh_binary "$_TRY_MESH_TMP/myownmesh"
  _cleanup_try_mesh
  trap - EXIT INT TERM
  return 0
}

# version_ge A B — true when dotted version A >= B, comparing the numeric
# major/minor/patch fields.
version_ge() {
  IFS=. read -r a1 a2 a3 <<EOF
$1
EOF
  IFS=. read -r b1 b2 b3 <<EOF
$2
EOF
  a1="${a1%%[!0-9]*}"; a2="${a2%%[!0-9]*}"; a3="${a3%%[!0-9]*}"
  b1="${b1%%[!0-9]*}"; b2="${b2%%[!0-9]*}"; b3="${b3%%[!0-9]*}"
  a1="${a1:-0}"; a2="${a2:-0}"; a3="${a3:-0}"
  b1="${b1:-0}"; b2="${b2:-0}"; b3="${b3:-0}"
  if [ "$a1" -ne "$b1" ]; then [ "$a1" -gt "$b1" ]; return; fi
  if [ "$a2" -ne "$b2" ]; then [ "$a2" -gt "$b2" ]; return; fi
  [ "$a3" -ge "$b3" ]
}

# The minimum daemon version this app wants: the rev pinned in
# .myownmesh-rev, read from the checkout when running from one, fetched
# from the repo otherwise. Prints nothing when the pin is unreachable or
# isn't a version tag (a sha pin can't be compared) — any installed
# daemon passes then.
mesh_min_version() {
  rev=""
  if [ -f "$0" ] && [ -f "$(dirname "$0")/../.myownmesh-rev" ]; then
    rev="$(cat "$(dirname "$0")/../.myownmesh-rev" 2>/dev/null || true)"
  fi
  if [ -z "$rev" ] && command -v curl >/dev/null 2>&1; then
    rev="$(curl -fsSL "https://raw.githubusercontent.com/${REPO}/main/.myownmesh-rev" 2>/dev/null || true)"
  fi
  rev="$(printf '%s' "$rev" | tr -d '[:space:]')"
  case "$rev" in
    v[0-9]*) printf '%s' "${rev#v}" ;;
    *) ;;
  esac
}

# `myownmesh --version` → "0.2.4" (empty when the binary won't answer).
installed_mesh_version() {
  "$1" --version 2>/dev/null | awk 'NR==1 {print $NF}' | tr -cd '0-9.'
}

ensure_mesh() {
  # Prefer a daemon sitting next to the app (where we'd install one —
  # the app checks there first too), then PATH.
  existing=""
  if [ -x "$PREFIX_DIR/myownmesh" ]; then
    existing="$PREFIX_DIR/myownmesh"
  elif command -v myownmesh >/dev/null 2>&1; then
    existing="$(command -v myownmesh)"
  fi
  min="$(mesh_min_version || true)"

  if [ -n "$existing" ]; then
    ver="$(installed_mesh_version "$existing" || true)"
    if [ -n "$ver" ] && { [ -z "$min" ] || version_ge "$ver" "$min"; }; then
      if [ -n "$min" ]; then
        log "Mesh: using the installed myownmesh v$ver at $existing (needs v$min+)."
      else
        log "Mesh: using the installed myownmesh v$ver at $existing."
      fi
      return 0
    fi
    if [ -n "$ver" ]; then
      log "Mesh: installed myownmesh is v$ver but this release wants v$min+."
    else
      log "Mesh: $existing didn't answer --version."
    fi
    if [ "$DRY_RUN" = "true" ]; then
      log "(dry-run) would ask it to update itself: myownmesh update"
      return 0
    fi
    log "Asking it to update itself (myownmesh update)…"
    # Its own output lands here; the re-check below is what decides.
    "$existing" update || true
    ver="$(installed_mesh_version "$existing" || true)"
    if [ -n "$ver" ] && { [ -z "$min" ] || version_ge "$ver" "$min"; }; then
      log "Mesh: myownmesh is now v$ver."
    else
      warn "Mesh: couldn't bring myownmesh up to v${min:-a readable version} (see above)."
      warn "The app still runs — an older daemon just lacks the newer mesh"
      warn "features. Retry later with: myownmesh update"
    fi
    return 0
  fi

  if [ "$DRY_RUN" = "true" ]; then
    log "(dry-run) would install the myownmesh daemon ($MESH_ASSET) next to the app"
    return 0
  fi
  log "Mesh: no myownmesh daemon found — installing it next to the app…"
  if try_release_mesh; then
    ver="$(installed_mesh_version "$PREFIX_DIR/myownmesh" || true)"
    log "Mesh: installed myownmesh${ver:+ v$ver} — the app starts it automatically."
  else
    warn "Mesh: couldn't fetch the daemon. The app still opens (demo graph);"
    warn "for live machines, re-run this installer later or use MyOwnMesh's:"
    warn "  curl -fsSL https://raw.githubusercontent.com/${MESH_REPO}/main/scripts/install.sh | sh -s -- --no-gui"
  fi
}

build_from_source() {
  log "Building from source…"
  if ! command -v cargo >/dev/null 2>&1; then
    err "cargo not found. Install Rust via https://rustup.rs first."
    exit 1
  fi
  if ! command -v git >/dev/null 2>&1; then
    err "git is required to build from source."
    exit 1
  fi
  if [ -f Cargo.toml ] && [ -d crates/allmystuff-cli ]; then
    repo_dir="$(pwd)"
    log "Using current directory as source: $repo_dir"
  else
    repo_dir="$(mktemp -d)/AllMyStuff"
    log "Cloning into $repo_dir"
    if [ "$DRY_RUN" != "true" ]; then
      git clone --depth 1 "https://github.com/${REPO}.git" "$repo_dir"
    fi
  fi
  if [ "$DRY_RUN" = "true" ]; then
    log "(dry-run) would build in $repo_dir"
    return 0
  fi
  ( cd "$repo_dir" && cargo build --release --bin allmystuff )
  built="$repo_dir/target/release/allmystuff"
  if [ ! -x "$built" ]; then
    err "Build did not produce $built"
    exit 1
  fi
  install_binary "$built"
}

INSTALLED_FROM_RELEASE=false
if [ "$FORCE_SOURCE" = "true" ] || ! try_release; then
  build_from_source
else
  INSTALLED_FROM_RELEASE=true
fi

# Desktop app (allmystuff-gui). On by default — it's small and lets a
# bare `allmystuff` open the app. `--no-gui` skips it. Only attempted
# on the release path; building the GUI from source needs the full
# Tauri/pnpm toolchain, which is out of scope for a curl|sh installer.
GUI_INSTALLED=false
if [ "$INSTALL_GUI" = "true" ]; then
  if [ "$INSTALLED_FROM_RELEASE" = "true" ]; then
    if try_release_gui; then
      GUI_INSTALLED=true
    else
      warn "GUI binary not installed; a bare 'allmystuff' will print a hint until it is. Re-run the installer later, or build it from gui/."
    fi
  elif [ "$DRY_RUN" = "true" ]; then
    log "(dry-run) would install the GUI binary ($GUI_ASSET) next to allmystuff"
  else
    warn "Built the CLI from source; skipping the GUI binary (needs the Tauri/pnpm toolchain)."
    warn "Build it with:  cd gui && pnpm install && pnpm tauri build"
  fi
fi

# Mesh daemon — see the block above ensure_mesh for the rules. Only the
# desktop app talks to the daemon, so a CLI-only install skips it; a GUI
# built from source bundles its own (gui's build.rs).
if [ "$INSTALL_MESH" != "true" ]; then
  log "Skipping the mesh daemon (--no-mesh)."
elif [ "$INSTALL_GUI" != "true" ]; then
  log "CLI-only install; skipping the mesh daemon (only the desktop app uses it)."
elif [ "$GUI_INSTALLED" = "true" ]; then
  ensure_mesh
else
  log "Mesh: skipped — no desktop app was installed, and only the app uses the"
  log "daemon (a GUI built from gui/ bundles its own)."
fi

if [ "$DRY_RUN" != "true" ]; then
  ensure_on_path
fi

log "Done."
log ""
log "Quick start:"
if [ "$INSTALL_GUI" = "true" ]; then
  log "  allmystuff                 # open the desktop app"
fi
log "  allmystuff scan            # pretty inventory of this machine"
log "  allmystuff capabilities    # what this machine would expose on the mesh"
log "  allmystuff update          # update to the latest release"
if [ "$INSTALL_GUI" = "true" ]; then
  log ""
  log "The app opens into a demo graph even with no mesh. Live machines run"
  log "on the 'myownmesh' daemon (handled above), which the app starts and"
  log "manages automatically."
  if [ "$INSTALL_MESH" != "true" ]; then
    log "You skipped it (--no-mesh) — when you want live mode:"
    log "  curl -fsSL https://raw.githubusercontent.com/${MESH_REPO}/main/scripts/install.sh | sh -s -- --no-gui"
  fi
fi
