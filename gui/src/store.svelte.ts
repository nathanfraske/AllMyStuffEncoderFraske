// The single source of UI truth. Holds the graph catalog plus the
// transient interaction state (what's selected, what's being dragged,
// which sheet is open) as Svelte 5 runes, and exposes the verbs the
// components call. The connection rules live in `catalog.ts`; this layer
// is about *intent* and *feedback*.

import {
  canSink,
  canSource,
  capabilityForDisplay,
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
  tuneRoute,
  type StreamTune,
  consoleWindowTarget,
  disconnectRoute,
  fleetKick,
  fleetLeave,
  isTauri,
  openFilesWindow,
  meshIdentity,
  meshIdentitySetLabel,
  meshConfigShow,
  meshNetworkAdd,
  meshNetworkIdGenerate,
  meshNetworkRemove,
  meshNetworks,
  meshNetworkUpdate,
  meshPeers,
  meshRosterApprove,
  meshRosterList,
  meshRosterRemove,
  onOwned,
  onOwnership,
  onSession,
  onSubscription,
  openConsoleWindow,
  openTerminalWindow,
  ownedRoster,
  scanSelf,
  sendInput,
  sessionSnapshot,
  setClaimable,
  updateApply,
  updateCheck,
  updateSetPrefs,
  updateStatus,
  type SessionSnapshot,
} from "./tauri";
import {
  BUNDLE_TEMPLATES,
  FEATURE_FILES,
  FEATURE_TERMINAL,
  isAppNode,
  networkDisplayName,
  type Capability,
  type Catalog,
  type CheckOutcome,
  type Flow,
  type Grant,
  type IdentityInfo,
  type InputAction,
  type MediaKind,
  type MeshNode,
  type NetworkConfigFull,
  type NetworkSummary,
  type OwnedRoster,
  type PeerInfo,
  type Person,
  type Relationship,
  type RosterPeer,
  type Route,
  type RouteLiveState,
  type TurnEntry,
  type UpdatePrefs,
  type UpdateStatus,
} from "./types";

/** Which pane the settings panel is showing. */
export type SettingsTab = "networks" | "updates" | "fleet" | "sharing";

/** Sub-pane within the Networks settings tab (MyOwnLLM-style sub-tabs). */
export type NetworksSubtab = "status" | "servers" | "devices";

/** A device waiting to be let onto a network — surfaced across *all* joined
 *  networks for the "new device joining" approval nudge. */
export interface PendingJoin {
  networkId: string;
  networkName: string;
  peer: PeerInfo;
}

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

/** One person/fleet you're sharing with, gathered for the Sharing settings
 *  pane: who they are, which of their nodes you know, and every grant
 *  you've given them (with the node it's recorded on, for revocation). */
export interface SharePartner {
  person: Person;
  nodes: MeshNode[];
  grants: { node: MeshNode; grant: Grant }[];
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
  /** The unified Settings panel (networks · updates · fleet) and which pane
   *  it's showing. The top-bar gear opens it; the Networks button deep-links
   *  to the networks pane. */
  settingsOpen = $state(false);
  settingsTab = $state<SettingsTab>("networks");
  /** The "a new device wants to join" approval popup (the code-grid nudge). */
  approvalsOpen = $state(false);
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
  private consoleAudioRouteId: string | null = null;
  private consoleControlRouteId: string | null = null;
  /** The *live* display route the console renders frames for — also set when
   *  the route pre-existed (owned-for-teardown is tracked separately). */
  consoleVideoLive = $state<string | null>(null);
  /** The console's codec pill: which transport to *offer* for its video
   *  route. "auto" and "h264" both offer H.264 (auto lets the decode
   *  ladder pick where it's decoded); "mjpeg" forces the fallback. */
  consoleCodec = $state<"auto" | "h264" | "mjpeg">("auto");
  /** The console's quality pills — absent fields are Automatic. Sent to
   *  the streaming side, which restarts its capture with them. */
  consoleTune = $state<StreamTune>({});
  /** The live outbound control route console input events ride on. */
  consoleControlLive = $state<string | null>(null);

  // ---- terminal (the mesh-native shell) ----------------------------
  /** The remote machine the in-page terminal popover is open on (web
   *  preview only — the desktop opens a dedicated window per machine). */
  terminalNodeId = $state<string | null>(null);
  /** Live negotiation state per route id, straight from the last session
   *  snapshot. A terminal tab watches its own route here to tell
   *  "connecting" from "active" from "rejected (reason)" / "torn_down". */
  routeStates = $state<Record<string, RouteLiveState>>({});
  /** Per-app-run counter so each terminal tab mints a unique viewer-side
   *  endpoint (`{me}:term-view:…`) — unique endpoint, unique route id. */
  private termViewSeq = 0;

  // ---- files (the mesh-native file manager) -------------------------
  /** The remote machine the in-page files popover is open on (web preview
   *  only — the desktop opens a dedicated window per machine). */
  filesNodeId = $state<string | null>(null);
  /** Per-app-run counter so each files session mints a unique viewer-side
   *  endpoint (`{me}:files-view:…`) — unique endpoint, unique route id. */
  private filesViewSeq = 0;

  /** The local machine's node id. `"this"` in demo/web mode; the real mesh
   *  device id once the backend session is up. The graph centres on it. */
  localId = $state("this");

  // ---- networks / identity / roster (live mesh control) -----------
  /** This device's mesh identity. `label` is the display-name override. */
  identity = $state<IdentityInfo | null>(null);
  networks = $state<NetworkSummary[]>([]);
  /** config_id of the network the live session is currently on. */
  sessionNetwork = $state<string | null>(null);
  /** Which sub-pane of the Networks settings tab is showing. */
  networksSubtab = $state<NetworksSubtab>("status");
  /** Full per-network configs (signaling/STUN/TURN) from the daemon — the
   *  Servers pane reads + round-trips these. Keyed implicitly by `id`. */
  networkConfigs = $state<NetworkConfigFull[]>([]);
  /** config_id currently selected in the Servers pane. */
  serversNetwork = $state<string | null>(null);
  /** The network whose roster/approvals the Networks panel is showing. */
  rosterNetwork = $state<string | null>(null);
  roster = $state<RosterPeer[]>([]);
  livePeers = $state<PeerInfo[]>([]);
  /** Devices waiting to join, gathered across *every* joined network — what
   *  the approval nudge + popup act on. Refreshed by the mesh poll. */
  pendingJoins = $state<PendingJoin[]>([]);
  /** device ids the user declined in the popup this session. Declining is a
   *  cancel, not a deny: the device stays listed under Settings → Networks so
   *  it can still be approved later; it just stops nagging from the nudge. */
  dismissedJoins = $state<string[]>([]);

  // ---- owned fleet (the gossiped "Owned" roster) ------------------
  /** The shared key + members linking the devices you've claimed. */
  ownedFleet = $state<OwnedRoster | null>(null);

  // ---- self-update -------------------------------------------------
  updateInfo = $state<UpdateStatus | null>(null);
  updateBusy = $state(false);
  /** Result of the last manual "check now", for the Updates pane. */
  updateOutcome = $state<CheckOutcome | null>(null);

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

  /** Routes running between this machine and the console's remote — the
   *  live session the console's footer chips show. */
  consoleSessionRoutes = $derived.by(() => {
    const remote = this.consoleNodeId;
    if (!remote) return [] as Route[];
    return this.catalog.routes.filter((r) => {
      const f = this.capability(r.from);
      const t = this.capability(r.to);
      if (!f || !t) return false;
      const ends = [f.node, t.node];
      return ends.includes(remote) && ends.includes(this.localId);
    });
  });

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

