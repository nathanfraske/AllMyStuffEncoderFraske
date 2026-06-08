#!/usr/bin/env bash
# SessionStart hook — warm the caches so `cargo test` and the GUI build
# are ready to run immediately in a Claude Code (web) session. Best-effort:
# never fail the session if the network or a tool is unavailable.
set -uo pipefail

echo "[allmystuff] warming Rust deps…"
cargo fetch --quiet 2>/dev/null || echo "[allmystuff] cargo fetch skipped"

if command -v pnpm >/dev/null 2>&1; then
  echo "[allmystuff] installing GUI deps…"
  (cd gui && pnpm install --silent 2>/dev/null) || echo "[allmystuff] pnpm install skipped"
fi

echo "[allmystuff] ready — try: just test · just scan · just caps"
exit 0
