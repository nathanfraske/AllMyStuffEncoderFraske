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
  buildNetworkConfig,
  claimNode,
  connectRoute,
  disconnectRoute,
  isTauri,
  meshIdentity,
  meshIdentitySetLabel,
  meshNetworkAdd,
  meshNetworkIdGenerate,
  meshNetworkRemove,
  meshNetworks,
  meshPeers,
  meshRosterApprove,
  meshRosterList,
  meshRosterRemove,
  onOwnership,
  onSession,
  onSubscription,
  scanSelf,
  setClaimable,
  type SessionSnapshot,
} from "./tauri";
import {
  BUNDLE_TEMPLATES,
  isAppNode,
  type Capability,
  type Catalog,
  type Flow,
  type Grant,
  type IdentityInfo,
  type MediaKind,
  type MeshNode,
  type NetworkSummary,
  type PeerInfo,
  type Relationship,
  type RosterPeer,
} from "./types";

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** A short, readable device id for labels when no friendly name is known. */
function shortId(id: string): string {
  return id.length > 12 ? `${id.slice(0, 10)}…` : id;
}

/** The stable machine identity inside a mesh device id: the bare pubkey,
 *  with MyOwnMesh's 5-char display suffix (`-AB12C`) stripped. The daemon's
 *  roster/peer list reports a device by its bare pubkey, while AllMyStuff
 *  presence and `IdentityShow` report the *display id* (`pubkey-SUFFIX`).
 *  Keying graph nodes by this canonical form collapses both views of one
 *  machine into a single node. Mirrors `myownmesh-core`'s
 *  `signing::pubkey_part` (suffix = 5 alphanumeric chars after the last `-`). */
function canonicalNodeId(id: string): string {
  const dash = id.lastIndexOf("-");
  if (dash > 0) {
    const suffix = id.slice(dash + 1);
    if (suffix.length === 5 && /^[0-9a-zA-Z]+$/.test(suffix)) return id.slice(0, dash);
  }
  return id;
}

/** Whether two mesh ids name the same machine (same pubkey, any suffix). */
function sameMachine(a: string, b: string): boolean {
  return canonicalNodeId(a) === canonicalNodeId(b);
}

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

/** A graph with nothing in it — the starting point under the real backend,
 *  where every node + capability comes from the live scan and mesh presence
 *  (no demo stand-ins). */
function emptyCatalog(): Catalog {
  return { nodes: [], capabilities: [], routes: [], groups: [] };
}

class AppStore {
  // Under the real app the graph is built entirely from the live scan + mesh
  // presence, so it starts empty and fills with *your* stuff. The demo
  // catalog is only a stand-in for the browser/preview build (no Tauri
  // backend) so the marketing page is never blank.
  catalog = $state<Catalog>(isTauri() ? emptyCatalog() : demoCatalog());

  // ---- interaction state ------------------------------------------
  selectedNodeId = $state<string | null>(null);
  /** Capability the user is dragging a wire from, if any. */
  dragFrom = $state<string | null>(null);
  pendingShare = $state<PendingShare | null>(null);
  pendingGroupShare = $state<PendingGroupShare | null>(null);
  /** The "Add a machine" onboarding sheet (real machines join the mesh; you
   *  don't fabricate them). */
  addMachineOpen = $state(false);
  /** The Networks panel (identity, create/join/leave, approvals). */
  networksOpen = $state(false);
  manageShareNodeId = $state<string | null>(null);
  groupPickerFor = $state<string | null>(null); // groupId awaiting a target
  toasts = $state<Toast[]>([]);
  backendConnected = $state(false);