  /** Pending joins (across all networks) the user hasn't declined this
   *  session — what the top-bar nudge counts and the popup shows. */
  freshJoins = $derived(
    this.pendingJoins.filter((j) => !this.dismissedJoins.includes(canonicalNodeId(j.peer.device_id))),
  );

  /** Canonical pubkeys of every device in your owned fleet (you included), so
   *  the graph/drawer can badge co-owned machines as one group. */
  fleetMemberIds = $derived.by(() => {
    const set = new Set<string>();
    for (const m of this.ownedFleet?.members ?? []) set.add(canonicalNodeId(m.device));
    return set;
  });

  /** Whether a node is part of your owned fleet (linked by the shared key). */
  isFleetMember(nodeId: string): boolean {
    return this.fleetMemberIds.has(canonicalNodeId(nodeId)) && this.fleetMemberIds.size > 1;
  }

  /** Whether an id refers to this very machine (any suffix form). */
  isMe(id: string): boolean {
    return sameMachine(id, this.localId);
  }

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

  /** Like `capability`, but synthesizes display stand-ins for terminal
   *  endpoints (which are never real catalog entries) — used by the views
   *  that render routes, so a live terminal session shows up. */
  capabilityForDisplay(id: string): Capability | undefined {
    return capabilityForDisplay(this.catalog, id);
  }

  // ---- lifecycle ---------------------------------------------------

  /** Wire up live backend data, if there is a backend. No-op (keeps the
   *  demo graph) in web mode. Called once on mount. */
  async init() {
    if (!isTauri()) {
      this.seedDemoFleet();
      this.seedDemoNetworks();
    }
    await this.hydrateFromBackend();
    await this.loadIdentity();
    await this.refreshNetworks();
    await this.syncMeshGraph();
    // Pull the *current* session state once — snapshots are otherwise only
    // emitted on changes, so a freshly-opened window (a per-machine console)
    // would see peers without their presence detail (no capabilities, no
    // ownership) until something next changed, and wrongly refuse to open.
    await this.pullSessionSnapshot();
    await this.loadOwnedFleet();
    await this.loadUpdateStatus();
    this.startMeshPolling();
    await onSubscription((s) => {
      const live = s.status === "live";
      // When the mesh comes up, re-scan + reload networks/identity: the first
      // pass at mount can run before the session is ready.
      if (live) {
        void this.hydrateFromBackend();
        void this.loadIdentity();
        void this.refreshNetworks().then(() => this.syncMeshGraph());
        void this.pullSessionSnapshot();
        void this.loadOwnedFleet();
      }
      this.backendConnected = live;
    });
    await onSession((snap) => this.applySessionSnapshot(snap));
    // The fleet roster converges live — a claim, or gossip catching up, pushes
    // a fresh copy. This is what makes a claim visibly *do* something.
    await onOwned((r) => {
      this.ownedFleet = r;
      this.reconcileFleetRelationships();
    });
    await onOwnership((o) => {
      const who = this.catalog.nodes.find((n) => sameMachine(n.id, o.from))?.label ?? "A device";
      if (o.message.kind === "claimed") this.toast("ok", `${who} joined your fleet`);
      else if (o.message.kind === "declined")
        this.toast("warn", `Couldn't claim ${who}: ${o.message.reason ?? "not claimable"}`);
    });
  }

  /** Fetch the live session state (peers' presence + routes) and merge it
   *  into the graph — the on-demand twin of the `allmystuff://session`
   *  event, for windows that opened after the last change was emitted. */
  private async pullSessionSnapshot() {
    const snap = await sessionSnapshot();
    if (snap) this.applySessionSnapshot(snap);
  }

