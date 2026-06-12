// Thin bridge to the Tauri backend. Everything here degrades gracefully
// when the app runs as a plain web page (no Tauri) — `pnpm dev` in a
// browser, this repo's CI build — so the graph is always interactive even
// without the Rust side or a running `myownmesh` daemon.

import type {
  Capability,
  CheckOutcome,
  FileEvent,
  RoomWireMessage,
  IdentityInfo,
  InputAction,
  InventorySummary,
  MediaKind,
  NetworkConfigFull,
  NetworkSummary,
  OwnedRoster,
  PeerInfo,
  RosterPeer,
  TermEvent,
  UpdatePrefs,
  UpdateStatus,
  VideoFrameMsg,
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
    /** App features the peer advertises ("terminal", …). Absent from an
     *  older peer — same as empty. */
    features?: string[];
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
 *  web mode (the store falls back to a local route for the demo). A
 *  display route advertises H.264 by default (the streaming side then uses
 *  the mesh's RTP track lane): decode is covered everywhere — WebCodecs
 *  where the webview has it, the backend's native openh264 decoder where
 *  it doesn't — and the backend still withholds the offer when the local
 *  daemon predates the track lane. MJPEG stays the floor both ends share,
 *  and `codec: "mjpeg"` forces it (the console's codec pill). */
export function connectRoute(
  from: string,
  to: string,
  media: MediaKind,
  codec?: "auto" | "h264" | "mjpeg",
): Promise<string | null> {
  const video = media === "display" && codec !== "mjpeg" ? ["h264"] : [];
  return tryInvoke<string>("connect_route", { from, to, media, video });
}

/** The console's quality picks for a stream it's watching — each absent
 *  field means "automatic". The far end restarts its capture with these. */
export interface StreamTune {
  maxEdge?: number;
  bitrate?: number;
  fps?: number;
}

/** Ask the sender of `routeId` to stream with these picks. Best-effort:
 *  an old peer drops the ask and stays on automatic. */
export function tuneRoute(routeId: string, tune: StreamTune): Promise<null> {
  return tryInvoke("tune_route", {
    routeId,
    maxEdge: tune.maxEdge ?? null,
    bitrate: tune.bitrate ?? null,
    fps: tune.fps ?? null,
  });
}

/** Ask the sender of `routeId` for a clean decode entry (IDR) now — call
 *  from a decode-error handler. Rate-limited backend-side. */
export function refreshRoute(routeId: string): Promise<null> {
  return tryInvoke("video_refresh", { routeId });
}

export function disconnectRoute(routeId: string): Promise<null> {
  return tryInvoke("disconnect_route", { routeId });
}

/** Claim a device as yours. The claim only takes if that device is in claim
 *  mode; its next presence advert (owner = us) confirms it. Throws when the
 *  backend couldn't deliver the ask (device dropped offline, no shared
 *  network) so the UI can say so instead of waiting forever. No-op in web
 *  mode (the store simulates the claim on the demo graph). */
export async function claimNode(node: string): Promise<void> {
  if (!isTauri()) return;
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke("claim_node", { node });
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

// ---- remote console (per-machine windows + the media plane) ------------

/** Open (or focus) the dedicated console window for `node`. Desktop only —
 *  the web preview keeps its in-page console. */
export async function openConsoleWindow(node: string): Promise<void> {
  if (!isTauri()) return;
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke("open_console_window", { node });
}

/** Which machine this window is a console for, when the window was opened
 *  by `openConsoleWindow` (`?console=<node id>`). Null in the main window. */
export function consoleWindowTarget(): string | null {
  if (typeof window === "undefined") return null;
  return new URLSearchParams(window.location.search).get("console");
}

/** Forward one keyboard/mouse event down an active outbound input route. */
export function sendInput(routeId: string, action: InputAction): Promise<null> {
  return tryInvoke("send_input", { routeId, action });
}

/** Decode one wire packet (28-byte little-endian header + payload) out
 *  of a poll batch. Returns null for shapes we don't recognize. */
function parseVideoPacket(buf: ArrayBuffer, offset: number, len: number): VideoFrameMsg | null {
  if (len < 28) return null;
  const head = new DataView(buf, offset, 28);
  const kindByte = head.getUint8(0);
  if (kindByte !== 1 && kindByte !== 2 && kindByte !== 3) return null;
  return {
    kind: kindByte === 3 ? "raw" : kindByte === 2 ? "h264" : "jpeg",
    key: (head.getUint8(1) & 1) === 1,
    width: head.getUint32(4, true),
    height: head.getUint32(8, true),
    sourceWidth: head.getUint32(12, true),
    sourceHeight: head.getUint32(16, true),
    seq: Number(head.getBigUint64(20, true)),
    data: new Uint8Array(buf.slice(offset + 28, offset + len)),
  };
}

/** Stream one route's inbound video into this window by *pulling*: the
 *  backend queues raw packets per route and this drains them every
 *  display tick (`video_poll` → `[u32 len][packet]…`). A failed poll
 *  costs one tick and the next one recovers — unlike a push channel,
 *  where ordered delivery means one lost message silently freezes the
 *  stream while the backend keeps sending. `opts.decode` asks the backend
 *  to decode H.264 natively and deliver ready-to-paint RGBA (`raw`)
 *  packets — for webviews without WebCodecs, and the bottom rung of the
 *  console's decode ladder. Returns an unwatch fn (a no-op in web mode,
 *  where no frames can arrive anyway). */
export async function watchVideo(
  routeId: string,
  cb: (f: VideoFrameMsg) => void,
  opts?: { decode?: boolean },
): Promise<() => void> {
  if (!isTauri()) return () => {};
  const { invoke } = await import("@tauri-apps/api/core");
  const { listen } = await import("@tauri-apps/api/event");
  const token = (await invoke("video_watch", {
    routeId,
    decode: opts?.decode ?? false,
  })) as number;
  let stopped = false;
  let inFlight = false;
  const tick = async () => {
    if (stopped || inFlight) return;
    inFlight = true;
    try {
      const batch = (await invoke("video_poll", { routeId })) as ArrayBuffer;
      if (stopped || !(batch instanceof ArrayBuffer)) return;
      const view = new DataView(batch);
      let offset = 0;
      while (offset + 4 <= batch.byteLength) {
        const len = view.getUint32(offset, true);
        offset += 4;
        if (len === 0 || offset + len > batch.byteLength) break;
        const packet = parseVideoPacket(batch, offset, len);
        offset += len;
        if (packet) cb(packet);
      }
    } catch {
      // One missed poll; the next tick drains everything queued.
    } finally {
      inFlight = false;
    }
  };
  // Drain on the backend's "queue went non-empty" poke — event delivery
  // isn't timer-throttled, so an occluded (non-maximized) console keeps
  // painting at full rate, and arrival-driven pulls beat the interval's
  // worst-case 16 ms. The interval stays as the safety net.
  const unlisten = await listen<string>("allmystuff://video-ready", (e) => {
    if (e.payload === routeId) void tick();
  });
  const timer = setInterval(() => void tick(), 16);
  return () => {
    stopped = true;
    clearInterval(timer);
    unlisten();
    void invoke("video_unwatch", { routeId, token }).catch(() => {});
  };
}

/** A display-route host explaining its capture state in-band (`vstat`
 *  media frames): why frames aren't flowing — consent dialog pending on
 *  the host, display asleep, no monitor, grabs failing — or `ok` when
 *  they are (again). */
export type VideoHostStatus = {
  route: string;
  state: "ok" | "waiting_consent" | "display_asleep" | "no_monitor" | "grab_failed";
  detail?: string | null;
};

/** Listen for the host's capture-status reports on `routeId`, so the
 *  console can explain a black stage instead of just showing one. No-op
 *  in web mode. */
export async function watchVideoStatus(
  routeId: string,
  cb: (s: VideoHostStatus) => void,
): Promise<() => void> {
  if (!isTauri()) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  const unlisten = await listen<VideoHostStatus>("allmystuff://video-status", (e) => {
    if (e.payload.route === routeId) cb(e.payload);
  });
  return () => unlisten();
}

// ---- terminal (the mesh-native shell) ----------------------------------

/** Open (or focus) the dedicated terminal window for `node` — one window
 *  per machine, holding its terminal tabs. Desktop only; the web preview
 *  keeps its in-page terminal. */
export async function openTerminalWindow(node: string): Promise<void> {
  if (!isTauri()) return;
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke("open_terminal_window", { node });
}

/** Which machine this window is a terminal for, when the window was opened
 *  by `openTerminalWindow` (`?terminal=<node id>`). Null in the main window. */
export function terminalWindowTarget(): string | null {
  if (typeof window === "undefined") return null;
  return new URLSearchParams(window.location.search).get("terminal");
}

/** Send keystrokes or a resize down an active terminal route (this window
 *  is the viewer; the far end feeds the PTY). Throws when the backend
 *  refuses, so a dead tab is told apart from a slow one. */
export async function termSend(routeId: string, event: TermEvent): Promise<void> {
  if (!isTauri()) return;
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke("term_send", { routeId, event });
}

/** Stream one terminal route's output into this window by *pulling* —
 *  the exact shape `watchVideo` uses (the backend buffers from
 *  route-activation, pokes `allmystuff://term-ready` when the queue goes
 *  non-empty, and a safety interval catches lost pokes). The callback gets
 *  raw PTY bytes ready for `Terminal.write`. Returns an unwatch fn. */
export async function watchTerminal(
  routeId: string,
  cb: (bytes: Uint8Array) => void,
): Promise<() => void> {
  if (!isTauri()) return () => {};
  const { invoke } = await import("@tauri-apps/api/core");
  const { listen } = await import("@tauri-apps/api/event");
  const token = (await invoke("term_watch", { routeId })) as number;
  let stopped = false;
  let inFlight = false;
  const tick = async () => {
    if (stopped || inFlight) return;
    inFlight = true;
    try {
      const batch = (await invoke("term_poll", { routeId })) as ArrayBuffer;
      if (stopped || !(batch instanceof ArrayBuffer)) return;
      const view = new DataView(batch);
      let offset = 0;
      while (offset + 4 <= batch.byteLength) {
        const len = view.getUint32(offset, true);
        offset += 4;
        if (offset + len > batch.byteLength) break;
        if (len > 0) cb(new Uint8Array(batch.slice(offset, offset + len)));
        offset += len;
      }
    } catch {
      // One missed poll; the next tick drains everything queued.
    } finally {
      inFlight = false;
    }
  };
  const unlisten = await listen<string>("allmystuff://term-ready", (e) => {
    if (e.payload === routeId) void tick();
  });
  const timer = setInterval(() => void tick(), 50);
  void tick(); // drain whatever buffered before this window subscribed
  return () => {
    stopped = true;
    clearInterval(timer);
    unlisten();
    void invoke("term_unwatch", { routeId, token }).catch(() => {});
  };
}

/** The far shell ended (`allmystuff://term-exit`): which route, and the
 *  exit code when there was one (null = killed / no status). */
export async function onTermExit(
  cb: (e: { route: string; code: number | null }) => void,
): Promise<() => void> {
  if (!isTauri()) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  return listen<{ route: string; code: number | null }>("allmystuff://term-exit", (e) =>
    cb(e.payload),
  );
}

// ---- files (the mesh-native file manager) -------------------------------

/** Open (or focus) the dedicated files window for `node` — one window per
 *  machine, the finder-like view of its disk. Desktop only; the web
 *  preview keeps an in-page popover. */
export async function openFilesWindow(node: string): Promise<void> {
  if (!isTauri()) return;
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke("open_files_window", { node });
}

/** Which machine this window is a file manager for, when the window was
 *  opened by `openFilesWindow` (`?files=<node id>`). Null in the main
 *  window. */
export function filesWindowTarget(): string | null {
  if (typeof window === "undefined") return null;
  return new URLSearchParams(window.location.search).get("files");
}

/** Send one file request down an active files route (this window is the
 *  viewer; the far end owns the disk). Throws when the backend refuses. */
export async function fileSend(routeId: string, event: FileEvent): Promise<void> {
  if (!isTauri()) return;
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke("file_send", { routeId, event });
}

/** Stream one files route's responses into this window by *pulling* —
 *  the exact shape `watchTerminal` uses (the backend buffers from
 *  route-activation, pokes `allmystuff://file-ready` when the queue goes
 *  non-empty, and a safety interval catches lost pokes). Each buffered
 *  chunk is one JSON `FileEvent` frame; the callback gets it parsed.
 *  Returns an unwatch fn. */
export async function watchFiles(
  routeId: string,
  cb: (event: FileEvent) => void,
): Promise<() => void> {
  if (!isTauri()) return () => {};
  const { invoke } = await import("@tauri-apps/api/core");
  const { listen } = await import("@tauri-apps/api/event");
  const token = (await invoke("file_watch", { routeId })) as number;
  const decoder = new TextDecoder();
  let stopped = false;
  let inFlight = false;
  const tick = async () => {
    if (stopped || inFlight) return;
    inFlight = true;
    try {
      const batch = (await invoke("file_poll", { routeId })) as ArrayBuffer;
      if (stopped || !(batch instanceof ArrayBuffer)) return;
      const view = new DataView(batch);
      let offset = 0;
      while (offset + 4 <= batch.byteLength) {
        const len = view.getUint32(offset, true);
        offset += 4;
        if (len === 0 || offset + len > batch.byteLength) break;
        try {
          cb(JSON.parse(decoder.decode(new Uint8Array(batch, offset, len))) as FileEvent);
        } catch {
          // One unparseable frame; the stream's surviving frames stand.
        }
        offset += len;
      }
    } catch {
      // One missed poll; the next tick drains everything queued.
    } finally {
      inFlight = false;
    }
  };
  const unlisten = await listen<string>("allmystuff://file-ready", (e) => {
    if (e.payload === routeId) void tick();
  });
  const timer = setInterval(() => void tick(), 100);
  void tick(); // drain whatever buffered before this window subscribed
  return () => {
    stopped = true;
    clearInterval(timer);
    unlisten();
    void invoke("file_unwatch", { routeId, token }).catch(() => {});
  };
}

/** Route the coming `read` request's chunks straight into this machine's
 *  Downloads folder. Returns the destination path; completion lands as
 *  `allmystuff://file-saved`. Call *before* `fileSend`-ing the read so
 *  the first chunk can't race the registration. */
export async function fileDownload(routeId: string, req: number, name: string): Promise<string> {
  if (!isTauri()) throw new Error("Downloads need the desktop app");
  const { invoke } = await import("@tauri-apps/api/core");
  return (await invoke("file_download", { routeId, req, name })) as string;
}

/** A registered download finished (`allmystuff://file-saved`): where it
 *  landed, or the error that stopped it. */
export async function onFileSaved(
  cb: (e: { route: string; req: number; path: string | null; error: string | null }) => void,
): Promise<() => void> {
  if (!isTauri()) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  return listen<{ route: string; req: number; path: string | null; error: string | null }>(
    "allmystuff://file-saved",
    (e) => cb(e.payload),
  );
}

/** Progress of a registered download (`allmystuff://file-progress`),
 *  throttled backend-side. */
export async function onFileProgress(
  cb: (e: { route: string; req: number; written: number; total: number }) => void,
): Promise<() => void> {
  if (!isTauri()) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  return listen<{ route: string; req: number; written: number; total: number }>(
    "allmystuff://file-progress",
    (e) => cb(e.payload),
  );
}

/** Clipboard for the terminal: WebKitGTK's async clipboard API is flaky,
 *  so under Tauri these ride the clipboard-manager plugin; the web preview
 *  falls back to `navigator.clipboard`. */
export async function clipboardWrite(text: string): Promise<void> {
  if (isTauri()) {
    const { writeText } = await import("@tauri-apps/plugin-clipboard-manager");
    await writeText(text);
    return;
  }
  await navigator.clipboard.writeText(text);
}

export async function clipboardRead(): Promise<string> {
  if (isTauri()) {
    const { readText } = await import("@tauri-apps/plugin-clipboard-manager");
    return (await readText()) ?? "";
  }
  return navigator.clipboard.readText();
}

/** Open a link in the system browser (terminal web-links). Tauri routes it
 *  through the shell plugin; the web preview just opens a tab. */
export async function openExternal(url: string): Promise<void> {
  if (isTauri()) {
    const { open } = await import("@tauri-apps/plugin-shell");
    await open(url);
    return;
  }
  window.open(url, "_blank", "noopener");
}

/** Tear this window down (a console window's "End session"). Uses
 *  `destroy` rather than `close` so it never re-fires the close-requested
 *  handler — route teardown has already run by the time this is called. */
export async function closeThisWindow(): Promise<void> {
  if (!isTauri()) return;
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  await getCurrentWindow().destroy();
}

/** Run `cb` when the user closes this window via the OS chrome. The close
 *  is *held* (preventDefault) so the console can tear its routes down
 *  first and then finish the job with `closeThisWindow`. Returns an
 *  unlisten fn. */
export async function onThisWindowClose(cb: () => void): Promise<() => void> {
  if (!isTauri()) return () => {};
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  return getCurrentWindow().onCloseRequested((event) => {
    event.preventDefault();
    cb();
  });
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

/** Leave the fleet this device belongs to (also releases its owner).
 *  Throws with the backend's reason when refused. No-op in web mode —
 *  the store simulates it on the demo roster. */
export async function fleetLeave(): Promise<void> {
  if (!isTauri()) return;
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke("fleet_leave");
}

/** Kick a device out of the fleet. The backend enforces the rule — only a
 *  member may kick, never itself. Throws with the reason when refused. */
export async function fleetKick(device: string): Promise<void> {
  if (!isTauri()) return;
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke("fleet_kick", { device });
}

/** Name (or rename) the fleet (members only). Throws with the reason when
 *  refused; the renamed roster arrives back via `allmystuff://owned`. */
export async function fleetSetName(name: string): Promise<void> {
  if (!isTauri()) return;
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke("fleet_set_name", { name });
}

// ---- virtual rooms (the rooms plane) ------------------------------------

/** Fan one room-plane message out to `members` (canonical or display node
 *  ids; self is skipped backend-side). Resolves to how many members the
 *  daemon actually dispatched to — 0 in web mode, where nothing can flow
 *  and the room is local-only. */
export async function roomSend(members: string[], message: RoomWireMessage): Promise<number> {
  const n = await tryInvoke<number>("room_send", { members, message });
  return n ?? 0;
}

/** Inbound room-plane traffic (an invite, a join/leave, a chat line).
 *  No-op listener in web mode. */
export async function onRoom(
  cb: (e: { from: string; message: RoomWireMessage }) => void,
): Promise<() => void> {
  if (!isTauri()) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  return listen<{ from: string; message: RoomWireMessage }>("allmystuff://room", (e) =>
    cb(e.payload),
  );
}

/** Open (or focus) the dedicated window for one room — the call in its
 *  own OS window, full-screenable like a console. Desktop only; the web
 *  preview keeps the in-page panel. */
export async function openRoomWindow(roomId: string): Promise<void> {
  if (!isTauri()) return;
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke("open_room_window", { room: roomId });
}

/** Which room this window is the call for, when the window was opened by
 *  `openRoomWindow` (`?room=<room id>`). Null in the main window. */
export function roomWindowTarget(): string | null {
  if (typeof window === "undefined") return null;
  return new URLSearchParams(window.location.search).get("room");
}

/** The same-device chatter between this app's windows about rooms — a
 *  room window telling the main window "I joined / left", the main window
 *  asking a room window to hang up, any window announcing the saved rooms
 *  list changed. Mesh room messages never echo back to their sender, so
 *  windows of one device need this local lane to stay in step. Carried on
 *  the Tauri event bus (which reaches every window, the sender included —
 *  receivers drop their own `token`). No-op in web mode: one window. */
export interface RoomLocalEvent {
  /** The emitting window's store token — receivers ignore their own. */
  token: string;
  /** What happened: `join`/`leave` mirror self-presence, `leave-ask`
   *  requests whichever window holds the room joined to hang up, `sync`
   *  says the persisted rooms list changed (reload it), `knock-done`
   *  says an ask-to-join was answered (clear it everywhere). */
  kind: "join" | "leave" | "leave-ask" | "sync" | "knock-done";
  room?: string;
  /** `knock-done`: whose ask was answered. */
  from?: string;
}

export async function emitRoomLocal(event: RoomLocalEvent): Promise<void> {
  if (!isTauri()) return;
  const { emit } = await import("@tauri-apps/api/event");
  await emit("allmystuff://room-local", event);
}

export async function onRoomLocal(cb: (e: RoomLocalEvent) => void): Promise<() => void> {
  if (!isTauri()) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  return listen<RoomLocalEvent>("allmystuff://room-local", (e) => cb(e.payload));
}

/** Flip this OS window in or out of fullscreen (a room window's
 *  fullscreen control). Resolves to the new state; web mode is a no-op
 *  (the browser owns fullscreen there). */
export async function toggleWindowFullscreen(): Promise<boolean> {
  if (!isTauri()) return false;
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  const win = getCurrentWindow();
  const next = !(await win.isFullscreen());
  await win.setFullscreen(next);
  return next;
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

/** The networks currently switched off — their full parked configs (we
 *  only read the summary fields; the rest round-trips untouched). */
export async function disabledNetworks(): Promise<NetworkConfigFull[]> {
  const r = await invokeReq<NetworkConfigFull[]>("disabled_networks");
  return Array.isArray(r) ? r : [];
}

/** Switch a network off (leave the daemon, park the config) or back on
 *  (re-join from the parked config). Rosters survive on disk in between. */
export const setNetworkEnabled = (network: string, enabled: boolean) =>
  invokeReq<unknown>("network_set_enabled", { network, enabled });

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
