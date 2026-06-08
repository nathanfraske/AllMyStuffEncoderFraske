// A faithful TypeScript port of `allmystuff-graph`'s routing +
// authorization rules. The desktop app runs this client-side so the graph
// is fully interactive on its own; when the Tauri backend is present it
// validates the same way in Rust before anything touches the wire. Two
// implementations, one set of rules — kept honest by sharing the exact
// shapes in `types.ts`.

import type {
  Capability,
  Catalog,
  Flow,
  Grant,
  GrantRole,
  MediaKind,
  Route,
} from "./types";
import { MEDIA } from "./types";

export type ConnectResult =
  | { ok: true; route: Route }
  | { ok: false; reason: string; denied?: GrantRequest[] };

export interface GrantRequest {
  node: string;
  person: string;
  personName: string;
  media: MediaKind;
  role: GrantRole;
  capability: string | null;
  description: string;
}

export function capability(cat: Catalog, id: string): Capability | undefined {
  return cat.capabilities.find((c) => c.id === id);
}

export function mediaCompatible(a: MediaKind, b: MediaKind): boolean {
  return a === b || a === "generic" || b === "generic";
}

export function canSource(f: Flow): boolean {
  return f === "source" || f === "duplex";
}
export function canSink(f: Flow): boolean {
  return f === "sink" || f === "duplex";
}

function grantPermits(g: Grant, media: MediaKind, role: GrantRole, capId: string): boolean {
  if (!mediaCompatible(g.media, media)) return false;
  if (g.capability && g.capability !== capId) return false;
  if (role === "provide") return g.role === "provide" || g.role === "both";
  if (role === "consume") return g.role === "consume" || g.role === "both";
  return g.role === "both";
}

/** Returns a GrantRequest if the endpoint is on a shared node lacking
 *  coverage, otherwise null (mine, or already granted). */
function checkEndpoint(
  cat: Catalog,
  capId: string,
  media: MediaKind,
  role: GrantRole,
): GrantRequest | null {
  const cap = capability(cat, capId);
  if (!cap) return null;
  const node = cat.nodes.find((n) => n.id === cap.node);
  if (!node || node.relationship.kind !== "shared") return null;
  const share = node.relationship;
  const ok = share.grants.some((g) => grantPermits(g, media, role, capId));
  if (ok) return null;
  return {
    node: cap.node,
    person: share.person.id,
    personName: share.person.name,
    media,
    role,
    capability: capId,
    description: describeGrant(media, role),
  };
}

export function describeGrant(media: MediaKind, role: GrantRole): string {
  const m = MEDIA[media].label.toLowerCase();
  if (role === "provide") return `Send their ${m}`;
  if (role === "consume") return `Receive your ${m}`;
  return `Exchange ${m}`;
}

export function describeAction(media: MediaKind, role: GrantRole): string {
  const m = MEDIA[media].label.toLowerCase();
  if (role === "provide") return `send you their ${m}`;
  if (role === "consume") return `receive your ${m}`;
  return `exchange ${m} with you`;
}

export function requiredGrants(cat: Catalog, from: string, to: string): GrantRequest[] {
  const media = capability(cat, from)?.media ?? "generic";
  const out: GrantRequest[] = [];
  const a = checkEndpoint(cat, from, media, "provide");
  const b = checkEndpoint(cat, to, media, "consume");
  if (a) out.push(a);
  if (b) out.push(b);
  return out;
}

export function routeId(from: string, to: string): string {
  return `route:${from}→${to}`;
}

/** Validate + authorize a connection. Mirrors `Catalog::propose_route`. */
export function proposeRoute(cat: Catalog, from: string, to: string): ConnectResult {
  if (from === to) return { ok: false, reason: "A thing can't connect to itself." };
  const src = capability(cat, from);
  const dst = capability(cat, to);
  if (!src) return { ok: false, reason: `Unknown capability: ${from}` };
  if (!dst) return { ok: false, reason: `Unknown capability: ${to}` };
  if (!canSource(src.flow))
    return { ok: false, reason: `${src.label} can't send — it only receives.` };
  if (!canSink(dst.flow))
    return { ok: false, reason: `${dst.label} can't receive — it only sends.` };
  if (!mediaCompatible(src.media, dst.media))
    return {
      ok: false,
      reason: `${MEDIA[src.media].label} doesn't fit ${MEDIA[dst.media].label}.`,
    };

  const denied = requiredGrants(cat, from, to);
  if (denied.length > 0) {
    const who = denied[0].personName;
    const act = describeAction(denied[0].media, denied[0].role);
    return { ok: false, reason: `${who} isn't allowed to ${act} yet.`, denied };
  }

  const media = src.media !== "generic" ? src.media : dst.media;
  return { ok: true, route: { id: routeId(from, to), from, to, media, group: null } };
}

/** Fan a group out to a target node. Mirrors `Catalog::connect_group`:
 *  source members feed the target's sink, sink members are fed by its
 *  source, and an authorization failure aborts the whole connect. */
export function connectGroup(
  cat: Catalog,
  groupId: string,
  target: string,
):
  | { ok: true; routes: Route[] }
  | { ok: false; reason: string; denied?: GrantRequest[] } {
  const group = cat.groups.find((g) => g.id === groupId);
  if (!group) return { ok: false, reason: "Unknown group." };
  if (group.node === target) return { ok: false, reason: "That's the same place." };

  const routes: Route[] = [];
  for (const memberId of group.members) {
    const member = capability(cat, memberId);
    if (!member) continue;
    if (canSource(member.flow)) {
      const sink = matchEndpoint(cat, target, member.media, "consume");
      if (sink) {
        const r = proposeRoute(cat, memberId, sink.id);
        if (!r.ok) return { ok: false, reason: r.reason, denied: r.denied };
        routes.push({ ...r.route, group: groupId });
      }
    }
    if (canSink(member.flow)) {
      const src = matchEndpoint(cat, target, member.media, "provide");
      if (src) {
        const r = proposeRoute(cat, src.id, memberId);
        if (!r.ok) return { ok: false, reason: r.reason, denied: r.denied };
        routes.push({ ...r.route, group: groupId });
      }
    }
  }
  if (routes.length === 0)
    return { ok: false, reason: "Nothing on the target can pair with this group." };
  return { ok: true, routes };
}

const MACHINE_ORIGINS = new Set(["screen", "control", "system"]);

export function matchEndpoint(
  cat: Catalog,
  node: string,
  media: MediaKind,
  role: GrantRole,
): Capability | undefined {
  return cat.capabilities
    .filter((c) => c.node === node && mediaCompatible(c.media, media))
    .filter((c) => (role === "provide" ? canSource(c.flow) : canSink(c.flow)))
    .sort((a, b) => {
      const r = Number(!MACHINE_ORIGINS.has(a.origin)) - Number(!MACHINE_ORIGINS.has(b.origin));
      return r !== 0 ? r : a.id.localeCompare(b.id);
    })[0];
}