  /** Pull a fresh session snapshot now. For views whose state hangs off
   *  route negotiation (a terminal tab's connecting → live → ended): the
   *  `allmystuff://session` event is the latency win, but a pull is the
   *  truth — the same doctrine the video plane settled on after lost
   *  pushes froze streams. */
  refreshSession(): Promise<void> {
    return this.pullSessionSnapshot();
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
    const joins: PendingJoin[] = [];
    // Which networks each machine is seen on (canonical pubkey → network
    // names), so the graph can show that you're on several networks and a
    // device may share only some of them.
    const deviceNets = new Map<string, Set<string>>();
    const addNet = (deviceId: string, name: string) => {
      const k = canonicalNodeId(deviceId);
      (deviceNets.get(k) ?? deviceNets.set(k, new Set()).get(k)!).add(name);
    };
    const nets = Array.isArray(this.networks) ? this.networks : [];
    for (const net of nets) {
      const netName = networkDisplayName(net);
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
        if (p.status === "pending_approval") {
          // Surfaced as a "new device wants to join" nudge + popup, not on the
          // graph. Gathered across every network so the nudge catches them all.
          joins.push({ networkId: net.config_id, networkName: netName, peer: p });
          continue;
        }
        addNet(p.device_id, netName);
        const e = live.get(p.device_id) ?? { label: p.label?.trim() || shortId(p.device_id), online: false };
        if (p.label?.trim()) e.label = p.label.trim();
        if (p.status === "active") e.online = true;
        live.set(p.device_id, e);
      }
      for (const r of roster) addNet(r.device_id, netName);
      rosterAll.push(...roster);
    }
    this.pendingJoins = joins;
    // Forget declines for devices that are no longer pending (approved or
    // gone), so a device that comes back later nudges afresh.
    const pendingCanon = new Set(joins.map((j) => canonicalNodeId(j.peer.device_id)));
    if (this.dismissedJoins.some((id) => !pendingCanon.has(id))) {
      this.dismissedJoins = this.dismissedJoins.filter((id) => pendingCanon.has(id));
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
      // "Self" is recognised by the live local id *and* the daemon identity's
      // device id — the latter is known as soon as the socket is up, before a
      // scan has re-homed the local node off its `"this"` placeholder. Without
      // it, this machine's own roster entry (bare pubkey) would spawn a
      // "not on AllMyStuff" twin at startup and the real "this device" node
      // wouldn't be recognised.
      if (this.isLocalMachine(id)) continue;
      // The daemon reports the bare pubkey; a presence advert may already
      // have created this machine's node under its display id. Resolve by
      // canonical pubkey so we update that one node rather than spawning a
      // bare-pubkey twin that reads as "not on AllMyStuff".
      const nodeNets = [...(deviceNets.get(canonicalNodeId(id)) ?? [])].sort();
      const node = this.nodeByCanonical(id);
      if (!node) {
        this.catalog.nodes.push({
          id,
          label: info.label,
          kind: "machine",
          relationship: { kind: "unclaimed" },
          online: info.online,
          app: false,
          networks: nodeNets,
        });
      } else {
        node.online = info.online;
        node.networks = nodeNets;
        if (!node.hostname && info.label) node.label = info.label;
      }
    }
    // The local machine is on every network we've joined.
    const me = this.node(this.localId) ?? this.catalog.nodes.find((n) => n.kind === "this");
    if (me) me.networks = nets.map((n) => networkDisplayName(n)).sort();
    // A machine that's no longer in any roster/peer set has dropped offline.
    // Compare by canonical pubkey so a presence node (display id) isn't wrongly
    // marked offline just because the daemon lists it under the bare pubkey.
    const knownCanon = new Set([...known.keys()].map(canonicalNodeId));
    for (const n of this.catalog.nodes) {
      if (n.kind !== "this" && !this.isLocalMachine(n.id) && !knownCanon.has(canonicalNodeId(n.id))) {
        n.online = false;
      }
    }
    // A freshly-discovered device may belong to someone we already share
    // with — fold it into that share.
    this.reconcileShares();
  }

  /** Whether `id` names this machine — by the live local id or the daemon
   *  identity's device id (known before a scan re-homes the local node). */
  private isLocalMachine(id: string): boolean {
    if (sameMachine(id, this.localId)) return true;
    const did = this.identity?.device_id;
    return !!did && sameMachine(id, did);
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

    // Adopt this machine as "this device". Find the one true local node —
    // by its new id, else *the* existing "this" node (its id may have drifted
    // from `localId` after a session snapshot), else its previous id — and
    // re-home that one rather than spawning a duplicate.
    const host = scan.hostname || scan.label || "This device";
    // Display name follows the naming rule: the override if the user set one,
    // else the machine hostname.
    const label = this.identity?.label?.trim() || host;
    const existing =
      this.catalog.nodes.find((n) => n.id === newId) ??
      this.catalog.nodes.find((n) => n.kind === "this") ??
      this.catalog.nodes.find((n) => n.id === prevId);
    const me: MeshNode = existing ?? {
      id: newId,
      label,
      hostname: host,
      kind: "this",
      relationship: { kind: "mine" },
      online: true,
      app: true,
      summary: scan.summary,
    };
    me.id = newId;
    me.kind = "this";
    me.label = label;
    me.hostname = host;
    me.summary = scan.summary;
    me.online = true;
    me.app = true;
    if (!existing) this.catalog.nodes.push(me);
    // Exactly one local node: keep the one at `newId`, drop any other "this"
    // node and any peer twin of this machine (an early daemon poll may have
    // added it under a bare id). Match by id, never by reference — a node just
    // pushed into the `$state` array comes back as a proxy, so `n === me`
    // would be false and would silently delete the local node.
    this.catalog.nodes = this.catalog.nodes.filter(
      (n) => n.id === newId || (n.kind !== "this" && !sameMachine(n.id, newId)),
    );
    // Local capabilities are exactly what the scan reports; drop any tied to
    // the old or new local id so a re-scan replaces rather than accumulates.
    this.catalog.capabilities = [
      ...scan.capabilities,
      ...this.catalog.capabilities.filter((c) => c.node !== newId && c.node !== prevId),
    ];
    // A console window scans too (it needs the local sinks to wire routes),
    // but only the main window announces it.
    if (!consoleWindowTarget()) this.toast("ok", "Scanned this machine");
  }

  /** Point the graph's local identity at `id`, re-homing the "this" node and
   *  its capabilities so everything keyed by the local id (graph centring,
   *  endpoint matching) stays consistent. The first scan can label the local
   *  node with a placeholder id; the live session then hands us the real one,
   *  and without this the node and `localId` drift apart — leaving the graph
   *  with no machine in the centre. */
  private setLocalId(id: string) {
    if (this.localId === id) return;
    this.localId = id;
    const me = this.catalog.nodes.find((n) => n.kind === "this");
    if (!me || me.id === id) return;
    const old = me.id;
    me.id = id;
    // Re-key this machine's capabilities from `old` to `id`.
    for (const c of this.catalog.capabilities) {
      if (c.node === old) {
        c.id = id + c.id.slice(old.length);
        c.node = id;
      }
    }
    // Fold any bare-pubkey twin of this machine into the local node. Match by
    // id, not reference (`$state` proxies the array's objects).
    this.catalog.nodes = this.catalog.nodes.filter((n) => n.id === id || !sameMachine(n.id, id));
  }

  /** Merge a live session snapshot into the graph: presence peers become
   *  nodes (keeping any relationship the user already set), and live route
   *  states are reflected. */
  applySessionSnapshot(snap: SessionSnapshot) {
    if (!snap.ready) return;
    if (snap.me) this.setLocalId(snap.me);
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
      // App features it supports ("terminal", …) — absent from an older
      // peer means none, and the matching buttons stay hidden.
      node.features = p.features ?? [];
      // A device that says *we* own it is ours; one owned by someone else
      // stays a guest/unclaimed (you can't flat-claim it). Never auto-flip a
      // relationship the user already set, and never auto-adopt.
      if (p.owner && sameMachine(p.owner, this.localId) && node.relationship.kind === "unclaimed") {
        node.relationship = { kind: "mine" };
      }
      // Collapse any other view of this same machine into the one node we just
      // settled on (id `p.node`) — heals an already-split graph. Match by id,
      // not reference: a freshly-pushed node is proxied by `$state`, so
      // `n === node` would be false and would delete the peer we just added.
      this.catalog.nodes = this.catalog.nodes.filter(
        (n) => n.id === p.node || !sameMachine(n.id, p.node),
      );
      // Refresh this peer's capabilities.
      this.catalog.capabilities = [
        ...this.catalog.capabilities.filter((c) => c.node !== p.node),
        ...p.capabilities,
      ];
    }

    // Reflect live routes (active ones become catalog routes), and keep
    // the per-route negotiation states for whoever watches one (a
    // terminal tab telling "connecting" from "rejected" by its reason).
    const states: Record<string, RouteLiveState> = {};
    for (const lr of snap.routes ?? []) {
      states[lr.route.id] = lr.state;
      const active = lr.state.state === "active";
      const id = lr.route.id;
      const exists = this.catalog.routes.some((r) => r.id === id);
      if (active && !exists) {
        this.catalog.routes.push({ ...lr.route, group: null });
      } else if (!active && exists) {
        this.catalog.routes = this.catalog.routes.filter((r) => r.id !== id);
      }
    }
    this.routeStates = states;

    this.reconcileFleetRelationships();
    this.reconcileShares();
  }

  /** Fleet membership implies the relationship. Ownership is *directional*
   *  — your owner machine advertises no owner of its own — so on a claimed
   *  device its owner would read "unclaimed" forever even while wearing the
   *  fleet badge (mutually exclusive states on screen). Any co-member of
   *  your fleet is *yours*; one that left (or kicked you) and doesn't claim
   *  us as owner reverts to unclaimed. A relationship the user set to
   *  `shared` is never touched. */
  private reconcileFleetRelationships() {
    const meInFleet = this.isFleetMember(this.localId);
    for (const n of this.catalog.nodes) {
      if (n.kind === "this" || this.isMe(n.id)) continue;
      const inFleet = meInFleet && this.isFleetMember(n.id);
      const ownedByMe = !!n.owner && sameMachine(n.owner, this.localId);
      if (n.relationship.kind === "unclaimed" && inFleet) {
        n.relationship = { kind: "mine" };
      } else if (n.relationship.kind === "mine" && !inFleet && !ownedByMe) {
        n.relationship = { kind: "unclaimed" };
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
   *  reaches the node's matching sink, a sink is fed by its source. A
   *  connection that lands pops the console that manages it (the remote
   *  control for screens/audio/control, the file manager for storage), so
   *  sending something *to* a machine immediately hands you its session.
   */
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
    // The console belongs to whichever end of the wire isn't this machine.
    const remote = !this.isMe(nodeId) ? nodeId : !this.isMe(cap.node) ? cap.node : null;
    if (canSource(cap.flow)) {
      const sink = matchEndpoint(this.catalog, nodeId, cap.media, "consume");
      if (sink) {
        if (this.connect(capId, sink.id) && remote) this.popConsoleFor(remote, cap.media);
        return;
      }
    }
    if (canSink(cap.flow)) {
      const src = matchEndpoint(this.catalog, nodeId, cap.media, "provide");
      if (src) {
        if (this.connect(src.id, capId) && remote) this.popConsoleFor(remote, cap.media);
        return;
      }
    }
    const where = this.node(nodeId)?.label ?? "that device";
    this.toast("warn", `${where} has nowhere to put ${cap.label}`);
  }

  /** Open the console that manages connections of this kind with `nodeId`
   *  — the pikvm-style remote control for screen/audio/control media, the
   *  file manager for storage. Quiet when that console isn't right for the
   *  node (no gate-failure toasts after a *successful* connect) — and only
   *  for machines that are *yours*: the remote-control console pulls the
   *  far screen, which on a guest's machine would surprise both of you
   *  with a fresh permission ask. */
  private popConsoleFor(nodeId: string, media: MediaKind) {
    const node = this.node(nodeId);
    if (!node || this.isMe(nodeId) || !isAppNode(node)) return;
    if (media === "storage") {
      if (this.filesAllowed(node)) this.openFiles(nodeId);
      return;
    }
    if (media === "display" || media === "video" || media === "input" || media === "audio") {
      if (node.relationship.kind === "mine") this.openConsole(nodeId);
    }
  }

  /** Try to wire `from` → `to`. On success the route appears (and `true`
   *  comes back); if it needs a shared person's permission, raises the
   *  share sheet instead. */
  connect(from: string, to: string, codec?: "auto" | "h264" | "mjpeg"): boolean {
    const res = proposeRoute(this.catalog, from, to);
    if (res.ok) {
      this.addRoute(res.route.from, res.route.to);
      this.fireBackendConnect(res.route.from, res.route.to, res.route.media, codec);
      const f = this.capability(from)?.label ?? from;
      const t = this.capability(to)?.label ?? to;
      this.toast("ok", `Connected ${f} → ${t}`);
      return true;
    }
    if (res.denied && res.denied.length) {
      this.pendingShare = {
        from,
        to,
        fromLabel: this.capability(from)?.label ?? from,
        toLabel: this.capability(to)?.label ?? to,
        requests: res.denied,
      };
      return false;
    }
    this.toast("warn", res.reason);
    return false;
  }

  /** User approved the pending share: add exactly the requested grants,
   *  then complete the connection (popping the session's console, the same
   *  as a connect that never needed to ask). */
  approvePendingShare() {
    const p = this.pendingShare;
    if (!p) return;
    for (const req of p.requests) this.grant(req.node, requestToGrant(req));
    const res = proposeRoute(this.catalog, p.from, p.to);
    if (res.ok) {
      this.addRoute(res.route.from, res.route.to);
      this.fireBackendConnect(res.route.from, res.route.to, res.route.media);
      this.toast("ok", `Shared — connected ${p.fromLabel} → ${p.toLabel}`);
      const ends = [this.capability(p.from)?.node, this.capability(p.to)?.node];
      const remote = ends.find((n) => n && !this.isMe(n));
      if (remote) this.popConsoleFor(remote, res.route.media);
    }
    this.pendingShare = null;
  }

  dismissPendingShare() {
    this.pendingShare = null;
  }

  /** When a real backend is connected, fire the actual mesh route offer.
   *  The backend's session snapshots then keep the route's live state in
   *  sync; in web mode this is a no-op and the local route stands in. */
  private fireBackendConnect(
    from: string,
    to: string,
    media: MediaKind,
    codec?: "auto" | "h264" | "mjpeg",
  ) {
    if (this.backendConnected) void connectRoute(from, to, media, codec);
  }

  private addRoute(from: string, to: string, group: string | null = null) {
    const cap = this.capability(from);
    const id = `route:${from}→${to}`;
    if (this.catalog.routes.some((r) => r.id === id)) return;
    this.catalog.routes.push({ id, from, to, media: cap?.media ?? "generic", group });
  }

  /** Tear a route down. The local catalog updates synchronously; the
   *  returned promise settles when the backend disconnect has been sent —
   *  callers that must outlive the call (a closing console window) await
   *  it, everyone else ignores it. */
  disconnect(routeId: string): Promise<unknown> {
    const sent = this.backendConnected ? disconnectRoute(routeId) : Promise.resolve(null);
    const route = this.catalog.routes.find((r) => r.id === routeId);
    this.catalog.routes = this.catalog.routes.filter((r) => r.id !== routeId);
    // Tearing one leg of a group tears the whole bundle — it's one thing.
    if (route?.group) {
      this.catalog.routes = this.catalog.routes.filter((r) => r.group !== route.group);
      this.toast("info", "Disconnected the group");
    }
    return sent;
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
   *  screen, its audio passthrough and keyboard/mouse control. On the
   *  desktop this opens a *dedicated OS window* per machine (so several
   *  consoles can be up side by side); the web preview keeps the in-page
   *  popover. */
  openConsole(nodeId: string) {
    const node = this.node(nodeId);
    if (!this.consoleAllowed(node, nodeId)) return;
    if (isTauri()) {
      void openConsoleWindow(nodeId);
      return;
    }
    this.openConsoleHere(nodeId);
  }

  /** Start the console session *in this window* — the body of a console
   *  window (and the web preview's popover). Wires the backbone video route
   *  to this machine's display and brings audio passthrough and keyboard &
   *  mouse up with it — a console is the whole session by default, like
   *  sitting down at the machine. The toggles inside are the off-switches
   *  (and each leg reports itself when it has no path). */
  openConsoleHere(nodeId: string) {
    const node = this.node(nodeId);
    if (!this.consoleAllowed(node, nodeId)) return;
    this.consoleNodeId = nodeId;
    this.consoleAudio = false;
    this.consoleControl = false;
    this.consoleVideoRouteId = null;
    this.consoleAudioRouteId = null;
    this.consoleControlRouteId = null;
    this.consoleVideoLive = null;
    this.consoleControlLive = null;
    this.consoleInput = this.consoleVideoInputs(nodeId)[0]?.id ?? null;
    this.consoleCodec = "auto";
    this.consoleTune = {};
    void this.applyConsoleVideo();
    this.toggleConsoleAudio();
    this.toggleConsoleControl();
    this.toast("ok", `Console open on ${node!.label}`);
  }

  /** The gate both console entries share: a known remote machine that runs
   *  AllMyStuff and is yours (or shared with you). */
  private consoleAllowed(node: MeshNode | undefined, nodeId: string): node is MeshNode {
    if (!node) return false;
    if (nodeId === this.localId) {
      this.toast("warn", "That's this device");
      return false;
    }
    if (!isAppNode(node)) {
      this.toast("warn", `${node.label} isn't running AllMyStuff`);
      return false;
    }
    if (node.relationship.kind === "unclaimed") {
      this.toast("warn", `Claim ${node.label} first, or mark it shared`);
      return false;
    }
    return true;
  }

  /** Find the machine node `id` refers to, under any id form (exact, or the
   *  same canonical pubkey) — how a console window resolves its target. */
  machineByAnyId(id: string): MeshNode | undefined {
    return this.nodeByCanonical(id);
  }

  /** Close the console, tearing down exactly the routes it created. The UI
   *  state resets synchronously; the returned promise settles once the
   *  backend disconnects are on the wire, so a console *window* can hold
   *  its close until then. */
  closeConsole(): Promise<unknown> {
    const pending: Promise<unknown>[] = [];
    if (this.consoleVideoRouteId) pending.push(this.disconnect(this.consoleVideoRouteId));
    if (this.consoleAudioRouteId) pending.push(this.disconnect(this.consoleAudioRouteId));
    if (this.consoleControlRouteId) pending.push(this.disconnect(this.consoleControlRouteId));
    this.consoleVideoRouteId = null;
    this.consoleAudioRouteId = null;
    this.consoleControlRouteId = null;
    this.consoleVideoLive = null;
    this.consoleControlLive = null;
    this.consoleNodeId = null;
    this.consoleInput = null;
    this.consoleAudio = false;
    this.consoleControl = false;
    return Promise.allSettled(pending);
  }

  /** Forward one keyboard/mouse event down the console's control route.
   *  Fire-and-forget — at pointer-move rates a lost event is meaningless. */
  sendConsoleInput(action: InputAction) {
    if (!this.consoleControlLive) return;
    void sendInput(this.consoleControlLive, action);
  }

  /** Switch which remote source the console is showing. */
  setConsoleInput(capId: string) {
    this.consoleInput = capId;
    void this.applyConsoleVideo();
  }

  /** Bumped per video (re)wire; an apply that awaited a teardown checks it
   *  before connecting, so rapid tab clicks can't interleave two wires. */
  private consoleVideoEpoch = 0;

  private async applyConsoleVideo() {
    const epoch = ++this.consoleVideoEpoch;
    if (this.consoleVideoRouteId) {
      const old = this.consoleVideoRouteId;
      this.consoleVideoRouteId = null;
      this.consoleVideoLive = null;
      // Await the teardown so it's on the wire *before* the next offer:
      // the sender frees its one H.264 lane per peer when the teardown
      // arrives, and channel order then guarantees the next screen takes
      // the lane over instead of racing it and landing on MJPEG.
      await this.disconnect(old);
    }
    this.consoleVideoLive = null;
    if (epoch !== this.consoleVideoEpoch) return; // a newer switch took over
    const inp = this.consoleInput ? this.capability(this.consoleInput) : null;
    if (!inp) return;
    // The remote screen (display) lands on this machine's display sink — a
    // real route the backend streams video frames down. A camera (video)
    // has no local sink yet, so it's view-only until camera transport
    // lands; the console is honest about that.
    const sink = matchEndpoint(this.catalog, this.localId, inp.media, "consume");
    if (!sink) return;
    const leg = this.consoleConnect(inp.id, sink.id, this.consoleCodec);
    // Render whatever's live; only own the route for teardown if this call
    // created it.
    this.consoleVideoLive = leg?.id ?? null;
    this.consoleVideoRouteId = leg?.created ? leg.id : null;
    // Carry the quality pills onto the fresh route (the sender restarts
    // its capture with them; harmless no-op when everything is Auto).
    if (leg && this.hasTune()) void tuneRoute(leg.id, this.consoleTune);
  }

  private hasTune(): boolean {
    const t = this.consoleTune;
    return t.maxEdge != null || t.bitrate != null || t.fps != null;
  }

  /** One pill changed: remember it and re-tune the live stream. */
  setConsoleTune(patch: StreamTune) {
    this.consoleTune = { ...this.consoleTune, ...patch };
    if (this.consoleVideoLive) void tuneRoute(this.consoleVideoLive, this.consoleTune);
  }

  /** The codec pill changed: re-offer the video route on that transport. */
  setConsoleCodec(codec: "auto" | "h264" | "mjpeg") {
    if (this.consoleCodec === codec) return;
    this.consoleCodec = codec;
    void this.applyConsoleVideo();
  }

  /** Audio passthrough: play what the remote machine is playing — its
   *  system audio, loopback-captured on the far side — on this machine's
   *  speakers. Deliberately listen-only, not a call: the console never
   *  opens a microphone. The far side's loopback captures *everything*
   *  it plays, so any audio we injected would come straight back one
   *  round trip later as an echo (there's no echo cancellation yet) —
   *  the clean design is that nothing ever flows that way. */
  toggleConsoleAudio() {
    const remote = this.consoleNodeId;
    if (!remote) return;
    if (this.consoleAudio) {
      if (this.consoleAudioRouteId) this.disconnect(this.consoleAudioRouteId);
      this.consoleAudioRouteId = null;
      this.consoleAudio = false;
      return;
    }
    const from = matchEndpoint(this.catalog, remote, "audio", "provide");
    const to = matchEndpoint(this.catalog, this.localId, "audio", "consume");
    const leg = from && to ? this.consoleConnect(from.id, to.id) : null;
    if (leg) {
      // Own the route for teardown only if this call created it (never a
      // pre-existing one the user wired from the graph).
      this.consoleAudioRouteId = leg.created ? leg.id : null;
      this.consoleAudio = true;
    } else {
      this.toast("warn", "No audio path to that machine");
    }
  }

  /** Send this machine's keyboard & mouse to the remote (input injection on
   *  the far side is a follow-up; the route is real and shows active). */
  toggleConsoleControl() {
    const remote = this.consoleNodeId;
    if (!remote) return;
    if (this.consoleControl) {
      if (this.consoleControlRouteId) this.disconnect(this.consoleControlRouteId);
      this.consoleControlRouteId = null;
      this.consoleControlLive = null;
      this.consoleControl = false;
      return;
    }
    const mySrc = matchEndpoint(this.catalog, this.localId, "input", "provide");
    const remoteSink = matchEndpoint(this.catalog, remote, "input", "consume");
    const leg = mySrc && remoteSink ? this.consoleConnect(mySrc.id, remoteSink.id) : null;
    if (leg) {
      this.consoleControlRouteId = leg.created ? leg.id : null;
      this.consoleControlLive = leg.id;
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
  private consoleConnect(
    from: string,
    to: string,
    codec?: "auto" | "h264" | "mjpeg",
  ): { id: string; created: boolean } | null {
    const id = `route:${from}→${to}`;
    const existedBefore = this.catalog.routes.some((r) => r.id === id);
    this.connect(from, to, codec);
    const existsNow = this.catalog.routes.some((r) => r.id === id);
    if (!existsNow) return null; // blocked / denied — nothing got wired
    return { id, created: !existedBefore };
  }

  // ---- terminal (the mesh-native shell) ----------------------------

  /** Whether `node` can host a terminal at all: it runs AllMyStuff and its
   *  presence advertises the feature (an older build simply doesn't). */
  terminalSupported(node: MeshNode | undefined): boolean {
    return !!node && isAppNode(node) && (node.features ?? []).includes(FEATURE_TERMINAL);
  }

  /** The gate for "Open Terminal" — a mirror of the host's own rule
   *  (`sender_may_control`): only the node's recorded owner or a co-owned
   *  fleet member gets a shell. Deliberately *not* `relationship.kind`,
   *  which is a local label the user can set freely; this checks the same
   *  facts the far side will enforce, so the button never promises what
   *  the host would refuse. */
  terminalAllowed(node: MeshNode | undefined): boolean {
    if (!node || this.isMe(node.id) || !this.terminalSupported(node)) return false;
    const ownerIsMe = !!node.owner && this.isMe(node.owner);
    const coFleet = this.isFleetMember(this.localId) && this.isFleetMember(node.id);
    return ownerIsMe || coFleet;
  }

  /** Open a terminal on a remote machine. On the desktop this opens (or
   *  focuses) the machine's dedicated terminal window — tabs inside it are
   *  separate shells; the web preview keeps an in-page popover. */
  openTerminal(nodeId: string) {
    const node = this.node(nodeId);
    if (!node) return;
    if (this.isMe(nodeId)) {
      this.toast("warn", "That's this device");
      return;
    }
    if (!this.terminalSupported(node)) {
      this.toast("warn", `${node.label} doesn't support terminals (older AllMyStuff?)`);
      return;
    }
    if (!this.terminalAllowed(node)) {
      this.toast("warn", `Terminals are owner/fleet only — ${node.label} isn't yours`);
      return;
    }
    if (isTauri()) {
      void openTerminalWindow(nodeId);
      return;
    }
    this.terminalNodeId = nodeId;
  }

  /** Close the in-page terminal popover (web preview). The desktop's
   *  terminal windows close themselves, tearing their tabs down first. */
  closeTerminal() {
    this.terminalNodeId = null;
  }

  /** Open one terminal *session* (a tab) to `hostNodeId`: a generic route
   *  from the host's `…:terminal` endpoint to a viewer endpoint minted for
   *  this tab — unique endpoint, unique route id, so tabs to one machine
   *  never collide. Deliberately not `connect()`/`proposeRoute`: terminal
   *  endpoints aren't catalog capabilities (see `capabilityForDisplay`),
   *  and the binding authorization runs host-side against the owner/fleet
   *  rule. Returns the route id the tab watches, or null in web mode
   *  (no backend — nothing can flow). */
  terminalConnect(hostNodeId: string): string | null {
    if (!this.backendConnected) return null;
    const from = `${hostNodeId}:terminal`;
    const n = ++this.termViewSeq;
    const to = `${this.localId}:term-view:${Date.now().toString(36)}-${n}`;
    void connectRoute(from, to, "generic");
    return `route:${from}→${to}`;
  }

  /** Tear one terminal session down (tab closed / window closing). The
   *  returned promise settles when the disconnect is on the wire, so a
   *  closing window can hold its close until then. */
  terminalDisconnect(routeId: string): Promise<unknown> {
    return this.disconnect(routeId);
  }

  // ---- files (the mesh-native file manager) -------------------------

  /** Whether `node` can host a files session at all: it runs AllMyStuff
   *  and its presence advertises the feature (an older build doesn't). */
  filesSupported(node: MeshNode | undefined): boolean {
    return !!node && isAppNode(node) && (node.features ?? []).includes(FEATURE_FILES);
  }

  /** The gate for "Open Files" — the same owner/fleet rule as the
   *  terminal (browsing a disk is as privileged as a shell), checked
   *  against the facts the far side will enforce. */
  filesAllowed(node: MeshNode | undefined): boolean {
    if (!node || this.isMe(node.id) || !this.filesSupported(node)) return false;
    const ownerIsMe = !!node.owner && this.isMe(node.owner);
    const coFleet = this.isFleetMember(this.localId) && this.isFleetMember(node.id);
    return ownerIsMe || coFleet;
  }

  /** Open the file manager on a remote machine. On the desktop this opens
   *  (or focuses) the machine's dedicated files window; the web preview
   *  keeps an in-page popover. */
  openFiles(nodeId: string) {
    const node = this.node(nodeId);
    if (!node) return;
    if (this.isMe(nodeId)) {
      this.toast("warn", "That's this device");
      return;
    }
    if (!this.filesSupported(node)) {
      this.toast("warn", `${node.label} doesn't support file browsing (older AllMyStuff?)`);
      return;
    }
    if (!this.filesAllowed(node)) {
      this.toast("warn", `Files are owner/fleet only — ${node.label} isn't yours`);
      return;
    }
    if (isTauri()) {
      void openFilesWindow(nodeId);
      return;
    }
    this.filesNodeId = nodeId;
  }

  /** Close the in-page files popover (web preview). The desktop's files
   *  windows close themselves, tearing their route down first. */
  closeFiles() {
    this.filesNodeId = null;
  }

  /** Open one files *session* to `hostNodeId`: a generic route from the
   *  host's `…:files` endpoint to a viewer endpoint minted for this
   *  window. Deliberately not `connect()`/`proposeRoute` — files
   *  endpoints aren't catalog capabilities (see `capabilityForDisplay`),
   *  and the binding authorization runs host-side against the owner/fleet
   *  rule. Returns the route id the window watches, or null in web mode. */
  filesConnect(hostNodeId: string): string | null {
    if (!this.backendConnected) return null;
    const from = `${hostNodeId}:files`;
    const n = ++this.filesViewSeq;
    const to = `${this.localId}:files-view:${Date.now().toString(36)}-${n}`;
    void connectRoute(from, to, "generic");
    return `route:${from}→${to}`;
  }

  /** Tear one files session down (window closing). The returned promise
   *  settles when the disconnect is on the wire. */
  filesDisconnect(routeId: string): Promise<unknown> {
    return this.disconnect(routeId);
  }

  // ---- ownership / claiming ---------------------------------------

  /** Adopt a device as one of yours. Honours Task 4: this only works when
   *  the device is *claimable* (booted in claim mode, still unowned) or
   *  already says you own it — you can't flat-out take a box that has an
   *  owner or was never offered. */
  claim(nodeId: string) {
    const n = this.node(nodeId);
    if (!n) return;
    // Compare ownership by canonical pubkey: the device advertises its owner
    // as a bare-pubkey id, while our `localId` is the display id.
    const ownedByMe = !!n.owner && sameMachine(n.owner, this.localId);
    if (n.owner && !ownedByMe) {
      this.toast("warn", `${n.label} is owned by another device — you can't take it`);
      return;
    }
    if (!n.claimable && !ownedByMe) {
      this.toast(
        "warn",
        `${n.label} isn't in claim mode. Start it claimable on the device itself to adopt it.`,
      );
      return;
    }
    if (this.backendConnected) {
      // The device confirms by re-advertising presence with owner = us (the
      // `claimed` ownership event toasts the success); a delivery failure
      // rejects, so the ask never silently goes nowhere.
      claimNode(nodeId).catch((e) => {
        this.toast("warn", `Couldn't ask ${n.label} to join: ${String(e)}`);
      });
      this.toast("info", `Asking ${n.label} to join your fleet…`);
    } else {
      // Demo/web: the claimable device accepts and joins the fleet, so the
      // "Owned" roster visibly grows under a shared key — exactly what the
      // backend does over the wire.
      n.owner = this.localId;
      n.claimable = false;
      n.relationship = { kind: "mine" };
      this.addToDemoFleet(n);
      this.toast("ok", `${n.label} joined your fleet`);
      this.reauthorize();
    }
  }

  /** Demo/web only: grow the simulated fleet roster so claiming groups devices
   *  under a shared key without a backend (the real one gossips it instead). */
  private addToDemoFleet(node: MeshNode) {
    const key = this.ownedFleet?.key || `demo-${Math.random().toString(16).slice(2, 10)}`;
    const members = [...(this.ownedFleet?.members ?? [])];
    const add = (id: string, label: string) => {
      if (!members.some((m) => sameMachine(m.device, id))) members.push({ device: id, label });
    };
    const me = this.node(this.localId);
    if (me) add(me.id, me.label);
    add(node.id, node.label);
    this.ownedFleet = { key, version: (this.ownedFleet?.version ?? 0) + 1, members };
  }

  /** Demo/web only: seed the fleet from the machines already marked yours, so
   *  the Fleet view isn't empty before you claim anything in the preview. */
  private seedDemoFleet() {
    const members = this.catalog.nodes
      .filter((n) => n.relationship.kind === "mine")
      .map((n) => ({ device: n.id, label: n.label }));
    if (members.length > 1) {
      this.ownedFleet = { key: "demo-fleet-key-7f3a91c2", version: 1, members };
    }
  }

  /** Demo/web only: stand in two networks with their server configs and spread
   *  the demo devices across them, so the multi-network UI (the Servers +
   *  Devices panes, the per-node network chips) is alive in the preview. */
  private seedDemoNetworks() {
    this.networks = [
      { config_id: "net-home", network_id: "home-7f3a91c2x", label: "Home", phase: "joined" },
      { config_id: "net-work", network_id: "work-22ab90f1y", label: "Work", phase: "joined" },
    ];
    this.networkConfigs = [
      {
        id: "net-home",
        network_id: "home-7f3a91c2x",
        label: "Home",
        signaling: { servers: ["wss://myownmesh.com"] },
        stun_servers: [{ urls: ["stun:stun.myownmesh.com:3478"] }],
        turn_servers: [
          { urls: ["turn:turn.myownmesh.com:3478"], username: "guest", credential: "theguestpassword" },
        ],
      },
      {
        id: "net-work",
        network_id: "work-22ab90f1y",
        label: "Work",
        signaling: { servers: ["wss://relay.example.org"] },
        stun_servers: [{ urls: ["stun:stun.example.org:3478"] }],
        turn_servers: [],
      },
    ];
    this.serversNetwork = "net-home";
    // Spread the demo machines across the two networks — note some are on only
    // one, which is the whole point: you're not on a single "mesh".
    const assign: Record<string, string[]> = {
      this: ["Home", "Work"],
      desk: ["Home"],
      tv: ["Home"],
      studio: ["Work"],
      nuc: ["Work"],
      garage: ["Home"],
      alex: ["Work"],
    };
    for (const n of this.catalog.nodes) if (assign[n.id]) n.networks = assign[n.id];
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
      // Sending a bundle at a machine pops its session console too — the
      // first route's media picks which one (a desk/call kit is console
      // media; a storage bundle would open files).
      const lead = res.routes[0];
      if (lead && !this.isMe(target)) this.popConsoleFor(target, lead.media);
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

  // ---- per-network transport config (signaling · STUN · TURN) -----

  /** Pull every network's full config (servers) from the daemon for the
   *  Servers pane. Safe to call often; no-op in web mode. */
  async loadNetworkConfigs() {
    if (!isTauri()) return;
    try {
      this.networkConfigs = (await meshConfigShow()) ?? [];
      if (!this.serversNetwork && this.networkConfigs.length > 0) {
        this.serversNetwork = this.networkConfigs[0].id;
      }
    } catch (e) {
      this.toast("warn", `Couldn't load network settings: ${errMsg(e)}`);
    }
  }

  networkConfig(configId: string): NetworkConfigFull | undefined {
    return this.networkConfigs.find((n) => n.id === configId);
  }

  /** Replace one network's signaling/STUN/TURN servers. Round-trips the full
   *  config so unrelated fields (topology, auto-approve, roster path) survive,
   *  then asks the daemon to apply it — which restarts that network's
   *  transport and reconnects. */
  async updateNetworkServers(
    configId: string,
    servers: { signaling: string[]; stun: string[]; turn: TurnEntry[] },
  ) {
    const cfg = this.networkConfig(configId);
    if (!cfg) {
      this.toast("warn", "That network isn't loaded — reopen Settings");
      return;
    }
    const next: NetworkConfigFull = {
      ...cfg,
      signaling: {
        ...(cfg.signaling ?? {}),
        servers: servers.signaling.map((s) => s.trim()).filter(Boolean),
      },
      stun_servers: servers.stun.map((s) => s.trim()).filter(Boolean).map((u) => ({ urls: [u] })),
      turn_servers: servers.turn
        .filter((t) => t.url.trim())
        .map((t) => ({
          urls: [t.url.trim()],
          username: t.username.trim() || null,
          credential: t.credential.trim() || null,
        })),
    };
    try {
      await meshNetworkUpdate(next);
      this.toast("ok", "Saved — reconnecting with the new servers");
      await this.loadNetworkConfigs();
      await this.refreshNetworks();
    } catch (e) {
      this.toast("warn", `Couldn't save servers: ${errMsg(e)}`);
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

  // ---- settings · approvals · fleet · updates ---------------------

  /** Open the unified settings panel on a given pane. The top-bar gear opens
   *  it on Networks; the Networks button deep-links here too. Refreshes the
   *  pane data so it's never stale on open. */
  openSettings(tab: SettingsTab = "networks") {
    this.settingsTab = tab;
    this.settingsOpen = true;
    void this.refreshNetworks().then(() => {
      const net = this.rosterNetwork ?? this.activeNetwork?.config_id ?? null;
      if (net) void this.refreshRoster(net);
    });
    void this.loadOwnedFleet();
    void this.loadNetworkConfigs();
    if (tab === "updates") void this.loadUpdateStatus();
  }

  /** Open the "a new device wants to join" approval popup (the code grid). */
  openApprovals() {
    if (this.freshJoins.length === 0) return;
    this.approvalsOpen = true;
  }

  /** Approve a pending join straight from the popup. */
  async approveJoin(j: PendingJoin) {
    await this.approveDevice(j.networkId, j.peer.device_id, j.peer.label);
    // Drop it locally so the popup updates at once (the next poll confirms).
    this.pendingJoins = this.pendingJoins.filter(
      (x) => !(x.networkId === j.networkId && x.peer.device_id === j.peer.device_id),
    );
    if (this.freshJoins.length === 0) this.approvalsOpen = false;
  }

  /** Decline a join — a *cancel*, not a deny. It stops the nudge but leaves
   *  the device approvable later under Settings → Networks. (A real block is a
   *  separate control, coming later.) */
  dismissJoin(deviceId: string) {
    const canon = canonicalNodeId(deviceId);
    if (!this.dismissedJoins.includes(canon)) {
      this.dismissedJoins = [...this.dismissedJoins, canon];
    }
    if (this.freshJoins.length === 0) this.approvalsOpen = false;
  }

  async loadOwnedFleet() {
    if (!isTauri()) return;
    try {
      const r = await ownedRoster();
      if (r) {
        this.ownedFleet = r;
        this.reconcileFleetRelationships();
      }
    } catch {
      /* no daemon yet — claim still simulates a fleet in demo mode */
    }
  }

  /** Leave the fleet this device is in. The backend broadcasts the bumped
   *  roster (the others drop us), releases our owner, and pushes the now-
   *  empty roster back via `allmystuff://owned`. */
  async leaveFleet() {
    if (this.backendConnected) {
      try {
        await fleetLeave();
        this.toast("ok", "Left the fleet");
      } catch (e) {
        this.toast("warn", `Couldn't leave the fleet: ${String(e)}`);
      }
      return;
    }
    // Demo/web: drop ourselves from the simulated roster.
    if (!this.ownedFleet) return;
    const members = this.ownedFleet.members.filter((m) => !this.isMe(m.device));
    this.ownedFleet = members.length
      ? { ...this.ownedFleet, version: this.ownedFleet.version + 1, members }
      : null;
    this.toast("ok", "Left the fleet");
  }

  /** Kick a member out of the fleet — allowed only while we're a member
   *  ourselves (the backend enforces it; the demo mirrors the rule). */
  async kickFleetMember(device: string) {
    if (this.isMe(device)) {
      void this.leaveFleet();
      return;
    }
    const label =
      this.ownedFleet?.members.find((m) => sameMachine(m.device, device))?.label || "that device";
    if (this.backendConnected) {
      try {
        await fleetKick(device);
        this.toast("ok", `Kicked ${label} from the fleet`);
      } catch (e) {
        this.toast("warn", `Couldn't kick ${label}: ${String(e)}`);
      }
      return;
    }
    // Demo/web: mirror the membership rule, then drop them.
    if (!this.ownedFleet || !this.isFleetMember(this.localId)) {
      this.toast("warn", "You can't kick devices from a fleet you aren't in");
      return;
    }
    this.ownedFleet = {
      ...this.ownedFleet,
      version: this.ownedFleet.version + 1,
      members: this.ownedFleet.members.filter((m) => !sameMachine(m.device, device)),
    };
    this.toast("ok", `Kicked ${label} from the fleet`);
  }

  async loadUpdateStatus() {
    if (!isTauri()) return;
    try {
      this.updateInfo = await updateStatus();
    } catch (e) {
      this.toast("warn", `Couldn't read update status: ${errMsg(e)}`);
    }
  }

  /** Check the release feed now and stage anything permitted. */
  async checkUpdates() {
    if (!isTauri()) {
      this.toast("info", "Updates need the desktop app");
      return;
    }
    this.updateBusy = true;
    this.updateOutcome = null;
    try {
      this.updateOutcome = await updateCheck();
      this.updateInfo = (await updateStatus()) ?? this.updateInfo;
      this.describeCheckOutcome(this.updateOutcome);
    } catch (e) {
      this.toast("warn", `Update check failed: ${errMsg(e)}`);
    } finally {
      this.updateBusy = false;
    }
  }

  /** Apply a staged update to disk (it takes effect on next launch). */
  async applyUpdate() {
    if (!isTauri()) return;
    this.updateBusy = true;
    try {
      const r = await updateApply();
      if (r?.applied) this.toast("ok", `Update ${r.applied} staged — it applies on next launch`);
      else this.toast("info", "Nothing staged to apply");
      this.updateInfo = (await updateStatus()) ?? this.updateInfo;
    } catch (e) {
      this.toast("warn", `Couldn't apply update: ${errMsg(e)}`);
    } finally {
      this.updateBusy = false;
    }
  }

  async setUpdatePrefs(prefs: UpdatePrefs) {
    if (!isTauri()) return;
    try {
      const next = await updateSetPrefs(prefs);
      if (next) this.updateInfo = next;
    } catch (e) {
      this.toast("warn", `Couldn't save update settings: ${errMsg(e)}`);
    }
  }

  private describeCheckOutcome(o: CheckOutcome | null) {
    if (!o) return;
    switch (o.outcome) {
      case "staged":
        this.toast("ok", `Update ${o.version} downloaded — applies on next launch`);
        break;
      case "up_to_date":
        this.toast("ok", "You're on the latest version");
        break;
      case "policy_blocked":
        this.toast("info", `${o.latest} is available but held by your auto-apply setting`);
        break;
      case "package_manager":
        this.toast("info", "Installed via a package manager — update through it");
        break;
      case "disabled":
        this.toast("info", "Auto-update is off");
        break;
      case "not_due":
        this.toast("info", "Checked recently — try again shortly");
        break;
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

  /** The person a share with this node is *really* with: its owner. The
   *  person is keyed by the owner's canonical pubkey (the node's own when
   *  it advertises no owner — an owner machine doesn't), so every node
   *  one owner brings resolves to the same person. An existing share with
   *  that person lends its identity, so names stay stable. */
  personFor(node: MeshNode): Person {
    const ownerKey = canonicalNodeId(node.owner ?? node.id);
    const id = `person:${ownerKey}`;
    for (const n of this.catalog.nodes) {
      if (n.relationship.kind === "shared" && n.relationship.person.id === id) {
        return n.relationship.person;
      }
    }
    const ownerNode = this.catalog.nodes.find((n) => sameMachine(n.id, ownerKey));
    return { id, name: ownerNode?.label ?? node.label };
  }

  /** Mark a node shared — a connection with its *owner*, not just this
   *  machine: every node that owner brings joins the same share, so what
   *  you grant them works to any of their devices. */
  markShared(nodeId: string) {
    const n = this.node(nodeId);
    if (!n) return;
    const person = this.personFor(n);
    n.relationship = { kind: "shared", person, grants: [] };
    this.reconcileShares();
    this.reauthorize();
  }

  /** Sharing follows the person across their fleet: any unclaimed node
   *  whose owner is already a share partner joins that share (so a second
   *  machine of theirs appearing later is covered without re-asking). A
   *  relationship the user set is never touched. */
  private reconcileShares() {
    const partners = new Map<string, Person>();
    for (const n of this.catalog.nodes) {
      if (n.relationship.kind === "shared") {
        partners.set(n.relationship.person.id, n.relationship.person);
      }
    }
    if (partners.size === 0) return;
    for (const n of this.catalog.nodes) {
      if (n.kind === "this" || this.isMe(n.id)) continue;
      if (n.relationship.kind !== "unclaimed") continue;
      const person = partners.get(`person:${canonicalNodeId(n.owner ?? n.id)}`);
      if (person) n.relationship = { kind: "shared", person, grants: [] };
    }
  }

  /** Everyone you're sharing with, one entry per person/fleet: their
   *  nodes and every grant you've given them (with the node each grant is
   *  recorded on). Drives the Sharing settings pane. */
  sharePartners = $derived.by((): SharePartner[] => {
    const map = new Map<string, SharePartner>();
    for (const n of this.catalog.nodes) {
      if (n.relationship.kind !== "shared") continue;
      const share = n.relationship;
      const p = map.get(share.person.id) ?? { person: share.person, nodes: [], grants: [] };
      p.nodes.push(n);
      for (const g of share.grants) p.grants.push({ node: n, grant: g });
      map.set(share.person.id, p);
    }
    return [...map.values()].sort((a, b) => a.person.name.localeCompare(b.person.name));
  });

  /** Rescind the whole connection with a person: every node of theirs
   *  goes back to unclaimed, every grant (and any route riding one) goes
   *  with it. */
  stopSharingWith(personId: string) {
    let name = "";
    for (const n of this.catalog.nodes) {
      if (n.relationship.kind === "shared" && n.relationship.person.id === personId) {
        name = n.relationship.person.name;
        n.relationship = { kind: "unclaimed" };
      }
    }
    this.reauthorize();
    if (name) this.toast("info", `Stopped sharing with ${name}`);
  }

  grant(nodeId: string, grant: Grant) {
    const n = this.node(nodeId);
    if (!n || n.relationship.kind !== "shared") return;
    const pid = n.relationship.person.id;
    // De-dupe by (media, role, capability) across the *person* — a grant
    // authorizes them wherever it happens to be recorded.
    const exists = this.catalog.nodes.some(
      (x) =>
        x.relationship.kind === "shared" &&
        x.relationship.person.id === pid &&
        x.relationship.grants.some(
          (g) =>
            g.media === grant.media && g.role === grant.role && g.capability === grant.capability,
        ),
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
