<script lang="ts">
  import { app } from "../store.svelte";
  import { displayName, mediaColor, humanBytes, isAppNode, MEDIA, type MediaKind } from "../types";
  import type { MeshNode } from "../types";

  // Canvas size tracked via ResizeObserver so the layout fits its
  // container (same approach as the MyOwnMesh NodeMap).
  let width = $state(1000);
  let height = $state(700);
  let canvas = $state<HTMLDivElement | null>(null);

  $effect(() => {
    if (!canvas) return;
    const ro = new ResizeObserver((entries) => {
      for (const e of entries) {
        width = Math.max(360, e.contentRect.width);
        height = Math.max(320, e.contentRect.height);
      }
    });
    ro.observe(canvas);
    return () => ro.disconnect();
  });

  const NODE_W = 184;
  const NODE_H = 86;

  type Placed = { node: MeshNode; x: number; y: number };

  // ---- fleet grouping -------------------------------------------------
  //
  // Every node belongs to exactly one fleet: yours (this device, your
  // owned-fleet co-members, anything you own), a named fleet per *other*
  // owner (people bring fleets — their devices group under them), or
  // "Unknown fleet" for devices that advertise no owner at all (including
  // bare mesh endpoints not running AllMyStuff). Both views read this:
  // the ring seats fleets together, the grid draws them as sections.

  type FleetGroup = { key: string; label: string; nodes: MeshNode[] };

  const mineLabel = (): string =>
    app.fleetName
      ? `${app.fleetName}'s fleet`
      : app.inFleet
        ? "Your fleet"
        : "Your devices";

  function fleetKeyOf(n: MeshNode): { key: string; label: string } {
    // The local device groups with your fleet — but only while it actually
    // belongs to one. Leaving a fleet releases this machine's ownership, so a
    // fleet-less "this" device drops to "Unknown fleet" alongside any other
    // owner-less node, instead of clinging to a "Your devices" group of one.
    // It still anchors your group while you genuinely own other machines, and
    // while it's the only machine on the graph (a lone first-run device isn't
    // "unknown").
    if (n.kind === "this") {
      const inFleet = app.isFleetMember(n.id) || (!!n.owner && app.isMe(n.owner));
      const ownsOthers = app.catalog.nodes.some(
        (m) =>
          m.id !== n.id &&
          (m.relationship.kind === "mine" ||
            app.isFleetMember(m.id) ||
            (!!m.owner && app.isMe(m.owner))),
      );
      const aloneHere =
        app.catalog.nodes.filter((m) => m.id !== n.id && isAppNode(m)).length === 0;
      return inFleet || ownsOthers || aloneHere
        ? { key: "mine", label: mineLabel() }
        : { key: "unknown", label: "Unknown fleet" };
    }
    if (
      n.relationship.kind === "mine" ||
      app.isFleetMember(n.id) ||
      (!!n.owner && app.isMe(n.owner))
    ) {
      return { key: "mine", label: mineLabel() };
    }
    if (n.relationship.kind === "shared") {
      const p = n.relationship.person;
      return { key: `fleet:${p.id}`, label: `${p.name}'s fleet` };
    }
    if (n.owner) {
      const person = app.personFor(n);
      return { key: `fleet:${person.id}`, label: `${person.name}'s fleet` };
    }
    return { key: "unknown", label: "Unknown fleet" };
  }

  const fleetGroups = $derived.by((): FleetGroup[] => {
    const groups = new Map<string, FleetGroup>();
    for (const n of app.catalog.nodes) {
      const { key, label } = fleetKeyOf(n);
      const g = groups.get(key) ?? { key, label, nodes: [] };
      g.nodes.push(n);
      groups.set(key, g);
    }
    for (const g of groups.values()) {
      g.nodes.sort((a, b) => {
        // This device leads its fleet; the rest read alphabetically.
        const rank = (n: MeshNode) => (n.kind === "this" ? 0 : 1);
        return rank(a) - rank(b) || a.label.localeCompare(b.label);
      });
    }
    // Your fleet first, named fleets alphabetically, the unknowns last.
    const rank = (g: FleetGroup) => (g.key === "mine" ? 0 : g.key === "unknown" ? 2 : 1);
    return [...groups.values()].sort(
      (a, b) => rank(a) - rank(b) || a.label.localeCompare(b.label),
    );
  });

  // ---- views ------------------------------------------------------------
  //
  // Two layouts over the same nodes: the grouped grid (one labelled section
  // per fleet — the default) and the radial ("this" centred, fleets seated
  // together around the ring) — switched from the zoom controls.

  type ViewMode = "radial" | "grid";
  const VIEW_STORE_KEY = "allmystuff.graphView.v1";
  let view = $state<ViewMode>(loadView());

  function loadView(): ViewMode {
    // Grid is the default now — only an explicit, stored "radial" choice opts
    // back out, so a fresh install lands on the grouped view.
    try {
      return localStorage.getItem(VIEW_STORE_KEY) === "radial" ? "radial" : "grid";
    } catch {
      return "grid";
    }
  }

  function setView(v: ViewMode) {
    view = v;
    panX = 0;
    panY = 0;
    zoom = 1;
    try {
      localStorage.setItem(VIEW_STORE_KEY, v);
    } catch {
      /* private mode — the toggle just doesn't persist */
    }
  }

  // The grid's geometry: one section per fleet, nodes wrapped into rows.
  const GRID_MARGIN = 28;
  const CELL_W = NODE_W + 26;
  const CELL_H = NODE_H + 64; // node + meta rows + breathing room
  const SECTION_HEAD = 34;
  const SECTION_GAP = 26;
  const SECTION_PAD = 14;

  type Section = { key: string; label: string; x: number; y: number; w: number; h: number; count: number };

  const gridLayout = $derived.by((): { placed: Placed[]; sections: Section[] } => {
    const placed: Placed[] = [];
    const sections: Section[] = [];
    const cols = Math.max(1, Math.floor((width - 2 * GRID_MARGIN) / CELL_W));
    let y = GRID_MARGIN;
    for (const g of fleetGroups) {
      const useCols = Math.min(cols, Math.max(1, g.nodes.length));
      const rows = Math.ceil(g.nodes.length / useCols);
      const w = useCols * CELL_W + 2 * SECTION_PAD;
      const x0 = Math.max(GRID_MARGIN, (width - w) / 2);
      sections.push({
        key: g.key,
        label: g.label,
        x: x0,
        y,
        w,
        h: SECTION_HEAD + rows * CELL_H + SECTION_PAD,
        count: g.nodes.length,
      });
      g.nodes.forEach((n, i) => {
        const col = i % useCols;
        const row = Math.floor(i / useCols);
        placed.push({
          node: n,
          x: x0 + SECTION_PAD + col * CELL_W + CELL_W / 2,
          y: y + SECTION_HEAD + row * CELL_H + CELL_H / 2 - 10,
        });
      });
      y += SECTION_HEAD + rows * CELL_H + SECTION_PAD + SECTION_GAP;
    }
    return { placed, sections };
  });

  // Radial layout: "this" in the middle, everything else on a ring seated
  // by fleet — your devices first, then each named fleet, unknowns last —
  // so the eye reads "my fleet, then everyone else's."
  const radialLayout = $derived.by((): Placed[] => {
    const cx = width / 2;
    const cy = height / 2;
    // Centre the local machine by its definitive marker (`kind === "this"`),
    // not by id: a presence snapshot can move `localId` to the real session id
    // before a scan re-homes the node off its first-scan placeholder, and we
    // must never leave the centre empty with "me" stranded out on the ring.
    const me =
      app.catalog.nodes.find((n) => n.kind === "this") ??
      app.catalog.nodes.find((n) => n.id === app.localId);
    const others = fleetGroups.flatMap((g) => g.nodes).filter((n) => n.id !== me?.id);
    const placed: Placed[] = [];
    if (me) placed.push({ node: me, x: cx, y: cy });
    const radius = Math.max(180, Math.min(width, height) / 2 - 130);
    others.forEach((n, i) => {
      const a = -Math.PI / 2 + (i * 2 * Math.PI) / Math.max(1, others.length);
      placed.push({ node: n, x: cx + Math.cos(a) * radius, y: cy + Math.sin(a) * radius });
    });
    return placed;
  });

  const layout = $derived(view === "grid" ? gridLayout.placed : radialLayout);
  const sections = $derived(view === "grid" ? gridLayout.sections : []);

  const posOf = $derived.by(() => {
    const m = new Map<string, Placed>();
    for (const p of layout) m.set(p.node.id, p);
    return m;
  });

  // Edges: one per route, connecting the two nodes the capabilities live
  // on. Curved + coloured by media, with parallel routes fanned apart.
  // Endpoints resolve through the display fallback so a live terminal
  // session (whose endpoints aren't catalog capabilities) draws its wire.
  type Edge = { id: string; x1: number; y1: number; x2: number; y2: number; cx: number; cy: number; color: string; media: MediaKind };
  const edges = $derived.by((): Edge[] => {
    const pairCount = new Map<string, number>();
    const out: Edge[] = [];
    for (const r of app.catalog.routes) {
      const from = app.capabilityForDisplay(r.from);
      const to = app.capabilityForDisplay(r.to);
      if (!from || !to) continue;
      const a = posOf.get(from.node);
      const b = posOf.get(to.node);
      if (!a || !b || a === b) continue;
      const key = [from.node, to.node].sort().join("|");
      const idx = pairCount.get(key) ?? 0;
      pairCount.set(key, idx + 1);
      const mx = (a.x + b.x) / 2;
      const my = (a.y + b.y) / 2;
      // Perpendicular offset so multiple wires between the same pair fan.
      const dx = b.x - a.x;
      const dy = b.y - a.y;
      const len = Math.hypot(dx, dy) || 1;
      const off = (idx % 2 === 0 ? 1 : -1) * Math.ceil((idx + 1) / 2) * 26;
      out.push({
        id: r.id,
        x1: a.x,
        y1: a.y,
        x2: b.x,
        y2: b.y,
        cx: mx + (-dy / len) * off,
        cy: my + (dx / len) * off,
        color: mediaColor(r.media as MediaKind),
        media: r.media as MediaKind,
      });
    }
    return out;
  });

  // A small label that follows the cursor along a connection wire, naming what
  // flows down it (1–2 words). Placed in canvas coordinates so it sits right
  // under the pointer — zero eye travel.
  let lineTip = $state<{ x: number; y: number; text: string } | null>(null);
  function onWireMove(e: PointerEvent, media: MediaKind) {
    const rect = canvas?.getBoundingClientRect();
    if (!rect) return;
    lineTip = { x: e.clientX - rect.left, y: e.clientY - rect.top, text: MEDIA[media].label };
  }
  function onWireLeave() {
    lineTip = null;
  }

  // ---- pan / zoom / select ------------------------------------------
  //
  // Matching every graph and design tool: right-drag pans, left-drag on empty
  // space marquee-selects, and a left-drag from one device onto another opens
  // the share builder. A plain left-click still selects a single device.
  let panX = $state(0);
  let panY = $state(0);
  let zoom = $state(1);
  const MIN_ZOOM = 0.4;
  const MAX_ZOOM = 2.2;

  // "Show me this device" from a settings list: when the store bumps a focus
  // request, pan the camera so the node sits at the canvas centre (at the
  // current zoom), so the View buttons actually reveal the node rather than
  // just selecting it somewhere off-screen. Guarded by the request's seq so it
  // fires once per request — a plain counter, not $state, so updating it doesn't
  // re-run this effect.
  let lastFocusSeq = 0;
  $effect(() => {
    const req = app.focusRequest;
    if (!req || req.seq === lastFocusSeq) return;
    lastFocusSeq = req.seq;
    const p = layout.find((pl) => pl.node.id === req.id);
    if (!p) return;
    panX = width / 2 - p.x * zoom;
    panY = height / 2 - p.y * zoom;
  });

  // The active canvas gesture: panning (right-drag) or marqueeing (left-drag on
  // empty). Node drags are tracked separately, below.
  type Gesture =
    | { kind: "pan"; x: number; y: number; panX: number; panY: number }
    | { kind: "marquee"; x0: number; y0: number; x1: number; y1: number };
  let gesture = $state<Gesture | null>(null);
  const panning = $derived(gesture?.kind === "pan");

  // Multi-selection (the marquee's result). The single-click selection still
  // flows through app.selectedNodeId so the drawer always has one focused node;
  // these add the extra highlighted devices on top.
  let selectedIds = $state<Set<string>>(new Set());

  // Live marquee preview — the devices currently inside the box, recomputed as
  // it's dragged so the highlight tracks the rubber-band in real time. Folded
  // into the selected look while dragging, then committed on release.
  const marqueeHits = $derived.by((): Set<string> => {
    const set = new Set<string>();
    if (gesture?.kind !== "marquee") return set;
    const x1 = Math.min(gesture.x0, gesture.x1);
    const x2 = Math.max(gesture.x0, gesture.x1);
    const y1 = Math.min(gesture.y0, gesture.y1);
    const y2 = Math.max(gesture.y0, gesture.y1);
    if (x2 - x1 < 4 && y2 - y1 < 4) return set;
    for (const p of layout) {
      const sx = p.x * zoom + panX;
      const sy = p.y * zoom + panY;
      if (sx >= x1 && sx <= x2 && sy >= y1 && sy <= y2) set.add(p.node.id);
    }
    return set;
  });

  function isSelected(id: string): boolean {
    return app.selectedNodeId === id || selectedIds.has(id) || marqueeHits.has(id);
  }

  // A left-drag that started on a device — used for drag-onto-another-device (or
  // fleet) to open the share builder. `moved` tells a real drag from a plain
  // click. A ghost chip follows the cursor while it's in flight.
  let nodeDrag = $state<{ id: string; sx: number; sy: number; moved: boolean } | null>(null);
  let dragOverId = $state<string | null>(null);
  let dragOverSection = $state<string | null>(null);
  let dragLabel = $state("");
  let dragPos = $state<{ x: number; y: number } | null>(null);

  const dropTargetLabel = $derived.by(() => {
    if (dragOverId) {
      const t = app.node(dragOverId);
      return t ? displayName(t) : "";
    }
    return dragOverSection ?? "";
  });

  function canvasPoint(e: PointerEvent): { x: number; y: number } {
    const r = canvas?.getBoundingClientRect();
    return { x: e.clientX - (r?.left ?? 0), y: e.clientY - (r?.top ?? 0) };
  }

  function onWheel(e: WheelEvent) {
    e.preventDefault();
    const rect = canvas?.getBoundingClientRect();
    if (!rect) return;
    const px = e.clientX - rect.left;
    const py = e.clientY - rect.top;
    const factor = Math.exp(-e.deltaY * 0.0012);
    const next = Math.max(MIN_ZOOM, Math.min(MAX_ZOOM, zoom * factor));
    if (next === zoom) return;
    const ratio = next / zoom;
    panX = px - (px - panX) * ratio;
    panY = py - (py - panY) * ratio;
    zoom = next;
  }

  function onPointerDown(e: PointerEvent) {
    const target = e.target as Element | null;
    // Let node clicks/drags and the floating controls handle themselves —
    // capturing here would swallow their clicks.
    if (target?.closest?.(".node, .zoombar, .arm-banner, .restart-panel")) return;
    if (app.dragFrom) {
      app.cancelConnect();
      return;
    }
    // Stop a drag from turning into a native text selection.
    e.preventDefault();
    if (e.button === 2 || e.button === 1) {
      // Right (or middle) drag pans — the platform convention.
      gesture = { kind: "pan", x: e.clientX, y: e.clientY, panX, panY };
      (e.currentTarget as Element).setPointerCapture?.(e.pointerId);
    } else if (e.button === 0) {
      // Left drag on empty space marquee-selects. Without a modifier it starts
      // a fresh selection.
      if (!e.shiftKey && !e.metaKey && !e.ctrlKey) {
        selectedIds = new Set();
        app.selectNode(null);
      }
      const p = canvasPoint(e);
      gesture = { kind: "marquee", x0: p.x, y0: p.y, x1: p.x, y1: p.y };
      (e.currentTarget as Element).setPointerCapture?.(e.pointerId);
    }
  }
  function onPointerMove(e: PointerEvent) {
    if (!gesture) return;
    if (gesture.kind === "pan") {
      panX = gesture.panX + (e.clientX - gesture.x);
      panY = gesture.panY + (e.clientY - gesture.y);
    } else {
      const p = canvasPoint(e);
      gesture = { ...gesture, x1: p.x, y1: p.y };
    }
  }
  function onPointerUp(e: PointerEvent) {
    if (gesture?.kind === "marquee") {
      // Commit whatever the live preview is highlighting (read it before the
      // gesture clears, since it's derived from the box).
      const hits = marqueeHits;
      if (hits.size > 0) {
        const next = new Set(selectedIds);
        for (const id of hits) next.add(id);
        selectedIds = next;
        // Keep the drawer useful: focus the single marqueed device, if one.
        if (next.size === 1) app.selectNode([...next][0]);
      }
    }
    gesture = null;
    (e.currentTarget as Element).releasePointerCapture?.(e.pointerId);
  }

  // No browser context menu on the canvas — right-drag is pan, and an empty
  // right-click should do nothing (not pop the OS menu).
  function onContextMenu(e: MouseEvent) {
    e.preventDefault();
  }

  // ---- drag one device onto another → open the share builder ----------
  function onNodePointerDown(e: PointerEvent, n: MeshNode) {
    if (e.button !== 0 || app.dragFrom) return; // left only; connect mode uses clicks
    // A press on an inline control (Claim, Make claimable…) is that button's —
    // don't hijack it into a node drag, or capturing the pointer eats its click.
    if ((e.target as Element | null)?.closest?.("button, a, input, select, textarea")) return;
    e.stopPropagation();
    e.preventDefault(); // don't let the drag select the node's text
    nodeDrag = { id: n.id, sx: e.clientX, sy: e.clientY, moved: false };
    dragLabel = displayName(n);
    dragPos = canvasPoint(e);
    (e.currentTarget as Element).setPointerCapture?.(e.pointerId);
  }
  function onNodePointerMove(e: PointerEvent, n: MeshNode) {
    if (!nodeDrag || nodeDrag.id !== n.id) return;
    // Only your own devices are draggable share sources — a foreign device
    // stays a plain click (select), never a drag.
    if (!app.isMyDevice(n.id)) return;
    if (!nodeDrag.moved && Math.hypot(e.clientX - nodeDrag.sx, e.clientY - nodeDrag.sy) < 6) return;
    nodeDrag = { ...nodeDrag, moved: true };
    dragPos = canvasPoint(e);
    // Hit-test what's under the cursor: another device, or a fleet band.
    const hit = document.elementFromPoint(e.clientX, e.clientY);
    const overNode = hit?.closest?.(".node")?.getAttribute("data-node-id") ?? null;
    if (overNode && overNode !== n.id) {
      dragOverId = overNode;
      dragOverSection = null;
    } else {
      dragOverId = null;
      dragOverSection = hit?.closest?.(".section")?.getAttribute("data-section-label") ?? null;
    }
  }
  function onNodePointerUp(e: PointerEvent, n: MeshNode) {
    // Only act when this device actually started the gesture (a press that
    // began on an inline button left nodeDrag null — let that button win).
    if (app.dragFrom) {
      // Connect mode: a tap completes the wire, exactly as before.
      onNodeClick(n);
      return;
    }
    if (!nodeDrag || nodeDrag.id !== n.id) return;
    const moved = nodeDrag.moved;
    const overNode = dragOverId;
    const overSection = dragOverSection;
    (e.currentTarget as Element).releasePointerCapture?.(e.pointerId);
    nodeDrag = null;
    dragOverId = null;
    dragOverSection = null;
    dragPos = null;
    if (moved) {
      // Dropped on another device → sender = dragged, receiver = target. Dropped
      // on a fleet band → open the builder with the dragged device as sender and
      // let you pick the receiver in that fleet.
      if (overNode) app.openShareFlow(n.id, overNode);
      else if (overSection) app.openShareFlow(n.id, null);
    } else {
      // Didn't move — it's a plain click: select (the old behaviour).
      onNodeClick(n);
    }
  }

  function nodeAvatar(n: MeshNode): string {
    // A device on the mesh that isn't running AllMyStuff reads as a bare
    // endpoint, not a fleshed-out machine.
    if (!isAppNode(n)) return "📡";
    if (n.relationship.kind === "shared") return "🧑";
    const os = (n.summary?.os ?? "").toLowerCase();
    if (n.kind === "this") return "💻";
    if (os.includes("mac")) return "🍎";
    if (os.includes("win")) return "🪟";
    if (os.includes("android") || os.includes("phone")) return "📱";
    if (n.label.toLowerCase().includes("tv")) return "📺";
    return "🖥";
  }

  // Whether a node is a valid connect-drop target right now — drives the
  // pulsing highlight. A device must be running AllMyStuff and already
  // claimed to be a target.
  const armed = $derived(!!app.dragFrom);
  function targetable(n: MeshNode): boolean {
    return isAppNode(n) && n.relationship.kind !== "unclaimed";
  }

  /** A device offering itself for adoption that you can actually take — the
   *  same `standing().claimable` the node's visual reads, so the tap target
   *  and the look never disagree. */
  function isAdoptable(n: MeshNode): boolean {
    return app.standingOf(n).claimable;
  }

  /** The claimable node whose inline "Claim" button is currently dropped out
   *  (revealed by tapping it). Null = none showing. */
  let claimRevealed = $state<string | null>(null);

  function onNodeClick(n: MeshNode) {
    if (app.dragFrom) {
      // Mesh-only and not-yet-claimed nodes aren't connection targets.
      if (!isAppNode(n)) {
        app.toast("warn", `${n.label} isn't running AllMyStuff`);
        return;
      }
      if (n.relationship.kind === "unclaimed") {
        app.toast("warn", `Claim ${n.label} first — open it to adopt it`);
        return;
      }
      app.dropConnectOnNode(n.id);
    } else if (isAdoptable(n)) {
      // Tapping a claimable node drops an inline "Claim" button out from
      // under it (shimmer + slide) — the fast path to adopt, right on the
      // graph — and opens the drawer for the full story.
      claimRevealed = claimRevealed === n.id ? null : n.id;
      app.selectNode(n.id);
    } else {
      claimRevealed = null;
      // Clicking a device always selects it and keeps it selected — re-clicking
      // the focused node no longer toggles it off (close the drawer to deselect).
      app.selectNode(n.id);
    }
  }

  // ---- per-node actions menu (the gear) -----------------------------
  //
  // Opened from a card's gear and positioned in viewport coordinates so it
  // can flip up / left to stay on screen — the cards live in a panned + zoomed
  // layer, so the menu is rendered OUTSIDE that layer (a top-level sibling) and
  // anchored with `position: fixed` from the gear's on-screen rect.
  let nodeMenu = $state<{ id: string; left: number; top: number } | null>(null);
  const MENU_W = 216;
  const MENU_H = 124;

  function openNodeMenu(e: MouseEvent, nodeId: string) {
    e.stopPropagation();
    if (nodeMenu?.id === nodeId) {
      nodeMenu = null; // toggle closed
      return;
    }
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    // Flip toward whichever side has room: open up when the gear is near the
    // bottom, and align to the right edge when it's near the right.
    const openUp = r.bottom + MENU_H + 8 > vh;
    const openLeft = r.left + MENU_W + 8 > vw;
    const left = openLeft
      ? Math.max(8, r.right - MENU_W)
      : Math.min(r.left, vw - MENU_W - 8);
    const top = openUp ? Math.max(8, r.top - MENU_H - 6) : r.bottom + 6;
    nodeMenu = { id: nodeId, left, top };
  }

  // Close the menu on any outside pointer-down (the gear + the menu are exempt
  // so they can toggle / be clicked), and on Escape.
  $effect(() => {
    if (!nodeMenu) return;
    function onDown(e: PointerEvent) {
      const t = e.target as Element | null;
      if (!t?.closest?.(".node-menu, .node-gear")) nodeMenu = null;
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") nodeMenu = null;
    }
    window.addEventListener("pointerdown", onDown);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("pointerdown", onDown);
      window.removeEventListener("keydown", onKey);
    };
  });