  // ---- remote console (the pikvm-style session popup) -------------
  /** The remote machine a console session is open on, if any. */
  consoleNodeId = $state<string | null>(null);
  /** The video input (a remote display/camera source) currently selected in
   *  the console's input tab bar. */
  consoleInput = $state<string | null>(null);
  /** Whether audio passthrough is on for the session. */
  consoleAudio = $state(false);
  /** Whether keyboard & mouse control is being sent to the remote. */
  consoleControl = $state(false);
  // Route ids the console owns, by channel, so it tears down exactly what it
  // set up (and nothing a different connection made).
  private consoleVideoRouteId: string | null = null;
  private consoleAudioRouteIds: string[] = [];
  private consoleControlRouteId: string | null = null;
  /** The local machine's node id. `"this"` in demo/web mode; the real mesh
   *  device id once the backend session is up. The graph centres on it. */
  localId = $state("this");

  // ---- networks / identity / roster (live mesh control) -----------
  /** This device's mesh identity. `label` is the display-name override. */
  identity = $state<IdentityInfo | null>(null);
  networks = $state<NetworkSummary[]>([]);
  /** config_id of the network the live session is currently on. */
  sessionNetwork = $state<string | null>(null);
  /** The network whose roster/approvals the Networks panel is showing. */
  rosterNetwork = $state<string | null>(null);
  roster = $state<RosterPeer[]>([]);
  livePeers = $state<PeerInfo[]>([]);

  // ---- bundles (pre-set kits with category slots) -----------------
  /** The bundle template id currently being filled, if any. */
  bundleDraftId = $state<string | null>(null);
  /** slot id → chosen local capability id, for the draft bundle. */
  bundleSlots = $state<Record<string, string>>({});

  /** Safety-net poll that keeps the graph's mesh members fresh. */
  private meshPoll: ReturnType<typeof setInterval> | null = null;

  // ---- derived -----------------------------------------------------
  selectedNode = $derived(
    this.selectedNodeId ? this.catalog.nodes.find((n) => n.id === this.selectedNodeId) ?? null : null,
  );

  /** The machine a console session is currently open on, if any. */
  consoleNode = $derived(
    this.consoleNodeId ? this.catalog.nodes.find((n) => n.id === this.consoleNodeId) ?? null : null,
  );

  mineCount = $derived(this.catalog.nodes.filter((n) => n.relationship.kind === "mine").length);
  sharedCount = $derived(this.catalog.nodes.filter((n) => n.relationship.kind === "shared").length);

  /** The network the live session is on (or the first one configured).
   *  Guarded against a non-array `networks` — this derived renders in the top
   *  bar every frame, so a bad backend shape here would wedge the whole UI. */
  activeNetwork = $derived.by(() => {
    const nets = Array.isArray(this.networks) ? this.networks : [];
    return nets.find((n) => n.config_id === this.sessionNetwork) ?? nets[0] ?? null;
  });
  /** Devices waiting to be let onto the roster network. */
  pendingPeers = $derived(
    (Array.isArray(this.livePeers) ? this.livePeers : []).filter(
      (p) => p.status === "pending_approval",
    ),
  );

  capsOf(nodeId: string): Capability[] {
    return this.catalog.capabilities.filter((c) => c.node === nodeId);
  }

  node(nodeId: string): MeshNode | undefined {
    return this.catalog.nodes.find((n) => n.id === nodeId);
  }

  /** Find the node representing the same machine as `id`, preferring an
   *  exact match (so a presence advert lands on its own node) and falling
   *  back to the canonical pubkey (so the daemon's bare-pubkey view and the
   *  presence display-id view of one machine resolve to a single node). */
  private nodeByCanonical(id: string): MeshNode | undefined {
    return (
      this.catalog.nodes.find((n) => n.id === id) ??
      this.catalog.nodes.find((n) => sameMachine(n.id, id))
    );
  }

  capability(id: string): Capability | undefined {
    return this.catalog.capabilities.find((c) => c.id === id);
  }

  // ---- lifecycle ---------------------------------------------------

