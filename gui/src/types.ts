// TypeScript mirror of `allmystuff-graph`'s model + `allmystuff-protocol`'s
// presence shapes. Kept in sync by hand against the Rust source — a drift
// shows up as a decode error when the Tauri backend hands real data over.
// (Same discipline as the MyOwnMesh GUI's `types.ts`.)

export type MediaKind = "audio" | "video" | "display" | "input" | "storage" | "generic";
export type Flow = "source" | "sink" | "duplex";
export type GrantRole = "provide" | "consume" | "both";
export type NodeKind = "this" | "machine";

export interface Capability {
  id: string;
  node: string;
  label: string;
  media: MediaKind;
  flow: Flow;
  origin: string;
  /** `true` when this is the node's current default for its device category
   *  (the mic it captures from, the screen it drives first…). The drawer
   *  badges it and routing prefers it. Mirrors the Rust `Capability.default`;
   *  optional so older presence without the field still decodes. */
  default?: boolean;
}

export interface Person {
  id: string;
  name: string;
}

export interface Grant {
  id: string;
  media: MediaKind;
  role: GrantRole;
  capability?: string | null;
  label: string;
}

export interface Share {
  person: Person;
  grants: Grant[];
}

// Relationship is internally tagged on `kind`. `mine` = a device you own or
// manage; `shared` = someone you're connecting with for specific purposes;
// `unclaimed` = on your mesh but not yet classified (a GUI-only state for
// freshly-discovered peers — you claim it as mine or mark it shared). The
// Rust model only knows `mine`/`shared`; `unclaimed` never crosses the wire.
export type Relationship =
  | { kind: "mine" }
  | { kind: "unclaimed" }
  | ({ kind: "shared" } & Share);

export interface InventorySummary {
  os: string;
  cpu: string;
  ram_bytes: number;
  device_count: number;
}

export interface MeshNode {
  id: string;
  label: string;
  /** The node's real machine hostname (from its scan / presence advert).
   *  When `label` is a user override that differs from this, the UI shows
   *  "label (hostname)" so the true machine is always visible. */
  hostname?: string;
  kind: NodeKind;
  relationship: Relationship;
  online: boolean;
  /** Hardware thumbnail for the node card (from the peer's presence advert,
   *  or this machine's own scan). Not part of the Rust `MeshNode` — the GUI
   *  carries it alongside for display. */
  summary?: InventorySummary;
  /** `true` once we've heard this node's AllMyStuff presence advert — i.e.
   *  it's actually *running AllMyStuff*, not just a device on the mesh. A
   *  node that's only known from the daemon's roster/peers (no app) has no
   *  wireable capabilities, so the graph shows it but makes it un-targetable
   *  and visually quieter. The local machine is always an app node. */
  app?: boolean;
  /** The node id that owns this device, from its presence advert. `owner`
   *  equal to the local node means the device says *you* own it. */
  owner?: string | null;
  /** `true` when the device is in claim mode and unowned — it's offering
   *  itself for adoption, the only state in which a claim takes. */
  claimable?: boolean;
  /** App features this node advertises in presence ("terminal", …).
   *  Absent (an older peer) means none — the matching buttons stay hidden. */
  features?: string[];
  /** Friendly names of the networks this device has been seen on. You can be
   *  on several networks at once and a device may share only some of them, so
   *  the graph shows which — it's never just "the" mesh. */
  networks?: string[];
}

/** The presence feature tag for mesh-native terminal hosting (mirrors the
 *  Rust `FEATURE_TERMINAL`). */
export const FEATURE_TERMINAL = "terminal";

/** The presence feature tag for mesh-native file hosting — the "Open
 *  Files" console (mirrors the Rust `FEATURE_FILES`). */
export const FEATURE_FILES = "files";

/** The presence feature tag for camera *streaming* (mirrors the Rust
 *  `FEATURE_CAMERA`): the node's video routes have a capture backend
 *  behind them. Cameras have always ridden presence as capabilities;
 *  without this tag the machine advertising them predates the transport,
 *  and the console says so instead of waiting on pixels that never come. */
export const FEATURE_CAMERA = "camera";

/** Whether a node is actually running AllMyStuff (vs. a bare mesh device).
 *  The local node and any node we've had presence from count as app nodes. */
export function isAppNode(n: { kind?: NodeKind; app?: boolean }): boolean {
  return n.kind === "this" || n.app === true;
}

export interface Route {
  id: string;
  from: string;
  to: string;
  media: MediaKind;
}