</script>

<!-- The small console glyphs for the card buttons: remote desktop, files,
     terminal, sites. Stroke uses currentColor. -->
{#snippet cicon(kind: "remote" | "files" | "terminal" | "sites")}
  {#if kind === "remote"}
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <rect x="3" y="4" width="18" height="13" rx="2" /><path d="M8 20h8M12 17v3" />
    </svg>
  {:else if kind === "files"}
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <path d="M3 6.5A1.5 1.5 0 0 1 4.5 5h4l2 2.2H19a1.5 1.5 0 0 1 1.5 1.5V18a1.5 1.5 0 0 1-1.5 1.5H4.5A1.5 1.5 0 0 1 3 18Z" />
    </svg>
  {:else if kind === "terminal"}
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <rect x="3" y="4.5" width="18" height="15" rx="2" /><path d="M7 9.5l3 2.5-3 2.5M12.5 15h4" />
    </svg>
  {:else}
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <circle cx="12" cy="12" r="8.5" /><path d="M3.5 12h17M12 3.5c2.5 2.4 2.5 14.6 0 17M12 3.5c-2.5 2.4-2.5 14.6 0 17" />
    </svg>
  {/if}
{/snippet}

<div
  class="canvas"
  class:panning
  class:marqueeing={gesture?.kind === "marquee"}
  class:armed
  bind:this={canvas}
  onwheel={onWheel}
  onpointerdown={onPointerDown}
  onpointermove={onPointerMove}
  onpointerup={onPointerUp}
  onpointercancel={onPointerUp}
  oncontextmenu={onContextMenu}
  role="application"
  aria-label="Your stuff, as a graph"
>
  <!-- edge layer -->
  <svg class="edges" {width} {height} aria-hidden="true">
    <defs>
      <pattern id="dots" width="26" height="26" patternUnits="userSpaceOnUse">
        <circle cx="1.5" cy="1.5" r="1.5" fill="oklch(0.62 0.20 292 / 0.16)" />
      </pattern>
    </defs>
    <rect x="0" y="0" {width} {height} fill="url(#dots)" />
    <g transform="translate({panX} {panY}) scale({zoom})">
      {#each edges as e (e.id)}
        <path
          class="wire"
          d="M {e.x1} {e.y1} Q {e.cx} {e.cy} {e.x2} {e.y2}"
          stroke={e.color}
          fill="none"
        />
        <path
          class="wire-flow"
          d="M {e.x1} {e.y1} Q {e.cx} {e.cy} {e.x2} {e.y2}"
          stroke={e.color}
          fill="none"
        />
        <!-- A wide transparent hit path so hovering anywhere near the wire
             raises its cursor-following media label. The edge layer is
             aria-hidden; this is a decorative hover affordance. -->
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <path
          class="wire-hit"
          d="M {e.x1} {e.y1} Q {e.cx} {e.cy} {e.x2} {e.y2}"
          fill="none"
          onpointerenter={(ev) => onWireMove(ev, e.media)}
          onpointermove={(ev) => onWireMove(ev, e.media)}
          onpointerleave={onWireLeave}
        />
      {/each}
    </g>
  </svg>

  <!-- node layer (HTML, shares the same transform) -->
  <div class="nodes" style="transform: translate({panX}px, {panY}px) scale({zoom});">
    {#each sections as s (s.key)}
      <!-- grid view only: one labelled band per fleet -->
      <div
        class="section"
        class:mine={s.key === "mine"}
        class:unknown={s.key === "unknown"}
        class:dragover={dragOverSection === s.label}
        data-section-label={s.label}
        style="left: {s.x}px; top: {s.y}px; width: {s.w}px; height: {s.h}px;"
      >
        <div class="section-head">
          {s.key === "mine" ? "🔗" : s.key === "unknown" ? "❓" : "🧑"}
          {s.label}
          <span class="section-count">{s.count}</span>
        </div>
      </div>
    {/each}
    {#each layout as p (p.node.id)}
      {@const n = p.node}
      <!-- One derived standing drives every visual + affordance, so the node
           never shows contradictory state (e.g. "unclaimed" while wearing a
           fleet badge). It recomputes live from the fleet roster + the device's
           advert, so claiming or fleet changes reflect immediately. -->
      {@const st = app.standingOf(n)}
      {@const cons = app.consoleAccess(n)}
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div
        class="node"
        class:self={st.self}
        class:shared={st.kind === "shared"}
        class:mine={st.mine && !st.self}
        class:unclaimed={st.kind === "free" || st.kind === "theirs"}
        class:claimable={st.claimable}
        class:meshonly={!st.app}
        class:selected={isSelected(n.id)}
        class:armed={armed && targetable(n)}
        class:dragover={dragOverId === n.id}
        class:dragging-node={nodeDrag?.id === n.id && nodeDrag.moved}
        class:grabbable={app.isMyDevice(n.id)}
        class:offline={!n.online}
        data-node-id={n.id}
        style="left: {p.x - NODE_W / 2}px; top: {p.y - NODE_H / 2}px; width: {NODE_W}px; min-height: {NODE_H}px;"
        onpointerdown={(e) => onNodePointerDown(e, n)}
        onpointermove={(e) => onNodePointerMove(e, n)}
        onpointerup={(e) => onNodePointerUp(e, n)}
        onpointercancel={(e) => onNodePointerUp(e, n)}
        onkeydown={(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            onNodeClick(n);
          }
        }}
        role="button"
        tabindex="0"
        aria-label={displayName(n)}
      >
        {#if st.self}<span class="self-corner" aria-hidden="true">This device</span>{/if}
        <div class="node-top">
          <span class="avatar">{nodeAvatar(n)}</span>
          <div class="node-id">
            <div class="node-label" title={displayName(n)}>{displayName(n)}</div>
            <div class="node-sub">
              {#if st.shared}
                shared with {st.shared.name}
              {:else if !st.app}
                on the mesh · not running AllMyStuff
              {:else if n.summary}
                {n.summary.cpu}
              {:else}
                device
              {/if}
            </div>
          </div>
        </div>
        <div class="node-meta">
          {#if !st.app}<span class="tag meshonly">not on AllMyStuff</span>
          {:else if st.shared}<span class="tag guest">guest</span>
          {:else if st.kind === "claimable"}<span class="tag claimable">＋ claim</span>
          {:else if st.kind === "theirs"}<span class="tag theirs">someone else's</span>
          {:else if st.kind === "free"}<span class="tag unclaimed">unclaimed</span>
          {:else if st.mine && !st.inFleet && !st.self}<span class="tag mine">yours</span>{/if}
          {#if st.inFleet}<span class="tag fleet" class:owner={st.role === "owner"} class:manager={st.role === "manager"} title="In your fleet · {st.role}">{st.role === "owner" ? "★ owner" : st.role === "manager" ? "⚑ manager" : "🔗 fleet"}</span>{/if}
          {#if n.summary}<span class="tag soft">{n.summary.device_count} things</span>{/if}
          {#if n.summary}<span class="tag soft">{humanBytes(n.summary.ram_bytes)}</span>{/if}
        </div>
        <!-- Bottom button row. The refresh (online dot ringed by refresh arrows
             = re-learn this node) and the settings gear lead, inline with the
             console buttons; a divider separates them from the consoles you can
             open on this device (your own fleet's, or what a fleet granted). -->
        <div class="node-consoles">
          <button
            class="cbtn status-refresh"
            class:online={n.online}
            data-tip="Refresh"
            aria-label={`Refresh ${displayName(n)}`}
            onclick={(e) => {
              e.stopPropagation();
              void app.refreshNode(n.id);
            }}
          >
            <svg class="refresh-ring" viewBox="0 0 24 24" aria-hidden="true">
              <polyline points="22 5 22 10 17 10" />
              <polyline points="2 19 2 14 7 14" />
              <path d="M4.2 9.3a8 8 0 0 1 13.4-3L22 10" />
              <path d="M19.8 14.7a8 8 0 0 1-13.4 3L2 14" />
            </svg>
            <span class="dot" class:on={n.online}></span>
          </button>
          <button
            class="cbtn node-gear"
            data-tip="Settings"
            aria-label={`Settings for ${displayName(n)}`}
            aria-haspopup="menu"
            aria-expanded={nodeMenu?.id === n.id}
            onclick={(e) => openNodeMenu(e, n.id)}
          >⚙</button>
          {#if cons.remote || cons.files || cons.terminal || cons.sites}
            <span class="ctl-div" aria-hidden="true"></span>
            {#if cons.remote}
              <button class="cbtn" data-tip="Remote control" aria-label="Remote control {displayName(n)}"
                onclick={(e) => { e.stopPropagation(); app.openConsoleKind(n.id, "remote"); }}>{@render cicon("remote")}</button>
            {/if}
            {#if cons.files}
              <button class="cbtn" data-tip="Files" aria-label="Open files on {displayName(n)}"
                onclick={(e) => { e.stopPropagation(); app.openConsoleKind(n.id, "files"); }}>{@render cicon("files")}</button>
            {/if}
            {#if cons.terminal}
              <button class="cbtn" data-tip="Terminal" aria-label="Open terminal on {displayName(n)}"
                onclick={(e) => { e.stopPropagation(); app.openConsoleKind(n.id, "terminal"); }}>{@render cicon("terminal")}</button>
            {/if}
            {#if cons.sites}
              <button class="cbtn" data-tip="Sites" aria-label="Open sites on {displayName(n)}"
                onclick={(e) => { e.stopPropagation(); app.openConsoleKind(n.id, "sites"); }}>{@render cicon("sites")}</button>
            {/if}
          {/if}
        </div>
        <!-- Claimable affordances drop out from *under* the node, floating
             below it so they never disturb the graph's layout. -->
        {#if st.self && st.app && !st.inFleet && !st.offering}
          <!-- Your own device, not in a fleet: offer it for adoption. -->
          <button
            class="node-drawer make-claimable"
            title="Offer this device so another of your machines can adopt it into a fleet"
            onclick={(e) => { e.stopPropagation(); void app.setLocalClaimable(true); }}
          >🔒 Make claimable</button>
        {:else if st.claimable && claimRevealed === n.id}
          <!-- A claimable device you tapped: the Claim button slides in. -->
          <button
            class="node-drawer claim-go"
            onclick={(e) => { e.stopPropagation(); void app.claim(n.id); claimRevealed = null; }}
          >＋ Claim this device</button>
        {/if}
      </div>
    {/each}
  </div>

  {#if gesture?.kind === "marquee"}
    <div
      class="marquee"
      style="left: {Math.min(gesture.x0, gesture.x1)}px; top: {Math.min(gesture.y0, gesture.y1)}px; width: {Math.abs(gesture.x1 - gesture.x0)}px; height: {Math.abs(gesture.y1 - gesture.y0)}px;"
    ></div>
  {/if}

  {#if nodeDrag?.moved && dragPos}
    <!-- The ghost that follows the cursor while a device is being dragged onto
         another device or fleet to start a share. -->
    <div class="ghost" class:ready={!!dropTargetLabel} style="left: {dragPos.x}px; top: {dragPos.y}px;">
      <span class="ghost-card">🔗 {dragLabel}</span>
      <span class="ghost-tip">
        {dropTargetLabel ? `New share → ${dropTargetLabel}` : "Drop on a device or fleet to share"}
      </span>
    </div>
  {/if}

  {#if lineTip}
    <div class="line-tip" style="left: {lineTip.x}px; top: {lineTip.y}px;">{lineTip.text}</div>
  {/if}

  <!-- Refresh progress — Restarting → Reconnecting → Connected, each dot going
       red → yellow → green, floating just above the bottom-centre of the graph
       so the result shows where the connections live. -->
  {#if app.restartFlow}
    <div class="restart-panel" role="status" aria-live="polite">
      {#each app.restartFlow as s, i (s.label)}
        {#if i > 0}
          <span class="restart-sep" class:done={app.restartFlow[i - 1].status === "ok"}></span>
        {/if}
        <span class="restart-step">
          <span class="restart-dot {s.status}"></span>
          <span class="restart-label">{s.label}</span>
        </span>
      {/each}
    </div>
  {/if}

  {#if app.catalog.nodes.length === 0}
    <div class="empty" aria-live="polite">
      <div class="empty-orb">🧦</div>
      <div class="empty-title">Getting your stuff together…</div>
      <div class="empty-sub">Scanning this machine and waiting for peers to appear.</div>
    </div>
  {/if}

  {#if armed}
    <div class="arm-banner">
      Tap a device to connect — or tap empty space to cancel
      <button class="btn small" onclick={() => app.cancelConnect()}>Cancel</button>
    </div>
  {/if}

  <div class="zoombar">
    <button
      class="zbtn"
      class:active={view === "radial"}
      title="Radial view — this device in the centre"
      aria-label="Radial view"
      onclick={() => setView("radial")}>◎</button
    >
    <button
      class="zbtn"
      class:active={view === "grid"}
      title="Grid view — grouped by fleet"
      aria-label="Grid view, grouped by fleet"
      onclick={() => setView("grid")}>⊞</button
    >
    <span class="zsep"></span>
    <button class="zbtn" title="Zoom out" onclick={() => (zoom = Math.max(MIN_ZOOM, zoom / 1.2))}>−</button>
    <button class="zbtn wide" title="Reset view" onclick={() => { panX = 0; panY = 0; zoom = 1; }}>{Math.round(zoom * 100)}%</button>
    <button class="zbtn" title="Zoom in" onclick={() => (zoom = Math.min(MAX_ZOOM, zoom * 1.2))}>+</button>
  </div>
</div>

<!-- The per-node actions menu, rendered at the component root (outside the
     panned/zoomed `.nodes` layer) so its `position: fixed` anchors to the
     viewport, and flipped up/left by `openNodeMenu` to stay on screen. -->
{#if nodeMenu}
  {@const menuId = nodeMenu.id}
  {@const mn = app.node(menuId)}
  <div class="node-menu" role="menu" style="left: {nodeMenu.left}px; top: {nodeMenu.top}px;">
    <button
      class="nm-item"
      role="menuitem"
      onclick={() => {
        void app.refreshNode(menuId);
        nodeMenu = null;
      }}
    >
      <span class="nm-icon" aria-hidden="true">↻</span>
      <span class="nm-text">
        <span class="nm-label">{app.isMe(menuId) ? "Rescan this device" : "Refresh details"}</span>
        <span class="nm-sub"
          >{app.isMe(menuId)
            ? "re-scan hardware, sites & options"
            : "re-learn its details, options & shares"}</span
        >
      </span>
    </button>
    {#if app.canRestartApp(mn)}
      <button
        class="nm-item"
        role="menuitem"
        onclick={() => {
          app.restartNodeApp(menuId);
          nodeMenu = null;
        }}
      >
        <span class="nm-icon" aria-hidden="true">⟲</span>
        <span class="nm-text">
          <span class="nm-label">{app.isMe(menuId) ? "Restart this app" : "Restart app"}</span>
          <span class="nm-sub"
            >{app.isMe(menuId) ? "relaunch AllMyStuff here" : "relaunch it on that machine"}</span
          >
        </span>
      </button>
    {/if}
  </div>
{/if}

<style>
  .canvas {
    position: relative;
    flex: 1;
    overflow: hidden;
    cursor: default;
    touch-action: none;
    user-select: none;
  }
  /* Right-drag pans (grabbing hand); left-drag on empty marquee-selects
     (crosshair). */
  .canvas.panning {
    cursor: grabbing;
  }
  .canvas.marqueeing {
    cursor: crosshair;
  }
  /* The marquee selection box. */
  .marquee {
    position: absolute;
    z-index: 4;
    border: 1px solid var(--accent);
    background: var(--accent-soft);
    border-radius: 2px;
    pointer-events: none;
  }
  /* The drag-to-share ghost — a chip that rides the cursor with a tooltip
     telling you a new share opens on drop. */
  .ghost {
    position: absolute;
    z-index: 8;
    transform: translate(14px, 14px);
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    pointer-events: none;
  }
  .ghost-card {
    align-self: flex-start;
    background: var(--surface);
    border: 1px solid var(--c-share);
    color: var(--ink);
    border-radius: var(--r-md);
    padding: 0.3rem 0.6rem;
    font-size: 0.8rem;
    font-weight: 700;
    box-shadow: var(--shadow-lg);
  }
  .ghost-tip {
    align-self: flex-start;
    background: oklch(0.16 0.02 285 / 0.97);
    border: 1px solid var(--line-strong);
    color: var(--ink-soft);
    border-radius: var(--r-pill);
    padding: 0.18rem 0.55rem;
    font-size: 0.7rem;
    font-weight: 650;
    box-shadow: var(--shadow-sm);
  }
  .ghost.ready .ghost-tip {
    border-color: var(--c-share);
    color: var(--c-share-ink);
    background: var(--c-share-soft);
  }
  .edges {
    position: absolute;
    inset: 0;
    pointer-events: none;
  }
  .wire {
    stroke-width: 3;
    opacity: 0.45;
    stroke-linecap: round;
  }
  .wire-flow {
    stroke-width: 3;
    stroke-linecap: round;
    stroke-dasharray: 1 14;
    opacity: 0.9;
    animation: flow 1.1s linear infinite;
  }
  /* Invisible, fat hit target so the cursor-following label is easy to raise —
     opts back into pointer events even though the edge layer ignores them. */
  .wire-hit {
    stroke: transparent;
    stroke-width: 16;
    pointer-events: stroke;
    cursor: help;
  }
  @keyframes flow {
    to {
      stroke-dashoffset: -30;
    }
  }
  /* The cursor-following wire label — a small black/grey tooltip naming what
     flows down the wire, placed right under the pointer. */
  .line-tip {
    position: absolute;
    z-index: 5;
    transform: translate(-50%, calc(-100% - 0.5rem));
    background: oklch(0.16 0.02 285 / 0.96);
    color: var(--ink);
    border: 1px solid var(--line-strong);
    border-radius: var(--r-sm);
    padding: 0.12rem 0.45rem;
    font-size: 0.7rem;
    font-weight: 650;
    white-space: nowrap;
    pointer-events: none;
    box-shadow: var(--shadow-sm);
  }
  /* The refresh 3-step panel — floats above the zoom bar at bottom centre. */
  .restart-panel {
    position: absolute;
    bottom: 4.4rem;
    left: 50%;
    transform: translateX(-50%);
    display: flex;
    align-items: center;
    gap: 0.55rem;
    z-index: 6;
    background: oklch(0.16 0.02 285 / 0.97);
    border: 1px solid var(--line-strong);
    border-radius: var(--r-pill);
    padding: 0.5rem 0.95rem;
    box-shadow: var(--shadow-lg);
    animation: restart-rise 0.18s ease;
  }
  @keyframes restart-rise {
    from {
      transform: translate(-50%, 8px);
      opacity: 0;
    }
  }
  .restart-step {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
  }
  .restart-label {
    font-size: 0.78rem;
    font-weight: 650;
    color: var(--ink-soft);
  }
  .restart-dot {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--danger);
    transition: background 0.3s ease, box-shadow 0.3s ease;
  }
  /* wait = red (default), go = yellow + pulse, ok = green. */
  .restart-dot.go {
    background: var(--warn);
    box-shadow: 0 0 0 3px var(--warn-soft);
    animation: restart-pulse 1s ease-in-out infinite;
  }
  .restart-dot.ok {
    background: var(--ok);
    box-shadow: 0 0 0 3px var(--ok-soft);
  }
  @keyframes restart-pulse {
    0%,
    100% {
      box-shadow: 0 0 0 2px var(--warn-soft);
    }
    50% {
      box-shadow: 0 0 0 5px oklch(0.79 0.14 75 / 0.06);
    }
  }
  .restart-sep {
    width: 1.5rem;
    height: 2px;
    border-radius: 2px;
    background: var(--line-strong);
    transition: background 0.3s ease;
  }
  .restart-sep.done {
    background: var(--ok);
  }
  .nodes {
    position: absolute;
    inset: 0;
    transform-origin: 0 0;
    pointer-events: none;
  }
  /* Grid view's fleet bands — quiet containers behind the nodes. */
  .section {
    position: absolute;
    border: 1.5px dashed var(--line-strong);
    border-radius: var(--r-lg);
    background: oklch(0.21 0.028 285 / 0.35);
  }
  /* Your fleet band — the green of the "fleet" concept (your own pack). */
  .section.mine {
    border-color: oklch(0.8 0.17 150 / 0.45);
    background: oklch(0.8 0.17 150 / 0.05);
  }
  .section.unknown {
    border-style: dotted;
    background: transparent;
  }
  /* A fleet band lit as a share-drop target. */
  .section.dragover {
    border-color: var(--c-share);
    border-style: solid;
    background: var(--c-share-soft);
  }
  .section-head {
    position: absolute;
    top: 0.45rem;
    left: 0.8rem;
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    font-size: 0.78rem;
    font-weight: 750;
    color: var(--ink-soft);
    letter-spacing: 0.01em;
  }
  .section.mine .section-head {
    color: var(--c-fleet-ink);
  }
  .section-count {
    font-size: 0.66rem;
    font-weight: 700;
    background: var(--surface-2);
    border: 1px solid var(--line);
    border-radius: var(--r-pill);
    padding: 0.02rem 0.4rem;
    color: var(--ink-faint);
  }
  .node {
    position: absolute;
    pointer-events: auto;
    background: var(--surface);
    border: 1px solid var(--line-strong);
    border-radius: var(--r-md);
    box-shadow: var(--shadow-md);
    padding: 0.55rem 0.6rem 0.5rem;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    transition: transform 0.1s ease, box-shadow 0.12s ease, border-color 0.12s ease;
  }
  .node:hover {
    transform: translateY(-2px);
    box-shadow: var(--shadow-lg), 0 0 0 1px var(--accent-soft),
      0 8px 30px -10px oklch(0.64 0.255 350 / 0.45);
  }
  .node.self {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-soft), var(--shadow-md);
  }
  /* A device shared with you wears the sharing concept's violet edge. */
  .node.shared {
    border-color: var(--c-share);
    background: linear-gradient(180deg, var(--surface-2), var(--surface));
  }
  .node.unclaimed {
    border-style: dashed;
    border-color: var(--line-strong);
  }
  /* Offering itself for adoption: a solid accent edge and a gentle pulsing
     halo, so a device you can claim invites the click the way a fresh, joinable
     thing should — the graph-level echo of the top-bar claim nudge. */
  .node.claimable {
    border-style: solid;
    border-color: var(--accent);
    animation: claim-halo 1.9s ease-out infinite;
  }
  .node.claimable:hover {
    transform: translateY(-2px);
  }
  @keyframes claim-halo {
    0% {
      box-shadow: 0 0 0 0 oklch(0.64 0.255 350 / 0.4), var(--shadow-md);
    }
    70% {
      box-shadow: 0 0 0 7px oklch(0.64 0.255 350 / 0), var(--shadow-md);
    }
    100% {
      box-shadow: 0 0 0 0 oklch(0.64 0.255 350 / 0), var(--shadow-md);
    }
  }
  /* The claim affordances drop out from *under* the node — floated below it so
     they never push siblings around — with a slide-in, and a shimmer on the
     accent Claim button to pull the eye to the new action. */
  /* A drawer that reads as part of *this* node: it tucks just under the card,
     inset and sharing the bottom edge (no top border, bottom-rounded), with a
     short stem down from the node's centre so it's unmistakably attached to
     the device above it — not floating loose between cards. */
  .node-drawer {
    position: absolute;
    top: calc(100% - 1px);
    left: 14px;
    right: 14px;
    z-index: 6;
    border: 1.5px solid var(--accent);
    border-top: none;
    border-radius: 0 0 var(--r-sm) var(--r-sm);
    padding: 0.42rem 0.5rem 0.4rem;
    font-size: 0.78rem;
    font-weight: 650;
    font-family: inherit;
    cursor: pointer;
    background: var(--surface);
    color: var(--accent-ink);
    box-shadow: 0 8px 16px oklch(0 0 0 / 0.18);
    transform-origin: top center;
    animation: drawer-drop 0.2s ease-out both;
  }
  /* The stem joining the drawer to the node's bottom edge. */
  .node-drawer::before {
    content: "";
    position: absolute;
    top: -8px;
    left: 50%;
    transform: translateX(-50%);
    width: 2px;
    height: 8px;
    background: var(--accent);
  }
  .node-drawer:hover {
    background: var(--accent-soft);
  }
  .claim-go {
    border-color: var(--accent);
    color: var(--accent-ink);
    background: linear-gradient(
      110deg,
      var(--accent-soft) 30%,
      oklch(0.7 0.16 350 / 0.5) 50%,
      var(--accent-soft) 70%
    );
    background-size: 220% 100%;
    animation: drawer-drop 0.22s ease-out both, shimmer 1.6s linear 0.22s infinite;
  }
  @keyframes drawer-drop {
    from {
      opacity: 0;
      transform: scaleY(0.4);
    }
    to {
      opacity: 1;
      transform: scaleY(1);
    }
  }
  @keyframes shimmer {
    from {
      background-position: 220% 0;
    }
    to {
      background-position: -20% 0;
    }
  }
  /* A device that isn't running AllMyStuff: quiet, washed-out, and not a
     connection target — present so you can see it's there, but it shouldn't
     invite a click the way your real machines do. */
  .node.meshonly {
    background: repeating-linear-gradient(
      135deg,
      var(--surface-2),
      var(--surface-2) 7px,
      var(--surface) 7px,
      var(--surface) 14px
    );
    border-style: dotted;
    border-color: var(--line-strong);
    box-shadow: var(--shadow-sm);
    opacity: 0.72;
  }
  .node.meshonly .avatar {
    filter: grayscale(1);
    opacity: 0.8;
  }
  .node.meshonly:hover {
    transform: none;
    box-shadow: var(--shadow-sm);
  }
  .node.selected {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-soft), var(--shadow-lg);
  }
  /* The device being dragged (the original stays in place; a ghost rides the
     cursor), and the one it's hovering over — the share-drop target in the
     sharing concept's violet. */
  /* Your own devices are draggable (to start a share) — show the grab hand,
     except in connect mode (tap to connect) or while already dragging. */
  .node.grabbable:not(.armed):not(.dragging-node) {
    cursor: grab;
  }
  .node.dragging-node {
    opacity: 0.8;
    cursor: grabbing;
  }
  .node.dragover {
    border-color: var(--c-share);
    box-shadow: 0 0 0 3px var(--c-share-soft), var(--shadow-lg);
  }
  .node.offline {
    opacity: 0.6;
  }
  .node.armed {
    cursor: pointer;
  }
  .canvas.armed .node:not(.armed) {
    opacity: 0.5;
  }
  .node.armed:hover {
    border-color: var(--ok);
    box-shadow: 0 0 0 3px oklch(0.8 0.17 150 / 0.18), var(--shadow-lg);
  }
  .node-top {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  .avatar {
    font-size: 1.25rem;
    line-height: 1;
    filter: drop-shadow(0 1px 1px rgba(0, 0, 0, 0.45));
  }
  .node-id {
    min-width: 0;
    flex: 1;
  }
  .node-label {
    font-weight: 650;
    font-size: 0.9rem;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .node-sub {
    font-size: 0.72rem;
    color: var(--ink-faint);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  /* A thin rule in the bottom row, between the refresh/settings controls and
     the console buttons. */
  .ctl-div {
    align-self: stretch;
    width: 1px;
    margin: 0.1rem 0.1rem;
    background: var(--line-strong);
    flex-shrink: 0;
  }
  /* The refresh control is a cbtn whose face is the online dot ringed by
     refresh arrows (clicking re-learns the node) rather than an icon. The ring
     fills the button, so it overrides cbtn's centred-svg sizing. */
  .status-refresh .refresh-ring {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    transform: none;
    fill: none;
    stroke: currentColor;
    stroke-width: 2;
    stroke-linecap: round;
    stroke-linejoin: round;
    opacity: 0.6;
    transition:
      opacity 0.12s ease,
      transform 0.5s ease;
  }
  .status-refresh:hover .refresh-ring {
    opacity: 1;
  }
  .status-refresh:active .refresh-ring {
    transform: rotate(-180deg);
  }
  /* Centred inside the ring rather than free-standing. */
  .dot {
    position: relative;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--line-strong);
    flex-shrink: 0;
  }
  .dot.on {
    background: var(--ok);
    box-shadow: 0 0 0 2px oklch(0.8 0.17 150 / 0.18);
  }
  /* The settings gear is a cbtn with a glyph face instead of an icon — sized a
     touch larger than the console icons so it reads clearly. */
  .node-gear {
    font-size: 1.05rem;
    line-height: 1;
  }
  /* The gear's actions menu — fixed-positioned, flipped on screen by JS. */
  .node-menu {
    position: fixed;
    z-index: 70;
    width: 216px;
    background: var(--surface);
    border: 1px solid var(--line-strong);
    border-radius: var(--r-md);
    box-shadow: var(--shadow-lg);
    padding: 0.3rem;
    display: flex;
    flex-direction: column;
    gap: 0.12rem;
    animation: nmenu 0.12s ease;
  }
  @keyframes nmenu {
    from {
      opacity: 0;
      transform: translateY(-3px);
    }
  }
  .nm-item {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    width: 100%;
    text-align: left;
    border: none;
    background: transparent;
    padding: 0.4rem 0.5rem;
    border-radius: var(--r-sm);
    cursor: pointer;
    color: var(--ink);
  }
  .nm-item:hover {
    background: var(--accent-soft);
  }
  .nm-icon {
    font-size: 0.95rem;
    width: 1.1rem;
    text-align: center;
    flex-shrink: 0;
  }
  .nm-text {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .nm-label {
    font-weight: 600;
    font-size: 0.85rem;
  }
  .nm-sub {
    font-size: 0.72rem;
    color: var(--ink-faint);
  }
  .node-meta {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.25rem;
  }
  /* Console buttons on the card — replace the old mesh pills. One per console
     you can open on the device; clicking opens it. */
  .node-consoles {
    display: flex;
    flex-wrap: wrap;
    gap: 0.3rem;
    margin-top: 0.15rem;
  }
  .cbtn {
    position: relative;
    display: grid;
    place-items: center;
    width: 1.55rem;
    height: 1.55rem;
    border-radius: var(--r-sm);
    border: 1px solid var(--line-strong);
    background: var(--surface-2);
    color: var(--ink-soft);
    box-shadow: var(--shadow-sm);
    transition: border-color 0.12s ease, color 0.12s ease, background 0.12s ease,
      transform 0.08s ease;
  }
  .cbtn:hover {
    border-color: var(--accent);
    color: var(--accent-ink);
    background: var(--surface);
  }
  .cbtn:active {
    transform: translateY(1px);
  }
  .cbtn :global(svg) {
    width: 0.95rem;
    height: 0.95rem;
    /* the glyphs sit right of centre — nudge them left so the spare pixels
       land on the right. */
    transform: translateX(-3px);
  }
  /* A quick black/grey tooltip (vs the slow native title) — shared by the
     console buttons below and the refresh/gear controls on the chips row. */
  .cbtn[data-tip]::after,
  .status-refresh[data-tip]::after,
  .node-gear[data-tip]::after {
    content: attr(data-tip);
    position: absolute;
    bottom: calc(100% + 5px);
    left: 50%;
    transform: translateX(-50%) translateY(3px);
    background: oklch(0.16 0.02 285 / 0.97);
    color: var(--ink);
    border: 1px solid var(--line-strong);
    border-radius: var(--r-sm);
    padding: 0.1rem 0.4rem;
    font-size: 0.64rem;
    font-weight: 650;
    white-space: nowrap;
    opacity: 0;
    pointer-events: none;
    box-shadow: var(--shadow-sm);
    transition: opacity 0.1s ease 0.1s, transform 0.1s ease 0.1s;
    z-index: 6;
  }
  .cbtn[data-tip]:hover::after,
  .status-refresh[data-tip]:hover::after,
  .node-gear[data-tip]:hover::after {
    opacity: 1;
    transform: translateX(-50%) translateY(0);
  }
  .tag {
    font-size: 0.64rem;
    font-weight: 650;
    padding: 0.08rem 0.4rem;
    border-radius: var(--r-pill);
    letter-spacing: 0.01em;
  }
  .tag.soft {
    background: var(--surface-2);
    color: var(--ink-soft);
  }
  /* "This device" — a pink chip that bumps over the top-left corner of your
     own card, so it reads as a label on the card rather than another tag. */
  .self-corner {
    position: absolute;
    top: -0.6rem;
    left: 0.7rem;
    z-index: 2;
    font-size: 0.58rem;
    font-weight: 800;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: #fff;
    background: linear-gradient(180deg, var(--accent-ink), var(--accent));
    border: 1px solid oklch(0.58 0.235 350);
    border-radius: var(--r-pill);
    padding: 0.1rem 0.5rem;
    box-shadow: 0 2px 6px -2px oklch(0.64 0.255 350 / 0.6),
      inset 0 1px 0 oklch(1 0 0 / 0.3);
    pointer-events: none;
  }
  .tag.mine {
    background: var(--ok-soft);
    color: var(--ok);
  }
  .tag.guest {
    background: var(--c-share-soft);
    color: var(--c-share-ink);
  }
  .tag.unclaimed {
    background: var(--surface-2);
    color: var(--ink-soft);
    border: 1px dashed var(--line-strong);
  }
  .tag.claimable {
    background: var(--accent-soft);
    color: var(--accent-ink);
    border: 1px solid var(--accent);
    font-weight: 700;
  }
  /* "Someone else's, not shared with me" — bronze keeps it distinct from a
     device actually shared with you (violet, above). */
  .tag.theirs {
    background: var(--bronze-soft);
    color: var(--bronze);
  }
  /* All fleet-role tags stay green; the ★ owner / ⚑ manager / 🔗 fleet
     glyph + word is what tells them apart (no per-role colour vomit). */
  .tag.fleet {
    background: var(--c-fleet-soft);
    color: var(--c-fleet-ink);
  }
  .tag.fleet.owner {
    font-weight: 750;
  }
  .tag.meshonly {
    background: var(--surface-2);
    color: var(--ink-faint);
    border: 1px dotted var(--line-strong);
  }
  .empty {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 0.35rem;
    text-align: center;
    padding: 1rem;
    pointer-events: none;
  }
  .empty-orb {
    font-size: 2.6rem;
    filter: drop-shadow(0 3px 6px oklch(0.64 0.255 350 / 0.35));
    animation: breathe 2.4s ease-in-out infinite;
  }
  .empty-title {
    font-weight: 750;
    font-size: 1.05rem;
    color: var(--ink);
  }
  .empty-sub {
    font-size: 0.84rem;
    color: var(--ink-faint);
    max-width: 22rem;
  }
  @keyframes breathe {
    0%,
    100% {
      transform: scale(1);
      opacity: 0.85;
    }
    50% {
      transform: scale(1.08);
      opacity: 1;
    }
  }
  .arm-banner {
    position: absolute;
    top: 1rem;
    left: 50%;
    transform: translateX(-50%);
    background: var(--surface-2);
    border: 1px solid var(--line-strong);
    color: var(--ink);
    padding: 0.5rem 0.7rem 0.5rem 1rem;
    border-radius: var(--r-pill);
    font-size: 0.82rem;
    font-weight: 550;
    display: flex;
    align-items: center;
    gap: 0.7rem;
    box-shadow: var(--shadow-lg);
    animation: drop 0.16s ease;
  }
  .arm-banner .btn {
    background: rgba(255, 255, 255, 0.1);
    border-color: transparent;
    color: var(--ink);
    box-shadow: none;
  }
  @keyframes drop {
    from {
      transform: translate(-50%, -8px);
      opacity: 0;
    }
  }
  .zoombar {
    position: absolute;
    right: 1rem;
    bottom: 1rem;
    display: flex;
    align-items: center;
    gap: 0.25rem;
    background: var(--surface);
    border: 1px solid var(--line);
    border-radius: var(--r-pill);
    padding: 0.2rem;
    box-shadow: var(--shadow-sm);
  }
  .zbtn {
    border: none;
    background: transparent;
    color: var(--ink-soft);
    width: 2rem;
    height: 1.8rem;
    border-radius: var(--r-pill);
    font-size: 0.95rem;
  }
  .zbtn:hover {
    background: var(--surface-2);
  }
  .zbtn.active {
    background: var(--accent-soft);
    color: var(--accent-ink);
  }
  .zbtn.wide {
    width: 3rem;
    font-size: 0.74rem;
    font-variant-numeric: tabular-nums;
  }
  .zsep {
    width: 1px;
    height: 1.1rem;
    background: var(--line);
    margin: 0 0.1rem;
  }
</style>