  /** Wire up live backend data, if there is a backend. No-op (keeps the
   *  demo graph) in web mode. Called once on mount. */
  async init() {
    await this.hydrateFromBackend();
    await this.loadIdentity();
    await this.refreshNetworks();
    await this.syncMeshGraph();
    this.startMeshPolling();
    await onSubscription((s) => {
      const live = s.status === "live";
      // When the mesh comes up, re-scan + reload networks/identity: the first
      // pass at mount can run before the session is ready.
      if (live) {
        void this.hydrateFromBackend();
        void this.loadIdentity();
        void this.refreshNetworks().then(() => this.syncMeshGraph());
      }
      this.backendConnected = live;
    });
    await onSession((snap) => this.applySessionSnapshot(snap));
    await onOwnership((o) => {
      const who = this.node(o.from)?.label ?? "A device";
      if (o.message.kind === "claimed") this.toast("ok", `${who} is yours now`);
      else if (o.message.kind === "declined")
        this.toast("warn", `Couldn't claim ${who}: ${o.message.reason ?? "not claimable"}`);
    });
  }

  /** Poll the daemon's mesh membership as a safety net (peer/roster changes
   *  don't all arrive as session snapshots). Mirrors the MyOwnMesh client. */
  private startMeshPolling() {
    if (!isTauri() || this.meshPoll) return;
    this.meshPoll = setInterval(() => void this.syncMeshGraph(), 3000);
  }

  /** Build the graph's machine nodes from the daemon's *actual* mesh
   *  membership — the roster of known devices plus currently-live peers,
   *  across every joined network. This is what makes "others on the mesh"
   *  appear; the bespoke presence channel only layers device detail on top
   *  when a peer also runs AllMyStuff. Mirrors how MyOwnMesh builds its map. */
  async syncMeshGraph() {
    if (!isTauri()) return;
    // Live peers are keyed by the full device id (`{pubkey}-{suffix}`) — the
    // same id presence + capabilities use, so they merge into one node.
    const live = new Map<string, { label: string; online: boolean }>();
    const rosterAll: RosterPeer[] = [];
    const nets = Array.isArray(this.networks) ? this.networks : [];
    for (const net of nets) {
      let peers: PeerInfo[] = [];
      let roster: RosterPeer[] = [];
      try {
        peers = await meshPeers(net.config_id);
      } catch {
        /* network still settling */
      }
      try {
        roster = await meshRosterList(net.config_id);
      } catch {
        /* roster optional */
      }
      for (const p of peers) {
        if (p.status === "pending_approval") continue; // shown under approvals
        const e = live.get(p.device_id) ?? { label: p.label?.trim() || shortId(p.device_id), online: false };
        if (p.label?.trim()) e.label = p.label.trim();
        if (p.status === "active") e.online = true;
        live.set(p.device_id, e);
      }
      rosterAll.push(...roster);
    }
    // The roster stores the bare pubkey; a live peer's id is `{pubkey}-{suffix}`.
    // Only surface a roster entry as its own offline node when no live peer
    // already covers that pubkey — otherwise one machine would show twice.
    const liveIds = [...live.keys()];
    const known = new Map(live);
    for (const r of rosterAll) {
      const covered = liveIds.some((id) => id === r.device_id || id.startsWith(`${r.device_id}-`));
      if (covered || known.has(r.device_id)) continue;
      known.set(r.device_id, { label: r.label?.trim() || shortId(r.device_id), online: false });
    }
    // Upsert a node per known device (never the local machine). Discovered
    // devices start *unclaimed* — they're on the mesh but not yet yours; you
    // claim them (only if they offer it) or mark them shared from their
    // drawer. A device known only from the daemon's roster/peers isn't
    // running AllMyStuff yet (`app: false`) — presence is what flips that on,
    // so we never downgrade a node the bespoke channel already enriched.
    for (const [id, info] of known) {
      if (sameMachine(id, this.localId)) continue;
      // The daemon reports the bare pubkey; a presence advert may already
      // have created this machine's node under its display id. Resolve by
      // canonical pubkey so we update that one node rather than spawning a
      // bare-pubkey twin that reads as "not on AllMyStuff".
      const node = this.nodeByCanonical(id);
      if (!node) {
        this.catalog.nodes.push({
          id,
          label: info.label,
          kind: "machine",
          relationship: { kind: "unclaimed" },
          online: info.online,
          app: false,
        });
      } else {
        node.online = info.online;
        if (!node.hostname && info.label) node.label = info.label;
      }
    }
    // A machine that's no longer in any roster/peer set has dropped offline.
    // Compare by canonical pubkey so a presence node (display id) isn't wrongly
    // marked offline just because the daemon lists it under the bare pubkey.
    const knownCanon = new Set([...known.keys()].map(canonicalNodeId));
    for (const n of this.catalog.nodes) {
      if (
        n.kind !== "this" &&
        !sameMachine(n.id, this.localId) &&
        !knownCanon.has(canonicalNodeId(n.id))
      ) {
        n.online = false;
      }
    }
  }

