#!/usr/bin/env bash
# Narrow native Ubuntu/Debian setup for the reviewed Mesh-control gates.
# Package sources: scripts/bootstrap.sh; .github/workflows/ci.yml node media
# step; release.yml's Ubuntu 22.04 PipeWire PPA. No app/daemon/service install.
set -euo pipefail
umask 077

mesh_setup_mode="${1:?usage: mesh-control-setup-linux.sh library|captureless|host ABSOLUTE_TOOL_ROOT [cmake=VERSION]}"
mesh_setup_tools="${2:?supply the manager-owned task tool directory}"
mesh_setup_cmake="${3:-}"
case "$mesh_setup_mode" in library|captureless|host) ;; *) exit 2 ;; esac
[[ "$mesh_setup_tools" == /* ]] || { echo 'tool directory must be absolute' >&2; exit 2; }
[[ -r /etc/os-release ]] || { echo 'missing native distribution evidence' >&2; exit 2; }
# shellcheck disable=SC1091
. /etc/os-release
case "$ID" in ubuntu|debian) ;; *) echo 'recipe covers native Ubuntu/Debian only' >&2; exit 2 ;; esac
if [[ "$EUID" -eq 0 ]]; then
    mesh_setup_apt=(env DEBIAN_FRONTEND=noninteractive apt-get)
    mesh_setup_privilege=()
else
    mesh_setup_apt=(sudo -n env DEBIAN_FRONTEND=noninteractive apt-get)
    mesh_setup_privilege=(sudo -n)
fi
mesh_setup_apt+=(-o Acquire::Retries=5 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30)
if [[ "$mesh_setup_mode" == library && -n "$mesh_setup_cmake" ]]; then
    echo 'library setup does not use a CMake package argument' >&2
    exit 2
fi
if [[ "$mesh_setup_mode" != library ]]; then
    if [[ -z "$mesh_setup_cmake" && "$ID" == ubuntu && "$VERSION_ID" == 22.04 ]]; then
        # Manager's read-only apt-cache receipt observed this exact candidate.
        mesh_setup_cmake='cmake=3.22.1-1ubuntu1.22.04.2'
    fi
    [[ "$mesh_setup_cmake" =~ ^cmake=[0-9A-Za-z.+:~_-]+$ ]] || {
        echo 'supply an exact native apt-cache-confirmed cmake=VERSION for this distribution' >&2
        exit 2
    }
fi
if [[ "$mesh_setup_mode" == host && "$ID" == ubuntu && "$VERSION_ID" == 22.04 ]]; then
    # Same reviewed release recipe: stock 22.04 PipeWire headers are too old.
    command -v add-apt-repository >/dev/null || {
        echo 'reviewed 22.04 host setup requires existing add-apt-repository' >&2
        exit 2
    }
    "${mesh_setup_privilege[@]}" add-apt-repository -y --no-update ppa:pipewire-debian/pipewire-upstream
fi
"${mesh_setup_apt[@]}" update
# Subset of bootstrap.sh's existing Linux tool packages; no new dependency list.
"${mesh_setup_apt[@]}" install -y --no-install-recommends build-essential pkg-config curl
if [[ "$mesh_setup_mode" != library ]]; then
    "${mesh_setup_apt[@]}" install -y --no-install-recommends "$mesh_setup_cmake"
fi
if [[ "$mesh_setup_mode" == host ]]; then
    # Exact ci.yml Linux node media list, plus bootstrap.sh bindgen prerequisites.
    "${mesh_setup_apt[@]}" install -y --no-install-recommends \
        libasound2-dev libpipewire-0.3-dev libxkbcommon-dev \
        libgbm-dev libudev-dev libgtk-3-dev libwayland-dev \
        libavcodec-dev libavformat-dev libavutil-dev libavfilter-dev \
        libavdevice-dev libswscale-dev libswresample-dev \
        clang libclang-dev
fi
mkdir -p "$mesh_setup_tools"
export CARGO_HOME="$mesh_setup_tools/cargo"
export RUSTUP_HOME="$mesh_setup_tools/rustup"
export PATH="$CARGO_HOME/bin:$PATH"
if [[ ! -x "$CARGO_HOME/bin/rustup" ]]; then
    # Exact installer invocation from bootstrap.sh, with task-local tool homes.
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path
fi
# A PATH shim from another installation is not evidence that these private
# homes contain a toolchain. Populate and use only this task's rustup/shims.
"$CARGO_HOME/bin/rustup" toolchain install stable --component rustfmt --component clippy
"$CARGO_HOME/bin/rustup" show
uname -a
id
cat /etc/os-release
"$CARGO_HOME/bin/cargo" --version
"$CARGO_HOME/bin/rustc" -vV
cc --version
make --version
if [[ "$mesh_setup_mode" != library ]]; then
    cmake --version
fi
printf 'Subsequent runs must use CARGO_HOME=%s RUSTUP_HOME=%s and prepend %s/bin to PATH.\n' \
    "$CARGO_HOME" "$RUSTUP_HOME" "$CARGO_HOME"
printf 'Node runs require CMAKE_POLICY_VERSION_MINIMUM=3.5 (ci.yml).\n'
