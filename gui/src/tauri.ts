// Thin bridge to the Tauri backend. Everything here degrades gracefully
// when the app runs as a plain web page (no Tauri) — `pnpm dev` in a
// browser, this repo's CI build — so the graph is always interactive even
// without the Rust side or a running `myownmesh` daemon.

import type {
  Capability,
  CheckOutcome,
  IdentityInfo,
  InventorySummary,
  MediaKind,
  NetworkConfigFull,
  NetworkSummary,
  OwnedRoster,
  PeerInfo,
  RosterPeer,
  UpdatePrefs,
  UpdateStatus,
} from "./types";

interface ScanResult {
  node_id: string;
  /** This machine's display name (hostname unless overridden). */
  label?: string;
  /** This machine's real hostname. */
  hostname?: string;
  summary: InventorySummary;
  capabilities: Capability[];
}

/** Live session snapshot from the backend: the peers presence has found
 *  and the routes currently negotiating/streaming. Mirrors the JSON the
 *  Rust `mesh::Mesh::snapshot` emits. */
export interface SessionSnapshot {
  ready: boolean;
  me?: string;
  network?: string | null;
  peers?: Array<{
    node: string;
    label: string;
    hostname?: string;
    summary: InventorySummary;
    capabilities: Capability[];
    /** From the peer's presence advert (Task 4): who owns it, and whether
     *  it's currently offering itself for adoption. */
    owner?: string | null;
    claimable?: boolean;
  }>;
  routes?: Array<{
    route: { id: string; from: string; to: string; media: MediaKind };
    peer: string;
    origin: "outbound" | "inbound";
    state: { state: string; reason?: string };
  }>;
}

export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

// ---- app metadata -----------------------------------------------------
//
// The running build's version, for showing in the UI like MyOwnMesh /
// MyOwnLLM do. Tauri's source of truth is `gui/src-tauri/Cargo.toml`
// (kept in sync with `gui/package.json` by `scripts/bump-version.sh`), so
// this stays correct after every `just release`. Both helpers degrade to a
// no-op in web mode — the in-browser preview has no Tauri runtime.

/** The running app's version (e.g. "0.1.0"), or null in web mode. */
export async function appVersion(): Promise<string | null> {
  if (!isTauri()) return null;
  try {
    const { getVersion } = await import("@tauri-apps/api/app");
    return await getVersion();
  } catch (e) {
    console.warn("app version unavailable:", e);
    return null;
  }
}

/** Stamp the native window title so the running build is identifiable at a
 *  glance. No-op (and harmless) in web mode. */
export async function setWindowTitle(title: string): Promise<void> {
  if (!isTauri()) return;
  try {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    await getCurrentWindow().setTitle(title);
  } catch (e) {
    console.warn("set window title failed:", e);
  }
}

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

/** Scan this machine. Returns null in web mode; the caller keeps its demo
 *  data. `node_id` is the mesh device id once the session is up. */
export function scanSelf(): Promise<ScanResult | null> {
  return tryInvoke<ScanResult>("scan_self");
}

/** Offer a real connection over the mesh. Returns the route id, or null in
 *  web mode (the store falls back to a local route for the demo). */
export function connectRoute(from: string, to: string, media: MediaKind): Promise<string | null> {
  return tryInvoke<string>("connect_route", { from, to, media });
}

export function disconnectRoute(routeId: string): Promise<null> {
  return tryInvoke("disconnect_route", { routeId });
}

/** Claim a device as yours. The claim only takes if that device is in claim
 *  mode; its next presence advert (owner = us) confirms it. Returns null in
 *  web mode (the store simulates the claim on the demo graph). */
export function claimNode(node: string): Promise<null> {
  return tryInvoke("claim_node", { node });
}

/** Put this device into / out of claim mode so another of your machines can
 *  adopt it. Returns whether it's now claimable (null in web mode). */
export function setClaimable(claimable: boolean): Promise<boolean | null> {
  return tryInvoke<boolean>("set_claimable", { claimable });
}

/** Ownership feedback from the mesh — a `claimed` / `declined` reply to a
 *  claim we sent. No-op listener in web mode. */
export async function onOwnership(
  cb: (o: { from: string; message: { kind: string; reason?: string } }) => void,
): Promise<() => void> {
  if (!isTauri()) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  return listen<{ from: string; message: { kind: string; reason?: string } }>(
    "allmystuff://ownership",
    (e) => cb(e.payload),
  );
}

export function sessionSnapshot(): Promise<SessionSnapshot | null> {
  return tryInvoke<SessionSnapshot>("session_snapshot");
}

// ---- owned fleet (the "Owned" roster) ---------------------------------

/** The current owned-fleet roster (shared key + members). Null in web mode —
 *  the store simulates a fleet on the demo graph. */
export function ownedRoster(): Promise<OwnedRoster | null> {
  return tryInvoke<OwnedRoster>("owned_roster");
}

/** Subscribe to live fleet-roster updates (after a claim, or when gossip
 *  converges). Returns an unlisten fn (no-op in web mode). */
export async function onOwned(cb: (r: OwnedRoster) => void): Promise<() => void> {
  if (!isTauri()) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  return listen<OwnedRoster>("allmystuff://owned", (e) => cb(e.payload));
}

// ---- self-update -------------------------------------------------------
//
// These degrade to null in web mode (no backend), so the Updates settings
// section can render a friendly "desktop only" note instead of throwing.

export function updateStatus(): Promise<UpdateStatus | null> {
  return tryInvoke<UpdateStatus>("update_status");
}

