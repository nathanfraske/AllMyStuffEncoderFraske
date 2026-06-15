// Network-settings envelope — the small JSON shape you export from one
// device and import on another so a network's full connection details
// (its handle + signaling / STUN / TURN servers) travel as a file instead
// of being re-typed by hand.
//
// Deliberately flatter than the daemon's on-disk `NetworkConfig`:
//
//   - `signaling_servers` is a string[] — each URL is one entry in
//     `SignalingCfg.servers`.
//   - `stun_servers` is a string[] — each URL becomes one `{ urls: [url] }`.
//   - `turn_servers` is `{ url, username?, credential? }[]` — each becomes
//     one `{ urls: [url], username, credential }`.
//
// The local `id` is NEVER in the envelope: dropping it lets the same file
// apply on many devices without colliding (each mints its own fresh id on
// import). Per-device governance choices (auto-approve, topology) are left
// out too — the file describes *how to reach* a network, not how you police
// it. A `kind` marker gates import so an unrelated JSON can't apply by
// accident; we also accept MyOwnMesh's marker since it's the same mesh.

import { buildNetworkConfig } from "./tauri";
import type { NetworkConfigFull } from "./types";

export const NETWORK_SETTINGS_KIND = "allmystuff.network-settings";
export const NETWORK_SETTINGS_VERSION = 1;

// A file exported from MyOwnMesh describes the very same network, so accept
// its marker too — importing it here Just Works.
const ACCEPTED_KINDS = new Set<string>([NETWORK_SETTINGS_KIND, "myownmesh.network-settings"]);

/** One TURN relay as it travels in the envelope (creds optional). */
export interface EnvelopeTurn {
  url: string;
  username?: string;
  credential?: string;
}

export interface NetworkSettingsExport {
  kind: string;
  version: number;
  network_id: string;
  /** Cosmetic label — optional, since the sender's name for it may not
   *  mean anything on the receiving device. */
  label?: string;
  signaling_servers: string[];
  stun_servers: string[];
  turn_servers: EnvelopeTurn[];
}

/** Flatten a daemon `NetworkConfigFull` into the shareable envelope —
 *  strips the local `id` and the urls-array nesting. */
export function exportNetworkSettings(cfg: NetworkConfigFull): NetworkSettingsExport {
  return {
    kind: NETWORK_SETTINGS_KIND,
    version: NETWORK_SETTINGS_VERSION,
    network_id: cfg.network_id,
    ...(cfg.label ? { label: cfg.label } : {}),
    signaling_servers: cfg.signaling?.servers ?? [],
    stun_servers: (cfg.stun_servers ?? []).flatMap((s) => s.urls),
    turn_servers: (cfg.turn_servers ?? []).map((t) => ({
      url: t.urls[0] ?? "",
      ...(t.username ? { username: t.username } : {}),
      ...(t.credential ? { credential: t.credential } : {}),
    })),
  };
}

/** Cheap shape check: does this parsed value carry a marker we accept and a
 *  network id? Field-level cleanup happens in `coerce`. */
export function isNetworkSettingsExport(raw: unknown): raw is NetworkSettingsExport {
  if (!raw || typeof raw !== "object") return false;
  const obj = raw as Record<string, unknown>;
  return typeof obj.kind === "string" && ACCEPTED_KINDS.has(obj.kind) && typeof obj.network_id === "string";
}

/** Parse a JSON string into an envelope, or null when it isn't JSON, isn't
 *  an object, or doesn't carry a marker we accept. Tolerant of malformed
 *  individual entries — "import a JSON" shouldn't fail over one bad URL. */
export function tryParseNetworkSettings(text: string): NetworkSettingsExport | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    return null;
  }
  if (!isNetworkSettingsExport(parsed)) return null;
  return coerce(parsed);
}

function coerce(raw: NetworkSettingsExport): NetworkSettingsExport {
  const strings = (v: unknown): string[] =>
    Array.isArray(v) ? v.filter((s): s is string => typeof s === "string") : [];
  const turn: EnvelopeTurn[] = Array.isArray(raw.turn_servers)
    ? raw.turn_servers
        .filter((t): t is EnvelopeTurn => !!t && typeof t === "object" && typeof t.url === "string")
        .map((t) => ({
          url: t.url,
          ...(typeof t.username === "string" && t.username ? { username: t.username } : {}),
          ...(typeof t.credential === "string" && t.credential ? { credential: t.credential } : {}),
        }))
    : [];
  return {
    kind: NETWORK_SETTINGS_KIND,
    version: NETWORK_SETTINGS_VERSION,
    network_id: String(raw.network_id ?? ""),
    ...(typeof raw.label === "string" && raw.label ? { label: raw.label } : {}),
    signaling_servers: strings(raw.signaling_servers),
    stun_servers: strings(raw.stun_servers),
    turn_servers: turn,
  };
}

/** Turn an imported envelope into the `mesh_network_add` payload, reusing the
 *  same builder Create/Join use — but with the file's servers instead of the
 *  defaults. Empty server lists fall back to the daemon's own defaults. */
export function networkAddPayloadFromEnvelope(
  env: NetworkSettingsExport,
  label?: string,
): Record<string, unknown> {
  return buildNetworkConfig({
    networkId: env.network_id,
    label: label ?? env.label,
    signaling: env.signaling_servers,
    stun: env.stun_servers,
    turn: env.turn_servers,
  });
}
