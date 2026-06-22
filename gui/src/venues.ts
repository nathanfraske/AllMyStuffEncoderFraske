// Venues — "where you call your mesh out."
//
// A venue is one named, reusable set of signaling / STUN / TURN servers. A
// mesh (network) uses one or more venues; its effective servers are the
// *union* of them, which AllMyStuff writes into the daemon's per-network
// config through the existing control API. The daemon has no notion of a
// venue — this concept lives entirely app-side (localStorage), exactly like
// the rooms list and the graph-view toggle.
//
// A venue is either:
//   - static  — servers authored locally (e.g. the built-in Public venue), or
//   - remote  — carrying a `url` it is fetched from, so whoever hosts it can
//               update the servers without anyone re-importing a file. A
//               remote venue exports as just its `url` (see venue-settings.ts):
//               the host must be online for the venue to function, so the
//               pointer *is* the truth.

import type { TurnEntry } from "./types";
import {
  MYOWNMESH_SIGNALING,
  MYOWNMESH_STUN,
  MYOWNMESH_TURN_URL,
  MYOWNMESH_TURN_USER,
  MYOWNMESH_TURN_PASS,
} from "./tauri";

export interface Venue {
  /** Stable local id. */
  id: string;
  /** Display name. */
  label: string;
  /** When set, this venue's servers are fetched from here and refreshable;
   *  it exports as just this url (the host must be online to function). */
  url?: string;
  signaling: string[];
  stun: string[];
  turn: TurnEntry[];
  /** The built-in Public venue — never deleted, never written to storage. */
  builtin?: boolean;
  /** Epoch ms of the last successful fetch, for remote venues. */
  fetchedAt?: number;
}

/** The flat server lists a network config wants. */
export interface ServerSet {
  signaling: string[];
  stun: string[];
  turn: TurnEntry[];
}

export const PUBLIC_VENUE_ID = "public-myownmesh";

/** The pinned, built-in venue: MyOwnMesh's shared reference servers. */
export function publicVenue(): Venue {
  return {
    id: PUBLIC_VENUE_ID,
    label: "Public (MyOwnMesh)",
    signaling: [MYOWNMESH_SIGNALING],
    stun: [MYOWNMESH_STUN],
    turn: [{ url: MYOWNMESH_TURN_URL, username: MYOWNMESH_TURN_USER, credential: MYOWNMESH_TURN_PASS }],
    builtin: true,
  };
}

/** Merge venues into one deduped server set — a mesh's effective servers.
 *  This is what lets multi-venue "just work" against today's engine: connect
 *  on the union of relays, and you find anyone sharing the beacon on any of
 *  them. (Multi-venue is teased now; the union is ready for when it ships.) */
export function unionServers(venues: Venue[]): ServerSet {
  const sig = new Set<string>();
  const stun = new Set<string>();
  const turn = new Map<string, TurnEntry>();
  for (const v of venues) {
    for (const s of v.signaling) if (s.trim()) sig.add(s.trim());
    for (const s of v.stun) if (s.trim()) stun.add(s.trim());
    for (const t of v.turn) {
      const url = t.url.trim();
      if (url) turn.set(url, { url, username: t.username, credential: t.credential });
    }
  }
  return { signaling: [...sig], stun: [...stun], turn: [...turn.values()] };
}

export function newVenueId(): string {
  return "venue-" + Math.random().toString(36).slice(2, 9);
}

const VENUES_KEY = "ams.venues.v1";

/** Load saved venues, always with the built-in Public venue present and first.
 *  Tolerant of malformed storage — a bad blob just yields the Public venue. */
export function loadVenues(): Venue[] {
  let saved: Venue[] = [];
  try {
    const raw = localStorage.getItem(VENUES_KEY);
    if (raw) {
      const parsed = JSON.parse(raw);
      if (Array.isArray(parsed)) {
        saved = parsed.filter(
          (v): v is Venue => !!v && typeof v.id === "string" && v.id !== PUBLIC_VENUE_ID && typeof v.label === "string",
        );
      }
    }
  } catch {
    /* ignore corrupt storage */
  }
  return [publicVenue(), ...saved];
}

/** Persist venues — the built-in Public venue is implicit and never written. */
export function saveVenues(venues: Venue[]): void {
  try {
    const keep = venues.filter((v) => !v.builtin && v.id !== PUBLIC_VENUE_ID);
    localStorage.setItem(VENUES_KEY, JSON.stringify(keep));
  } catch {
    /* ignore quota / unavailable storage */
  }
}

const NETWORK_VENUES_KEY = "ams.network-venues.v1";

/** Load the mesh→venue map (keyed by `network_id`, the portable wire id, so
 *  the choice survives re-adds and matches imported meshes). */
export function loadNetworkVenues(): Record<string, string[]> {
  try {
    const raw = localStorage.getItem(NETWORK_VENUES_KEY);
    if (raw) {
      const m = JSON.parse(raw) as unknown;
      if (m && typeof m === "object") {
        const out: Record<string, string[]> = {};
        for (const [k, v] of Object.entries(m as Record<string, unknown>)) {
          if (Array.isArray(v)) out[k] = v.filter((x): x is string => typeof x === "string");
        }
        return out;
      }
    }
  } catch {
    /* ignore corrupt storage */
  }
  return {};
}

export function saveNetworkVenues(map: Record<string, string[]>): void {
  try {
    localStorage.setItem(NETWORK_VENUES_KEY, JSON.stringify(map));
  } catch {
    /* ignore */
  }
}

const INACTIVE_VENUES_KEY = "ams.inactive-venues.v1";

/** Load the set of venues the user has explicitly switched **off** (by venue
 *  id). The off-list, not an on-list, so every venue is on by default and
 *  driving a mesh can only ever *remove* one from here — never add — matching
 *  "driving meshes turns venues on if needed, but only the user turns one off". */
export function loadInactiveVenues(): string[] {
  try {
    const raw = localStorage.getItem(INACTIVE_VENUES_KEY);
    if (raw) {
      const a = JSON.parse(raw) as unknown;
      if (Array.isArray(a)) return a.filter((x): x is string => typeof x === "string");
    }
  } catch {
    /* ignore corrupt storage */
  }
  return [];
}

export function saveInactiveVenues(ids: string[]): void {
  try {
    localStorage.setItem(INACTIVE_VENUES_KEY, JSON.stringify(ids));
  } catch {
    /* ignore */
  }
}
