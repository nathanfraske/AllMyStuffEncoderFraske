#!/usr/bin/env bash
# Manager-run setup only. See emulator-README.md for the official package sources.
set -euo pipefail
export LC_ALL=C

if [[ $# -gt 1 ]]; then
    echo "usage: bash emulator-setup-ubuntu.sh [exact-qemu-user-version]" >&2
    exit 2
fi
source /etc/os-release
if [[ ${ID:-} != ubuntu || ${VERSION_ID:-} != 24.04 ]]; then
    echo "This recipe is for Ubuntu 24.04 only." >&2
    exit 2
fi
if [[ $(dpkg --print-architecture) != amd64 || $(id -u) != 0 ]]; then
    echo "Run centrally as root in the reviewed amd64 Ubuntu distribution." >&2
    exit 2
fi

# Use configured signed repositories; do not add repositories or binfmt services.
apt-get update
apt-cache policy qemu-user
# Consume all policy output so pipefail cannot report SIGPIPE from an early exit.
package_version=${1:-$(apt-cache policy qemu-user | awk '/Candidate:/ {print $2}')}
if [[ -z $package_version || $package_version == '(none)' ]]; then
    echo "No qemu-user candidate in the configured repositories." >&2
    exit 1
fi
printf 'Selected qemu-user version: %s\n' "$package_version"
DEBIAN_FRONTEND=noninteractive apt-get install --yes --no-remove --no-install-recommends "qemu-user=$package_version"
dpkg-query -W -f='${Package} ${Version} ${Architecture}\n' qemu-user
qemu-riscv64 --version
# QEMU 8.2's CPU-list diagnostic exits 1; require the requested model as well.
cpu_help_status=0
cpu_help=$(qemu-riscv64 -cpu help) || cpu_help_status=$?
printf '%s\nCPU help exit status: %s\n' "$cpu_help" "$cpu_help_status"
if [[ $cpu_help_status != 0 && $cpu_help_status != 1 ]] ||
    ! awk '$0 == "rv64" { found = 1 } END { exit !found }' <<< "$cpu_help"; then
    echo "Unexpected CPU-help result or missing rv64 model." >&2
    exit 1
fi
sha256sum /usr/bin/qemu-riscv64
