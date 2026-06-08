// Thin bridge to the Tauri backend. Everything here degrades gracefully
// when the app runs as a plain web page (no Tauri) — `pnpm dev` in a
// browser, this repo's CI build — so the graph is always interactive even
// without the Rust side or a running `myownmesh` daemon.

import type { Capability, InventorySummary } from "./types";

interface ScanResult {
  summary: InventorySummary;
  capabilities: Capability[];
}

export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/** Invoke a backend command, or return null when there's no backend. */
async function tryInvoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T | null> {
  if (!isTauri()) return null;
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    return (await invoke(cmd, args)) as T;
  } catch (e) {
    console.warn(`backend command ${cmd} failed:`, e);
    return null;
  }
}

/** Scan this machine via the Rust `allmystuff-inventory` bridge. Returns
 *  null in web mode; the caller keeps its demo data. */
export function scanSelf(): Promise<ScanResult | null> {
  return tryInvoke<ScanResult>("scan_self");
}

/** Daemon status, when a mesh is running. */
export function meshStatus(): Promise<{ device_id: string; version: string } | null> {
  return tryInvoke("mesh_status");
}