  /** Pull a real scan from the backend and re-home the local node onto its
   *  real mesh id + real devices. */
  async hydrateFromBackend() {
    const scan = await scanSelf();
    if (!scan) return;
    this.backendConnected = true;

    const prevId = this.localId;
    const newId = scan.node_id || "this";
    this.localId = newId;

    // Adopt this machine as "this device". Match the local node by its new
    // id or its previous one, so a re-scan (once the mesh id is known)
    // re-homes the same node rather than adding a duplicate.
    const host = scan.hostname || scan.label || "This device";
    // Display name follows the naming rule: the override if the user set one,
    // else the machine hostname.
    const label = this.identity?.label?.trim() || host;
    const me =
      this.catalog.nodes.find((n) => n.id === newId) ??
      this.catalog.nodes.find((n) => n.id === prevId && n.kind === "this");
    if (me) {
      me.id = newId;
      me.kind = "this";
      me.label = label;
      me.hostname = host;
      me.summary = scan.summary;
      me.app = true;
    } else {
      this.catalog.nodes.push({
        id: newId,
        label,
        hostname: host,
        kind: "this",
        relationship: { kind: "mine" },
        online: true,
        app: true,
        summary: scan.summary,
      });
    }
    // If an early daemon poll (before we knew our real id) added this same
    // machine as a bare-pubkey peer node, drop that twin now.
    this.catalog.nodes = this.catalog.nodes.filter(
      (n) => n.kind === "this" || !sameMachine(n.id, newId),
    );
    // Local capabilities are exactly what the scan reports; drop any tied to
    // the old or new local id so a re-scan replaces rather than accumulates.
    this.catalog.capabilities = [
      ...scan.capabilities,
      ...this.catalog.capabilities.filter((c) => c.node !== newId && c.node !== prevId),
    ];
    this.toast("ok", "Scanned this machine");
  }