export interface Catalog {
  nodes: MeshNode[];
  capabilities: Capability[];
  routes: Route[];
}

// ---- networks · identity · roster (mirror the daemon control shapes) --

/** This device's mesh identity (from `mesh_identity`). `label` is the
 *  user's display-name override; empty means "use the hostname". */
export interface IdentityInfo {
  device_id: string;
  pubkey?: string;
  label: string;
}

/** A network the daemon knows about (from `mesh_networks`). `config_id` is
 *  the stable local key for control ops; `network_id` is the shareable
 *  handle peers join with; `label` is an optional cosmetic name. */
export interface NetworkSummary {
  config_id: string;
  network_id: string;
  label: string;
  phase?: string;
}

/** Friendly name for a network: cosmetic label, else the joinable id, else
 *  the internal config id. */
export function networkDisplayName(n: {
  label?: string | null;
  network_id?: string;
  config_id?: string;
}): string {
  return n.label?.trim() || n.network_id?.trim() || n.config_id || "";
}

/** An approved member of a network's roster (from `mesh_roster_list`). */
export interface RosterPeer {
  device_id: string;
  label: string;
  approved_at?: number;
}

// ---- per-network transport config (signaling · STUN · TURN) -----------
//
// The daemon's NetworkConfig shape, round-tripped via `mesh_config_show` /
// `mesh_network_update`. Loosely typed — we preserve fields we don't edit.

export interface StunServerCfg {
  urls: string[];
}
export interface TurnServerCfg {
  urls: string[];
  username?: string | null;
  credential?: string | null;
}
export interface SignalingCfg {
  kind?: string;
  servers?: string[];
  redundancy?: number;
  denylist?: string[];
  public_fallback?: boolean;
}
/** A full network config as the daemon stores it. We only edit the server
 *  fields; everything else is preserved on round-trip. */
export interface NetworkConfigFull {
  id: string;
  network_id: string;
  label?: string | null;
  signaling?: SignalingCfg;
  stun_servers?: StunServerCfg[];
  turn_servers?: TurnServerCfg[];
  [key: string]: unknown;
}

/** A TURN entry as the Servers editor works with it (one url + optional
 *  creds), mapped to/from the daemon's `{ urls: [...] }` shape. */
export interface TurnEntry {
  url: string;
  username: string;
  credential: string;
}

/** A live peer on a network (from `mesh_peers`). We only surface the bits the
 *  approvals UI needs; `status === "pending_approval"` is the one to act on.
 *  The suffix + the two verification codes are the four cells of the approval
 *  code grid; `local_approve_sent` / `remote_approve_seen` drive the
 *  fresh / waiting-for-peer / confirm states (mirrors the daemon's PeerInfo). */
export interface PeerInfo {
  device_id: string;
  label: string;
  status: string;
  device_suffix?: string;
  verification_code_received?: string | null;
  verification_code_sent?: string | null;
  local_approve_sent?: boolean;
  remote_approve_seen?: boolean;
}

// ---- owned fleet (the gossiped "Owned" roster) ------------------------

/** One device in your owned fleet — a machine you claimed, sharing the
 *  fleet's key. `device` is the canonical (bare-pubkey) id; the graph
 *  reconciles it to the right node. */
export interface OwnedMember {
  device: string;
  label: string;
}

/** The fleet roster (from `owned_roster` / the `allmystuff://owned` event):
 *  the shared key that links your co-owned devices and the members it links.
 *  An empty `key` means you haven't claimed anything yet. */
export interface OwnedRoster {
  key: string;
  /** The fleet's display name ("Casey") — cosmetic, gossiped with the
   *  roster. Absent/empty = unnamed (an older peer never sends it). */
  name?: string;
  version: number;
  members: OwnedMember[];
}

// ---- self-update (mirrors `allmystuff-updater`) -----------------------

export type InstallKind = "raw" | "package_manager";

/** Updater status (from `update_status`). */
export interface UpdateStatus {
  current_version: string;
  install_kind: InstallKind;
  enabled: boolean;
  /** "stable" | "beta". */
  channel: string;
  /** Auto-apply policy: "patch" | "minor" | "all" | "none". */
  auto_apply: string;
  check_interval_hours: number;
  last_check_at: number | null;
  staged_version: string | null;
  release_url: string;
  release_url_overridden: boolean;
}

/** Result of a manual check (from `update_check`). Tagged on `outcome`. */
export interface CheckOutcome {
  outcome:
    | "disabled"
    | "package_manager"
    | "not_due"
    | "up_to_date"
    | "policy_blocked"
    | "staged";
  current?: string;
  latest?: string;
  policy?: string;
  version?: string;
}