export function updateCheck(): Promise<CheckOutcome | null> {
  return tryInvoke<CheckOutcome>("update_check");
}

export function updateApply(): Promise<{ applied: string | null } | null> {
  return tryInvoke<{ applied: string | null }>("update_apply");
}

export function updateSetPrefs(prefs: UpdatePrefs): Promise<UpdateStatus | null> {
  return tryInvoke<UpdateStatus>("update_set_prefs", { prefs });
}

/** Subscribe to live session snapshots. Returns an unlisten fn (or a no-op
 *  in web mode). */
export async function onSession(cb: (snap: SessionSnapshot) => void): Promise<() => void> {
  if (!isTauri()) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  return listen<SessionSnapshot>("allmystuff://session", (e) => cb(e.payload));
}

export async function onSubscription(cb: (s: { status: string }) => void): Promise<() => void> {
  if (!isTauri()) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  return listen<{ status: string }>("allmystuff://subscription", (e) => cb(e.payload));
}

// ---- networks · identity · roster -------------------------------------
//
// Unlike the graph commands above (which degrade to null in web mode), these
// require a real daemon — the Networks panel only renders under Tauri. They
// throw on error so the UI can surface "couldn't create network", etc.

async function invokeReq<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauri()) throw new Error("Networks need the desktop app (no backend in the browser).");
  const { invoke } = await import("@tauri-apps/api/core");
  return (await invoke(cmd, args)) as T;
}

export const meshIdentity = () => invokeReq<IdentityInfo>("mesh_identity");

/** Set this device's display-name override (empty resets to the hostname). */
export const meshIdentitySetLabel = (label: string) =>
  invokeReq<unknown>("mesh_identity_set_label", { label });

// The daemon wraps these lists in an object (`{ networks }`, `{ peers }`,
// `{ roster }`) — matching MyOwnMesh's client, we unwrap to the inner array
// and never hand a non-array back (a non-array would crash the graph).
export async function meshNetworks(): Promise<NetworkSummary[]> {
  const r = await invokeReq<{ networks?: NetworkSummary[] }>("mesh_networks");
  return Array.isArray(r?.networks) ? r.networks : [];
}

export async function meshNetworkIdGenerate(): Promise<string> {
  const r = await invokeReq<{ network_id: string }>("mesh_network_id_generate");
  return r.network_id;
}

export const meshNetworkAdd = (config: unknown) =>
  invokeReq<unknown>("mesh_network_add", { config });

export const meshNetworkUpdate = (config: unknown) =>
  invokeReq<unknown>("mesh_network_update", { config });

export const meshNetworkRemove = (network: string) =>
  invokeReq<unknown>("mesh_network_remove", { network });

/** The whole daemon config (every network with its full signaling/STUN/TURN).
 *  The Servers settings pane reads this to populate its editor. */
export async function meshConfigShow(): Promise<NetworkConfigFull[]> {
  const r = await invokeReq<{ config?: { networks?: NetworkConfigFull[] } }>("mesh_config_show");
  return Array.isArray(r?.config?.networks) ? r.config!.networks! : [];
}

export async function meshRosterList(network: string): Promise<RosterPeer[]> {
  const r = await invokeReq<{ roster?: RosterPeer[] }>("mesh_roster_list", { network });
  return Array.isArray(r?.roster) ? r.roster : [];
}

export const meshRosterApprove = (network: string, deviceId: string, label?: string) =>
  invokeReq<unknown>("mesh_roster_approve", { network, deviceId, label });

export const meshRosterRemove = (network: string, deviceId: string) =>
  invokeReq<unknown>("mesh_roster_remove", { network, deviceId });

export async function meshPeers(network: string): Promise<PeerInfo[]> {
  const r = await invokeReq<{ peers?: PeerInfo[] }>("mesh_peers", { network });
  return Array.isArray(r?.peers) ? r.peers : [];
}

// MyOwnMesh's semi-public reference servers — the defaults a new network
// uses so two devices rendezvous on the *same* signaling relay (the usual
// reason "nothing connects": peers scattered across different public relays)
// and can traverse NAT via the shared STUN/TURN. All three are editable per
// network from Settings → Networks → Servers.
export const MYOWNMESH_SIGNALING = "wss://myownmesh.com";
export const MYOWNMESH_STUN = "stun:stun.myownmesh.com:3478";
export const MYOWNMESH_TURN_URL = "turn:turn.myownmesh.com:3478";
export const MYOWNMESH_TURN_USER = "guest";
export const MYOWNMESH_TURN_PASS = "theguestpassword";

function newNetworkInternalId(): string {
  return `net_${Math.random().toString(36).slice(2, 10)}_${Date.now().toString(36)}`;
}

/** Build the NetworkConfig payload `mesh_network_add` expects. Defaults the
 *  signaling relay + STUN + TURN to MyOwnMesh's reference servers so a freshly
 *  created/joined network connects out of the box; the Servers pane can change
 *  any of them later. */
export function buildNetworkConfig(args: {
  networkId: string;
  label?: string;
  autoApprove?: boolean;
}): Record<string, unknown> {
  return {
    id: newNetworkInternalId(),
    network_id: args.networkId,
    label: args.label?.trim() || undefined,
    signaling: { servers: [MYOWNMESH_SIGNALING] },
    stun_servers: [{ urls: [MYOWNMESH_STUN] }],
    turn_servers: [
      { urls: [MYOWNMESH_TURN_URL], username: MYOWNMESH_TURN_USER, credential: MYOWNMESH_TURN_PASS },
    ],
    auto_approve: args.autoApprove ?? false,
  };
}
