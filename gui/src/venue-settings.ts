// Venue file — the small JSON you export from one device and import on
// another (or host at a URL) so a venue travels as a file, exactly like a
// network-settings file does for a mesh.
//
// The export rule is the load-bearing bit: a *remote* venue (one with a
// `url`) exports as just `{ label, url }` — the pointer, not the servers,
// because the host must be online for the venue to work, and we want the
// importer to track the host's updates rather than freeze a snapshot. A
// *static* venue exports its actual `signaling_servers / stun_servers /
// turn_servers`.

import type { EnvelopeTurn } from "./network-settings";
import { newVenueId, type ServerSet, type Venue } from "./venues";

export const VENUE_SETTINGS_KIND = "allmystuff.venue";
export const VENUE_SETTINGS_VERSION = 1;

export interface VenueExport {
  kind: string;
  version: number;
  label: string;
  /** Remote venue: present instead of the server lists. */
  url?: string;
  /** Static venue: present instead of `url`. */
  signaling_servers?: string[];
  stun_servers?: string[];
  turn_servers?: EnvelopeTurn[];
}

/** Flatten a venue into the shareable envelope. Remote venues export their
 *  url (the host stays the source of truth); static venues export servers. */
export function exportVenue(v: Venue): VenueExport {
  if (v.url) {
    return { kind: VENUE_SETTINGS_KIND, version: VENUE_SETTINGS_VERSION, label: v.label, url: v.url };
  }
  return {
    kind: VENUE_SETTINGS_KIND,
    version: VENUE_SETTINGS_VERSION,
    label: v.label,
    signaling_servers: v.signaling.filter((s) => s.trim()),
    stun_servers: v.stun.filter((s) => s.trim()),
    turn_servers: v.turn
      .filter((t) => t.url.trim())
      .map((t) => ({
        url: t.url,
        ...(t.username ? { username: t.username } : {}),
        ...(t.credential ? { credential: t.credential } : {}),
      })),
  };
}

export function isVenueExport(raw: unknown): raw is VenueExport {
  if (!raw || typeof raw !== "object") return false;
  const o = raw as Record<string, unknown>;
  return o.kind === VENUE_SETTINGS_KIND && typeof o.label === "string";
}

function coerceTurn(raw: unknown): EnvelopeTurn[] {
  return Array.isArray(raw)
    ? raw
        .filter((t): t is EnvelopeTurn => !!t && typeof t === "object" && typeof (t as EnvelopeTurn).url === "string")
        .map((t) => ({
          url: t.url,
          ...(typeof t.username === "string" && t.username ? { username: t.username } : {}),
          ...(typeof t.credential === "string" && t.credential ? { credential: t.credential } : {}),
        }))
    : [];
}

const strings = (v: unknown): string[] =>
  Array.isArray(v) ? v.filter((s): s is string => typeof s === "string") : [];

/** Parse JSON text into a venue envelope, or null if it isn't one. */
export function tryParseVenue(text: string): VenueExport | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    return null;
  }
  if (!isVenueExport(parsed)) return null;
  const o = parsed as VenueExport;
  return {
    kind: VENUE_SETTINGS_KIND,
    version: VENUE_SETTINGS_VERSION,
    label: String(o.label ?? ""),
    ...(typeof o.url === "string" && o.url ? { url: o.url } : {}),
    signaling_servers: strings(o.signaling_servers),
    stun_servers: strings(o.stun_servers),
    turn_servers: coerceTurn(o.turn_servers),
  };
}

/** Build a fresh local venue from an imported envelope. A remote envelope
 *  yields a remote venue (servers empty until fetched); a static one carries
 *  its servers straight in. */
export function venueFromExport(env: VenueExport): Venue {
  return {
    id: newVenueId(),
    label: env.label || "Imported venue",
    ...(env.url ? { url: env.url } : {}),
    signaling: env.signaling_servers ?? [],
    stun: env.stun_servers ?? [],
    turn: (env.turn_servers ?? []).map((t) => ({
      url: t.url,
      username: t.username ?? "",
      credential: t.credential ?? "",
    })),
  };
}

/** Fetch a remote venue's current servers from its url. The host serves a
 *  venue file (static, with server lists); we take those. Throws on a network
 *  error or a non-venue response so callers can surface "venue offline". */
export async function fetchVenueServers(url: string): Promise<ServerSet> {
  const res = await fetch(url, { headers: { accept: "application/json" } });
  if (!res.ok) throw new Error(`venue fetch failed (${res.status})`);
  const env = tryParseVenue(await res.text());
  if (!env) throw new Error("not a venue file");
  return {
    signaling: env.signaling_servers ?? [],
    stun: env.stun_servers ?? [],
    turn: (env.turn_servers ?? []).map((t) => ({
      url: t.url,
      username: t.username ?? "",
      credential: t.credential ?? "",
    })),
  };
}

// ---- mesh invites --------------------------------------------------------
//
// A mesh's shareable handle used to be the bare network id — but rendezvous
// depends on both sides dialing the SAME signaling relays, so a handle joined
// on the wrong venue simply never meets its mesh, with no error anywhere. A
// compound invite (`handle#<venues>`) carries the venue(s) too, as base64url
// of the very envelopes Export/Import already speak.

/** Encode venues into an invite's `#` part. */
export function encodeInviteVenues(venues: Venue[]): string {
  const json = JSON.stringify(venues.map(exportVenue));
  const bytes = new TextEncoder().encode(json);
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

/** Decode an invite's `#` part back into venue envelopes. Null when the part
 *  isn't ours (a stray `#` in a pasted string must not eat the join). */
export function decodeInviteVenues(part: string): VenueExport[] | null {
  try {
    const b64 = part.replaceAll("-", "+").replaceAll("_", "/");
    const pad = b64 + "=".repeat((4 - (b64.length % 4)) % 4);
    const bytes = Uint8Array.from(atob(pad), (c) => c.charCodeAt(0));
    const parsed: unknown = JSON.parse(new TextDecoder().decode(bytes));
    if (!Array.isArray(parsed)) return null;
    // Normalize each entry through the same parser files go through.
    const envs = parsed
      .map((e) => tryParseVenue(JSON.stringify(e)))
      .filter((e): e is VenueExport => !!e);
    return envs.length === parsed.length && envs.length > 0 ? envs : null;
  } catch {
    return null;
  }
}