/** The bits of updater config the UI can change (sent to `update_set_prefs`). */
export interface UpdatePrefs {
  enabled?: boolean;
  channel?: string;
  auto_apply?: string;
  check_interval_hours?: number;
  stable_url?: string;
  beta_url?: string;
}

// ---- console media (mirrors `allmystuff-session`'s media frames) ------

/** One keyboard/mouse event the console forwards down an input route.
 *  Tagged exactly like the Rust `InputAction` (serde `kind`, snake_case).
 *  Mouse coordinates are normalized 0..1 over the remote screen the
 *  console is showing; `screen` names which one (the `screen:<id>`
 *  capability's id), absent for the primary — so control follows the
 *  selected tab. Key events carry the physical `code` alongside the
 *  layout-resolved `key`: combinations resolve through it on the far
 *  side (Ctrl+C must land on the C key, whatever character the held
 *  modifiers composed here). */
export type InputAction =
  | { kind: "mouse_move"; x: number; y: number; screen?: number }
  | { kind: "mouse_button"; button: number; down: boolean }
  | { kind: "wheel"; dx: number; dy: number }
  | { kind: "key"; key: string; code?: string; down: boolean };

/** One terminal event a terminal window sends down its route. Tagged
 *  exactly like the Rust `TermEvent` (serde `kind`, snake_case); `bytes`
 *  is base64 (the wire is JSON). `exit` is the host's word only — it never
 *  travels viewer → host. */
export type TermEvent =
  | { kind: "data"; bytes: string }
  | { kind: "resize"; cols: number; rows: number }
  | { kind: "exit"; code?: number | null };

/** One entry of a remote directory listing (mirrors the Rust `FileEntry`). */
export interface FileEntry {
  name: string;
  dir: boolean;
  size: number;
  modified?: number | null;
  symlink?: boolean;
}

/** One event of a files route — the request/response conversation between
 *  the file-manager viewer and the host whose disk it browses. Tagged
 *  exactly like the Rust `FileEvent` (serde `kind`, snake_case); `data`
 *  is base64 (the wire is JSON). Every event carries the viewer-minted
 *  request id (`req`) it belongs to. */
export type FileEvent =
  | { kind: "list"; req: number; path: string }
  | { kind: "read"; req: number; path: string }
  | { kind: "write"; req: number; path: string; data: string; append?: boolean; eof?: boolean }
  | { kind: "mkdir"; req: number; path: string }
  | { kind: "rename"; req: number; from: string; to: string }
  | { kind: "delete"; req: number; path: string }
  | { kind: "entries"; req: number; path: string; home: string; entries: FileEntry[] }
  | { kind: "chunk"; req: number; data: string; total: number; eof?: boolean }
  | { kind: "ok"; req: number }
  | { kind: "err"; req: number; reason: string };

/** A route's live negotiation state from the session snapshot — what a
 *  terminal tab watches to tell "connecting" from "rejected (reason)". */
export interface RouteLiveState {
  state: string;
  reason?: string;
}

/** One video packet of a display route, decoded off its IPC channel (a
 *  fixed binary header + the payload — see `watchVideo`). Either a
 *  standalone JPEG frame (the MJPEG transport) or one H.264 access unit
 *  from the mesh's track lane, for WebCodecs. */
export interface VideoFrameMsg {
  /** `raw` = RGBA the backend already decoded (paint it, no codec). */
  kind: "jpeg" | "h264" | "raw";
  /** H.264: this unit is an IDR — a safe decoder entry point. */
  key: boolean;
  /** JPEG/raw only — an H.264 unit carries its dimensions in the SPS. */
  width: number;
  height: number;
  sourceWidth: number;
  sourceHeight: number;
  /** JPEG: the frame's seq. H.264/raw: presentation timestamp in µs. */
  seq: number;
  /** The payload bytes — never base64. */
  data: Uint8Array<ArrayBuffer>;
}

// ---- virtual rooms ----------------------------------------------------
//
// A room is a lightweight, user-minted gathering of machines — a zoom-like
// call you join with everything off. Its membership + chat plane rides the
// `allmystuff/rooms/v1` channel (mirrors the Rust `RoomMessage`); the media
// itself (mic, screen, sound, control) is ordinary routes, proposed and
// authorized exactly like any other connection.

/** How a room admits a machine that knocks (asks to join with the room's
 *  id but no invite). Absent — an older host, an old save — reads as
 *  `invite`: never more open than the host meant. */
