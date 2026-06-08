// The single source of UI truth. Holds the graph catalog plus the
// transient interaction state (what's selected, what's being dragged,
// which sheet is open) as Svelte 5 runes, and exposes the verbs the
// components call. The connection rules live in `catalog.ts`; this layer
// is about *intent* and *feedback*.

import {
  canSink,
  canSource,
  connectGroup,
  matchEndpoint,
  proposeRoute,
  requiredGrants,
  type GrantRequest,
} from "./catalog";
import { demoCatalog } from "./mock";
import {
  connectRoute,
  disconnectRoute,
  onSession,
  onSubscription,
  scanSelf,
  type SessionSnapshot,
} from "./tauri";
import type { Capability, Catalog, Grant, MediaKind, MeshNode, Relationship } from "./types";

let seq = 0;
const newId = (p: string) => `${p}:${Date.now().toString(36)}:${seq++}`;

export interface Toast {
  id: number;
  kind: "ok" | "info" | "warn";
  text: string;
}

/** A connection the user started but that needs a permission grant first.
 *  Drives the "Let Alex…?" share sheet. */
export interface PendingShare {
  from: string;
  to: string;
  fromLabel: string;
  toLabel: string;
  requests: GrantRequest[];
}

/** A connection a group is about to make that needs permission. */
export interface PendingGroupShare {
  groupId: string;
  target: string;
  requests: GrantRequest[];
}

class AppStore {
  catalog = $state<Catalog>(demoCatalog());

  // ---- interaction state ------------------------------------------
  selectedNodeId = $state<string | null>(null);
  /** Capability the user is dragging a wire from, if any. */
  dragFrom = $state<string | null>(null);
  pendingShare = $state<PendingShare | null>(null);
  pendingGroupShare = $state<PendingGroupShare | null>(null);
  addNodeOpen = $state(false);
  manageShareNodeId = $state<string | null>(null);
  groupPickerFor = $state<string | null>(null); // groupId awaiting a target
  toasts = $state<Toast[]>([]);
  backendConnected = $state(false);
  /** The local machine's node id. `"this"` in demo/web mode; the real mesh
   *  device id once the backend session is up. The graph centres on it. */
  localId = $state("this");

  // ---- derived -----------------------------------------------------
  selectedNode = $derived(
    this.selectedNodeId ? this.catalog.nodes.find((n) => n.id === this.selectedNodeId) ?? null : null,
  );

  mineCount = $derived(this.catalog.nodes.filter((n) => n.relationship.kind === "mine").length);
  sharedCount = $derived(this.catalog.nodes.filter((n) => n.relationship.kind === "shared").length);

  capsOf(nodeId: string): Capability[] {
    return this.catalog.capabilities.filter((c) => c.node === nodeId);
  }

  node(nodeId: string): MeshNode | undefined {
    return this.catalog.nodes.find((n) => n.id === nodeId);
  }

  capability(id: string): Capability | undefined {
    return this.catalog.capabilities.find((c) => c.id === id);
  }

  // ---- lifecycle ---------------------------------------------------

  /** Wire up live backend data, if there is a backend. No-op (keeps the
   *  demo graph) in web mode. Called once on mount. */
  async init() {
    await this.hydrateFromBackend();
    await onSubscription((s) => {
      this.backendConnected = s.status === "live";
    });
    await onSession((snap) => this.applySessionSnapshot(snap));
  }

  /** Pull a real scan from the backend and re-home the local node onto its
   *  real mesh id + real devices. */
  async hydrateFromBackend() {
    const scan = await scanSelf();
    if (!scan) return;
    this.backendConnected = true;

    const oldId = this.localId;
    const newId = scan.node_id || "this";
    this.localId = newId;

    // Re-home the local node: drop its demo id + caps, adopt the real ones.
    const me = this.catalog.nodes.find((n) => n.id === oldId);
    if (me) {
      me.id = newId;
      me.kind = "this";
      me.summary = scan.summary;
    }
    this.catalog.capabilities = [
      ...scan.capabilities,
      ...this.catalog.capabilities.filter((c) => c.node !== oldId),
    ];
    // Demo groups referenced the old id; start live mode without them.
    if (newId !== oldId) this.catalog.groups = this.catalog.groups.filter((g) => g.node !== oldId);
    this.toast("ok", "Scanned this machine");
  }