  /** Merge a live session snapshot into the graph: presence peers become
   *  nodes (keeping any relationship the user already set), and live route
   *  states are reflected. */
  applySessionSnapshot(snap: SessionSnapshot) {
    if (!snap.ready) return;
    if (snap.me) this.localId = snap.me;
    if (snap.network !== undefined) this.sessionNetwork = snap.network ?? null;

    for (const p of snap.peers ?? []) {
      // Resolve by canonical pubkey so presence lands on the same node the
      // daemon's roster/peer view created (the bare-pubkey "not on AllMyStuff"
      // twin), rather than a second node keyed by the display id.
      let node = this.nodeByCanonical(p.node);
      if (!node) {
        // A freshly-discovered peer starts unclaimed — claim it (only if it
        // offers itself) or mark it shared from its drawer.
        node = {
          id: p.node,
          label: p.label,
          hostname: p.hostname,
          kind: "machine",
          relationship: { kind: "unclaimed" },
          online: true,
        };
        this.catalog.nodes.push(node);
      } else {
        // Adopt the presence display id so this peer's capabilities (keyed by
        // `p.node`) resolve to this node.
        if (node.id !== p.node) node.id = p.node;
        node.label = p.label;
        node.hostname = p.hostname;
        node.online = true;
      }
      node.summary = p.summary;
      // Presence means it's running AllMyStuff — it has wireable stuff.
      node.app = true;
      // Ownership the device advertises about itself (Task 4).
      node.owner = p.owner ?? null;
      node.claimable = p.claimable ?? false;
      // A device that says *we* own it is ours; one owned by someone else
      // stays a guest/unclaimed (you can't flat-claim it). Never auto-flip a
      // relationship the user already set, and never auto-adopt.
      if (p.owner && sameMachine(p.owner, this.localId) && node.relationship.kind === "unclaimed") {
        node.relationship = { kind: "mine" };
      }
      // Collapse any other view of this same machine into the one node we just
      // settled on — heals an already-split graph.
      this.catalog.nodes = this.catalog.nodes.filter(
        (n) => n === node || !sameMachine(n.id, p.node),
      );
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
    const cap = this.capability(capId);
    // Dragging a remote machine's *screen* onto this device is the "watch /
    // control that machine here" gesture — open its console rather than just
    // drawing a wire.
    if (cap && cap.origin === "screen" && cap.node !== this.localId && nodeId === this.localId) {
      this.openConsole(cap.node);
      return;
    }
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
    // A device on the mesh that isn't running AllMyStuff has nothing to wire
    // to — keep it un-targetable (Task 1).
    const target = this.node(nodeId);
    if (target && !isAppNode(target)) {
      this.toast("warn", `${target.label} isn't running AllMyStuff`);
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

  // ---- remote console (the pikvm-style session) -------------------

  /** A remote machine's video-capable sources — its screen plus any cameras
   *  — ordered so the screen leads and the default sits near the front. This
   *  is the console's "video inputs" tab bar. */
  consoleVideoInputs(nodeId: string): Capability[] {
    return this.capsOf(nodeId)
      .filter((c) => (c.media === "display" || c.media === "video") && canSource(c.flow))
      .sort((a, b) => {
        const rank = (c: Capability) => (c.origin === "screen" ? 0 : c.default ? 1 : 2);
        return rank(a) - rank(b) || a.id.localeCompare(b.id);
      });
  }

  /** Open a console session on a remote machine — the single handle for its
   *  screen, its audio passthrough and keyboard/mouse control. Wires the
   *  backbone video route to this machine's display now; audio and control
   *  are toggled from inside the console. */
  openConsole(nodeId: string) {
    const node = this.node(nodeId);
    if (!node) return;
    if (nodeId === this.localId) {
      this.toast("warn", "That's this device");
      return;
    }
    if (!isAppNode(node)) {
      this.toast("warn", `${node.label} isn't running AllMyStuff`);
      return;
    }
    if (node.relationship.kind === "unclaimed") {
      this.toast("warn", `Claim ${node.label} first, or mark it shared`);
      return;
    }
    this.consoleNodeId = nodeId;
    this.consoleAudio = false;
    this.consoleControl = false;
    this.consoleVideoRouteId = null;
    this.consoleAudioRouteIds = [];
    this.consoleControlRouteId = null;
    this.consoleInput = this.consoleVideoInputs(nodeId)[0]?.id ?? null;
    this.applyConsoleVideo();
    this.toast("ok", `Console open on ${node.label}`);
  }

  /** Close the console, tearing down exactly the routes it created. */
  closeConsole() {
    if (this.consoleVideoRouteId) this.disconnect(this.consoleVideoRouteId);
    for (const id of this.consoleAudioRouteIds) this.disconnect(id);
    if (this.consoleControlRouteId) this.disconnect(this.consoleControlRouteId);
    this.consoleVideoRouteId = null;
    this.consoleAudioRouteIds = [];
    this.consoleControlRouteId = null;
    this.consoleNodeId = null;
    this.consoleInput = null;
    this.consoleAudio = false;
    this.consoleControl = false;
  }

  /** Switch which remote source the console is showing. */
  setConsoleInput(capId: string) {
    this.consoleInput = capId;
    this.applyConsoleVideo();
  }

  private applyConsoleVideo() {
    if (this.consoleVideoRouteId) {
      this.disconnect(this.consoleVideoRouteId);
      this.consoleVideoRouteId = null;
    }
    const inp = this.consoleInput ? this.capability(this.consoleInput) : null;
    if (!inp) return;
    // The remote screen (display) lands on this machine's display sink — a
    // real route. A camera (video) has no local sink yet, so it's view-only
    // until video transport lands; the console is honest about that.
    const sink = matchEndpoint(this.catalog, this.localId, inp.media, "consume");
    if (!sink) return;
    const leg = this.consoleConnect(inp.id, sink.id);
    // Only own the route for teardown if this call created it.
    this.consoleVideoRouteId = leg?.created ? leg.id : null;
  }

  /** Audio passthrough: hear the remote *and* send it your audio. */
  toggleConsoleAudio() {
    const remote = this.consoleNodeId;
    if (!remote) return;
    if (this.consoleAudio) {
      for (const id of this.consoleAudioRouteIds) this.disconnect(id);
      this.consoleAudioRouteIds = [];
      this.consoleAudio = false;
      return;
    }
    // Two legs: hear the remote, and send it your audio. The channel reads
    // as on when either leg is live; only legs this call created are owned
    // for teardown.
    const owned: string[] = [];
    let anyLive = false;
    const legs: Array<[Capability | undefined, Capability | undefined]> = [
      [matchEndpoint(this.catalog, remote, "audio", "provide"), matchEndpoint(this.catalog, this.localId, "audio", "consume")],
      [matchEndpoint(this.catalog, this.localId, "audio", "provide"), matchEndpoint(this.catalog, remote, "audio", "consume")],
    ];
    for (const [from, to] of legs) {
      if (!from || !to) continue;
      const leg = this.consoleConnect(from.id, to.id);
      if (!leg) continue;
      anyLive = true;
      if (leg.created) owned.push(leg.id);
    }
    this.consoleAudioRouteIds = owned;
    this.consoleAudio = anyLive;
    if (!anyLive) this.toast("warn", "No audio path to that machine");
  }

  /** Send this machine's keyboard & mouse to the remote (input injection on
   *  the far side is a follow-up; the route is real and shows active). */
  toggleConsoleControl() {
    const remote = this.consoleNodeId;
    if (!remote) return;
    if (this.consoleControl) {
      if (this.consoleControlRouteId) this.disconnect(this.consoleControlRouteId);
      this.consoleControlRouteId = null;
      this.consoleControl = false;
      return;
    }
    const mySrc = matchEndpoint(this.catalog, this.localId, "input", "provide");
    const remoteSink = matchEndpoint(this.catalog, remote, "input", "consume");
    const leg = mySrc && remoteSink ? this.consoleConnect(mySrc.id, remoteSink.id) : null;
    if (leg) {
      this.consoleControlRouteId = leg.created ? leg.id : null;
      this.consoleControl = true;
    } else {
      this.toast("warn", "No control path to that machine");
    }
  }

  /** Connect a console leg through the normal route path (so authorization
   *  and the backend offer still apply). Returns the route id when it's now
   *  live, and whether *this* call created it — so the console reads the
   *  channel as on only when something is actually wired, and tears down only
   *  the routes it made (never a pre-existing one the user set up, and never
   *  a leg that was blocked behind a share prompt). */
  private consoleConnect(from: string, to: string): { id: string; created: boolean } | null {
    const id = `route:${from}→${to}`;
    const existedBefore = this.catalog.routes.some((r) => r.id === id);
    this.connect(from, to);
    const existsNow = this.catalog.routes.some((r) => r.id === id);
    if (!existsNow) return null; // blocked / denied — nothing got wired
    return { id, created: !existedBefore };
  }

  // ---- ownership / claiming ---------------------------------------

  /** Adopt a device as one of yours. Honours Task 4: this only works when
   *  the device is *claimable* (booted in claim mode, still unowned) or
   *  already says you own it — you can't flat-out take a box that has an
   *  owner or was never offered. */
  claim(nodeId: string) {
    const n = this.node(nodeId);
    if (!n) return;
    if (n.owner && n.owner !== this.localId) {
      this.toast("warn", `${n.label} is owned by another device — you can't take it`);
      return;
    }
    if (!n.claimable && n.owner !== this.localId) {
      this.toast(
        "warn",
        `${n.label} isn't in claim mode. Start it claimable on the device itself to adopt it.`,
      );
      return;
    }
    if (this.backendConnected) {
      // The device confirms by re-advertising presence with owner = us.
      void claimNode(nodeId);
      this.toast("info", `Asking ${n.label} to join your fleet…`);
    } else {
      // Demo/web: the claimable device accepts.
      n.owner = this.localId;
      n.claimable = false;
      n.relationship = { kind: "mine" };
      this.toast("ok", `${n.label} is yours now`);
      this.reauthorize();
    }
  }

  /** Put *this* device into (or out of) claim mode so another of your
   *  machines can adopt it. */
  async setLocalClaimable(on: boolean) {
    const me = this.node(this.localId);
    if (this.backendConnected) {
      try {
        const now = await setClaimable(on);
        if (me) me.claimable = now ?? on;
        this.toast(
          on ? "info" : "ok",
          on ? "This device can now be adopted by another of your machines" : "Adoption turned off",
        );
      } catch (e) {
        this.toast("warn", `Couldn't change claim mode: ${errMsg(e)}`);
      }
    } else {
      if (me) me.claimable = on;
      this.toast("info", on ? "Adoption on (demo)" : "Adoption off (demo)");
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

  // ---- bundles (pre-set kits with category slots) -----------------

  /** Start filling a bundle template — auto-fill each slot from this
   *  machine's matching devices; the user can swap any of them. */
  startBundle(templateId: string) {
    const tpl = BUNDLE_TEMPLATES.find((t) => t.id === templateId);
    if (!tpl) return;
    this.bundleDraftId = templateId;
    const slots: Record<string, string> = {};
    for (const slot of tpl.slots) {
      const role = slot.flow === "source" ? "provide" : "consume";
      const cap = matchEndpoint(this.catalog, this.localId, slot.media, role);
      if (cap) slots[slot.id] = cap.id;
    }
    this.bundleSlots = slots;
  }

  setBundleSlot(slotId: string, capId: string) {
    this.bundleSlots = { ...this.bundleSlots, [slotId]: capId };
  }

  cancelBundle() {
    this.bundleDraftId = null;
    this.bundleSlots = {};
  }

  /** This machine's capabilities that fit a slot (same media + direction). */
  bundleCandidates(slot: { media: MediaKind; flow: Flow }): Capability[] {
    const wantSource = slot.flow === "source";
    return this.capsOf(this.localId).filter(
      (c) => c.media === slot.media && (wantSource ? canSource(c.flow) : canSink(c.flow)),
    );
  }

  /** Turn the filled draft into a bundle and arm the "tap a machine to send
   *  it there" picker — it fans out as one connection. */
  sendBundle() {
    const tpl = BUNDLE_TEMPLATES.find((t) => t.id === this.bundleDraftId);
    if (!tpl) return;
    const members = Object.values(this.bundleSlots).filter(Boolean);
    if (members.length === 0) {
      this.toast("warn", "Fill at least one slot first");
      return;
    }
    const groupId = this.createGroup(tpl.name, this.localId, members);
    this.cancelBundle();
    this.startGroupConnect(groupId);
  }

  // ---- networks / identity / roster -------------------------------

  async loadIdentity() {
    if (!isTauri()) return;
    try {
      this.identity = await meshIdentity();
      this.applyLocalLabel();
    } catch {
      /* no daemon yet — the graph still works from the demo/scan */
    }
  }

  /** Re-apply the naming rule to the local node after identity changes. */
  private applyLocalLabel() {
    const me = this.node(this.localId);
    if (!me) return;
    const host = me.hostname ?? me.label;
    me.label = this.identity?.label?.trim() || host;
  }

  async refreshNetworks() {
    if (!isTauri()) return;
    try {
      this.networks = (await meshNetworks()) ?? [];
      if (this.rosterNetwork) await this.refreshRoster(this.rosterNetwork);
    } catch (e) {
      this.toast("warn", `Couldn't load networks: ${errMsg(e)}`);
    }
  }

  /** Set this device's display-name override (empty resets to the hostname). */
  async setIdentityLabel(label: string) {
    try {
      await meshIdentitySetLabel(label);
      this.identity = { device_id: this.identity?.device_id ?? "", label };
      this.applyLocalLabel();
      this.toast("ok", label.trim() ? "Updated this device's name" : "Reset to the machine name");
    } catch (e) {
      this.toast("warn", `Couldn't set name: ${errMsg(e)}`);
    }
  }

  async createNetwork(label?: string, autoApprove = false): Promise<string | null> {
    try {
      const networkId = await meshNetworkIdGenerate();
      await meshNetworkAdd(buildNetworkConfig({ networkId, label, autoApprove }));
      this.toast("ok", `Created network ${label?.trim() || networkId}`);
      await this.refreshNetworks();
      return networkId;
    } catch (e) {
      this.toast("warn", `Couldn't create network: ${errMsg(e)}`);
      return null;
    }
  }

  async joinNetwork(networkId: string, label?: string) {
    const id = networkId.trim();
    if (!id) return;
    try {
      await meshNetworkAdd(buildNetworkConfig({ networkId: id, label }));
      this.toast("ok", `Joined ${label?.trim() || id}`);
      await this.refreshNetworks();
    } catch (e) {
      this.toast("warn", `Couldn't join: ${errMsg(e)}`);
    }
  }

  async leaveNetwork(configId: string) {
    try {
      await meshNetworkRemove(configId);
      if (this.rosterNetwork === configId) {
        this.rosterNetwork = null;
        this.roster = [];
        this.livePeers = [];
      }
      this.toast("info", "Left the network");
      await this.refreshNetworks();
    } catch (e) {
      this.toast("warn", `Couldn't leave: ${errMsg(e)}`);
    }
  }

  /** Load the roster + live peers for one network (the approvals view). */
  async refreshRoster(configId: string) {
    this.rosterNetwork = configId;
    try {
      this.roster = (await meshRosterList(configId)) ?? [];
    } catch {
      this.roster = [];
    }
    try {
      this.livePeers = (await meshPeers(configId)) ?? [];
    } catch {
      this.livePeers = [];
    }
  }

  async approveDevice(configId: string, deviceId: string, label?: string) {
    try {
      await meshRosterApprove(configId, deviceId, label);
      this.toast("ok", "Approved — it can join now");
      await this.refreshRoster(configId);
    } catch (e) {
      this.toast("warn", `Couldn't approve: ${errMsg(e)}`);
    }
  }

  async removeDevice(configId: string, deviceId: string) {
    try {
      await meshRosterRemove(configId, deviceId);
      this.toast("info", "Removed from the network");
      await this.refreshRoster(configId);
    } catch (e) {
      this.toast("warn", `Couldn't remove: ${errMsg(e)}`);
    }
  }

  // ---- relationships ----------------------------------------------

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