export type RoomAccess = "open" | "invite";

/** A room as the rooms bar lists it. `members` are canonical (bare-pubkey)
 *  node ids and include this machine. Once a room lands here — made here,
 *  or invited into — it stays like a roster slot: listed and rejoinable
 *  until the host removes this device or closes the room. */
export interface VirtualRoom {
  id: string;
  name: string;
  members: string[];
  /** Canonical id of the room's **host** — its maker. The room's
   *  identity is minted under this device (`room:{host}:{nonce}`), and
   *  its control plane (roster, name, closing it) answers to it alone.
   *  Absent on rooms minted before the field (or stubbed from a stray
   *  chat), which leaves those controls open to whoever holds the copy. */
  owner?: string;
  /** The host's knock policy, restated on every invite. */
  access?: RoomAccess;
}

/** One line of a room's chat (kept in memory for the session). */
export interface RoomChatLine {
  /** Canonical node id of the sender ("" for system notes). */
  from: string;
  /** Display name resolved at receive time, so lines survive a peer
   *  dropping off the graph. */
  fromLabel: string;
  text: string;
  at: number;
}

/** A wire message of the rooms plane (mirrors the Rust `RoomMessage` —
 *  tagged on `kind`, with the room id + name restated on every message).
 *  `invite` (the roster/name/access replacement), `close` and `deny` are
 *  the **host's** alone; receivers ignore them from anyone else. `knock`
 *  travels the other way — a machine holding the room's id (pasted into
 *  the rooms UI) asking the host to let it in. */
export type RoomWireMessage = { room: string; name?: string } & (
  | { kind: "invite"; members: string[]; access?: RoomAccess }
  | { kind: "join" }
  | { kind: "leave" }
  | { kind: "chat"; text: string }
  | { kind: "close" }
  | { kind: "knock" }
  | { kind: "deny" }
);

/** The presence feature tag for the rooms plane (mirrors the Rust
 *  `FEATURE_ROOMS`). A member without it (an older build) never sees the
 *  room's invites or chat — the room panel badges them. */
export const FEATURE_ROOMS = "rooms";

// ---- visual helpers ---------------------------------------------------

/** How a node's name reads on screen: the chosen name, with the real machine
 *  hostname in parens when the name is an override. Implements the naming
 *  rule — default is the hostname; an override shows as "Override (hostname)". */
export function displayName(n: { label: string; hostname?: string }): string {
  const host = n.hostname?.trim();
  if (host && host !== n.label) return `${n.label} (${host})`;
  return n.label;
}

export const MEDIA: Record<MediaKind, { label: string; color: string; icon: string }> = {
  audio: { label: "Audio", color: "var(--m-audio)", icon: "🎙" },
  video: { label: "Video", color: "var(--m-video)", icon: "🎬" },
  display: { label: "Screen", color: "var(--m-display)", icon: "🖥" },
  input: { label: "Controls", color: "var(--m-input)", icon: "⌨️" },
  storage: { label: "Files", color: "var(--m-storage)", icon: "🗂" },
  generic: { label: "Data", color: "var(--m-data)", icon: "📦" },
};

export function mediaColor(m: MediaKind): string {
  return MEDIA[m].color;
}

/** A friendly glyph for a capability based on what kind of device it is. */
export function originIcon(origin: string, media: MediaKind): string {
  const map: Record<string, string> = {
    microphone: "🎙",
    speaker: "🔊",
    camera: "📷",
    display: "🖥",
    screen: "🪟",
    control: "🕹",
    controller: "⌨️",
    terminal: "📟",
    "terminal-view": "📟",
    files: "🗂",
    "files-view": "🗂",
    keyboard: "⌨️",
    mouse: "🖱",
    touchpad: "🖱",
    gamepad: "🎮",
    storage: "🗂",
    system: "🔉",
    viewer: "📺",
  };
  return map[origin] ?? MEDIA[media].icon;
}

export function flowArrow(flow: Flow): string {
  return flow === "source" ? "→" : flow === "sink" ? "←" : "↔";
}

/** "out", "in", or "both" — plain words for the consumer UI. */
export function flowWord(flow: Flow): string {
  return flow === "source" ? "sends" : flow === "sink" ? "receives" : "both ways";
}

export function humanBytes(bytes: number): string {
  if (!bytes) return "0 B";
  const u = ["B", "KB", "MB", "GB", "TB", "PB"];
  let v = bytes;
  let i = 0;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${v >= 100 || i === 0 ? Math.round(v) : v.toFixed(1)} ${u[i]}`;
}