  /** Merge a live session snapshot into the graph: presence peers become
   *  nodes (keeping any relationship the user already set), and live route
   *  states are reflected. */
  applySessionSnapshot(snap: SessionSnapshot) {
    if (!snap.ready) return;
    if (snap.me) this.localId = snap.me;

    for (const p of snap.peers ?? []) {
      let node = this.catalog.nodes.find((n) => n.id === p.node);
      if (!node) {
        // A freshly-discovered peer defaults to "mine" (it's on your mesh);
        // reclassify it as a guest from its drawer if it's someone else's.
        node = {
          id: p.node,
          label: p.label,
          kind: "machine",
          relationship: { kind: "mine" },
          online: true,
        };
        this.catalog.nodes.push(node);
      } else {
        node.label = p.label;
        node.online = true;
      }
      node.summary = p.summary;
      // Refresh this peer's capabilities.
      this.catalog.capabilities = [
        ...this.catalog.capabilities.filter((c) => c.node !== p.node),
        ...p.capabilities,
      ];
    }

    // Reflect live routes (active ones become catalog routes).
    for (const lr of snap.routes ?? []) {
      const active = lr.state.state === "active";
      const id = lr.route.id;
      const exists = this.catalog.routes.some((r) => r.id === id);
      if (active && !exists) {
        this.catalog.routes.push({ ...lr.route, group: null });
      } else if (!active && exists) {
        this.catalog.routes = this.catalog.routes.filter((r) => r.id !== id);
      }
    }
  }

  // ---- selection ---------------------------------------------------
  selectNode(id: string | null) {
    this.selectedNodeId = id;
  }

  // ---- connecting --------------------------------------------------

  /** Begin dragging a wire from a capability. The next node tapped on the
   *  graph becomes the destination (the "path dot" interaction). */
  startCapConnect(capId: string) {
    this.dragFrom = capId;
    const c = this.capability(capId);
    this.toast("info", `Connecting ${c?.label ?? "this"} — tap where it should go`);
  }

  cancelConnect() {
    this.dragFrom = null;
  }

  /** Finish a drag onto a node: auto-pick the matching endpoint there. */
  dropConnectOnNode(nodeId: string) {
    const capId = this.dragFrom;
    this.dragFrom = null;
    if (!capId) return;
    this.connectCapToNode(capId, nodeId);
  }

  /** Wire one capability to whichever endpoint on `nodeId` fits — a source
   *  reaches the node's matching sink, a sink is fed by its source. */
  connectCapToNode(capId: string, nodeId: string) {
    const cap = this.capability(capId);
    if (!cap) return;
    if (cap.node === nodeId) {
      this.toast("warn", "Pick a different device");
      return;
    }
    if (canSource(cap.flow)) {
      const sink = matchEndpoint(this.catalog, nodeId, cap.media, "consume");
      if (sink) return this.connect(capId, sink.id);
    }
    if (canSink(cap.flow)) {
      const src = matchEndpoint(this.catalog, nodeId, cap.media, "provide");
      if (src) return this.connect(src.id, capId);
    }
    const where = this.node(nodeId)?.label ?? "that device";
    this.toast("warn", `${where} has nowhere to put ${cap.label}`);
  }

  /** Try to wire `from` → `to`. On success the route appears; if it needs
   *  a shared person's permission, raises the share sheet instead. */
  connect(from: string, to: string) {
    const res = proposeRoute(this.catalog, from, to);
    if (res.ok) {
      this.addRoute(res.route.from, res.route.to);
      this.fireBackendConnect(res.route.from, res.route.to, res.route.media);
      const f = this.capability(from)?.label ?? from;
      const t = this.capability(to)?.label ?? to;
      this.toast("ok", `Connected ${f} → ${t}`);
      return;
    }
    if (res.denied && res.denied.length) {
      this.pendingShare = {
        from,
        to,
        fromLabel: this.capability(from)?.label ?? from,
        toLabel: this.capability(to)?.label ?? to,
        requests: res.denied,
      };
      return;
    }
    this.toast("warn", res.reason);
  }

  /** User approved the pending share: add exactly the requested grants,
   *  then complete the connection. */
  approvePendingShare() {
    const p = this.pendingShare;
    if (!p) return;
    for (const req of p.requests) this.grant(req.node, requestToGrant(req));
    const res = proposeRoute(this.catalog, p.from, p.to);
    if (res.ok) {
      this.addRoute(res.route.from, res.route.to);
      this.fireBackendConnect(res.route.from, res.route.to, res.route.media);
      this.toast("ok", `Shared — connected ${p.fromLabel} → ${p.toLabel}`);
    }
    this.pendingShare = null;
  }

  dismissPendingShare() {
    this.pendingShare = null;
  }

  /** When a real backend is connected, fire the actual mesh route offer.
   *  The backend's session snapshots then keep the route's live state in
   *  sync; in web mode this is a no-op and the local route stands in. */
  private fireBackendConnect(from: string, to: string, media: MediaKind) {
    if (this.backendConnected) void connectRoute(from, to, media);
  }

  private addRoute(from: string, to: string, group: string | null = null) {
    const cap = this.capability(from);
    const id = `route:${from}→${to}`;
    if (this.catalog.routes.some((r) => r.id === id)) return;
    this.catalog.routes.push({ id, from, to, media: cap?.media ?? "generic", group });
  }

  disconnect(routeId: string) {
    if (this.backendConnected) void disconnectRoute(routeId);
    const route = this.catalog.routes.find((r) => r.id === routeId);
    this.catalog.routes = this.catalog.routes.filter((r) => r.id !== routeId);
    // Tearing one leg of a group tears the whole bundle — it's one thing.
    if (route?.group) {
      this.catalog.routes = this.catalog.routes.filter((r) => r.group !== route.group);
      this.toast("info", "Disconnected the group");
    }
  }

  // ---- groups ------------------------------------------------------

  /** Begin pointing a group at a node — the next node click is the target. */
  startGroupConnect(groupId: string) {
    this.groupPickerFor = groupId;
    this.toast("info", "Pick where this group should go");
  }

  cancelGroupConnect() {
    this.groupPickerFor = null;
  }

  connectGroupTo(groupId: string, target: string) {
    const res = connectGroup(this.catalog, groupId, target);
    this.groupPickerFor = null;
    if (res.ok) {
      for (const r of res.routes) this.addRoute(r.from, r.to, groupId);
      const name = this.catalog.groups.find((g) => g.id === groupId)?.name ?? "group";
      const tlabel = this.node(target)?.label ?? target;
      this.toast("ok", `“${name}” is now using ${tlabel}`);
      return;
    }
    if (res.denied && res.denied.length) {
      this.pendingGroupShare = { groupId, target, requests: res.denied };
      return;
    }
    this.toast("warn", res.reason);
  }

  approveGroupShare() {
    const p = this.pendingGroupShare;
    if (!p) return;
    for (const req of p.requests) this.grant(req.node, requestToGrant(req));
    this.connectGroupTo(p.groupId, p.target);
    this.pendingGroupShare = null;
  }

  dismissGroupShare() {
    this.pendingGroupShare = null;
  }

  createGroup(name: string, node: string, members: string[]) {
    const id = newId("group");
    this.catalog.groups.push({ id, name, node, members });
    this.toast("ok", `Made the group “${name}”`);
    return id;
  }

  // ---- nodes + relationships --------------------------------------

  addNode(label: string, relationship: Relationship, summary?: MeshNode["summary"]) {
    const id = newId("node");
    this.catalog.nodes.push({
      id,
      label,
      kind: "machine",
      relationship,
      online: true,
      summary,
    });
    // A brand-new machine still exposes the synthetic trio.
    this.catalog.capabilities.push(
      { id: `${id}:screen`, node: id, label: "Screen", media: "display", flow: "source", origin: "screen" },
      { id: `${id}:control`, node: id, label: "Keyboard & mouse", media: "input", flow: "sink", origin: "control" },
      { id: `${id}:system-audio`, node: id, label: "System audio", media: "audio", flow: "duplex", origin: "system" },
    );
    this.addNodeOpen = false;
    this.selectNode(id);
    this.toast(
      "ok",
      relationship.kind === "mine"
        ? `Added ${label} to your stuff`
        : `Sharing with ${label}`,
    );
    return id;
  }

  /** Flip a node between "mine" and a fresh share, or vice versa. */
  setRelationship(nodeId: string, relationship: Relationship) {
    const n = this.node(nodeId);
    if (!n) return;
    n.relationship = relationship;
    this.reauthorize();
  }

  grant(nodeId: string, grant: Grant) {
    const n = this.node(nodeId);
    if (!n || n.relationship.kind !== "shared") return;
    // De-dupe by (media, role, capability).
    const exists = n.relationship.grants.some(
      (g) => g.media === grant.media && g.role === grant.role && g.capability === grant.capability,
    );
    if (!exists) n.relationship.grants.push(grant);
  }

  revokeGrant(nodeId: string, grantId: string) {
    const n = this.node(nodeId);
    if (!n || n.relationship.kind !== "shared") return;
    n.relationship.grants = n.relationship.grants.filter((g) => g.id !== grantId);
    this.reauthorize();
    this.toast("info", "Permission removed");
  }

  /** After any authorization change, drop routes that are no longer
   *  allowed. Security can't lag behind the grants. */
  private reauthorize() {
    const before = this.catalog.routes.length;
    this.catalog.routes = this.catalog.routes.filter(
      (r) => requiredGrants(this.catalog, r.from, r.to).length === 0,
    );
    const dropped = before - this.catalog.routes.length;
    if (dropped > 0) this.toast("warn", `${dropped} connection${dropped > 1 ? "s" : ""} stopped`);
  }

  // ---- toasts ------------------------------------------------------
  toast(kind: Toast["kind"], text: string) {
    const id = ++seq;
    this.toasts.push({ id, kind, text });
    setTimeout(() => {
      this.toasts = this.toasts.filter((t) => t.id !== id);
    }, 3200);
  }
}

function requestToGrant(req: GrantRequest): Grant {
  return {
    id: newId("grant"),
    media: req.media,
    role: req.role,
    capability: req.capability,
    label: req.description,
  };
}

export const app = new AppStore();

/** Re-export so components can build typed values without reaching into
 *  catalog.ts directly. */
export type { MediaKind };
