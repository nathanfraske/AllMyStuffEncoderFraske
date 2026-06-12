<script lang="ts">
  import { app } from "../store.svelte";
  import { displayName, mediaColor, humanBytes, isAppNode, type MediaKind } from "../types";
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

  function fleetKeyOf(n: MeshNode): { key: string; label: string } {
    if (
      n.kind === "this" ||
      n.relationship.kind === "mine" ||
      app.isFleetMember(n.id) ||
      (!!n.owner && app.isMe(n.owner))
    ) {
      const label = app.fleetName
        ? `${app.fleetName}'s fleet`
        : app.fleetMemberIds.size > 1
          ? "Your fleet"
          : "Your devices";
      return { key: "mine", label };
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
  // Two layouts over the same nodes: the radial default ("this" centred,
  // fleets seated together around the ring) and the grouped grid (one
  // labelled section per fleet) — switched from the zoom controls.

  type ViewMode = "radial" | "grid";
  const VIEW_STORE_KEY = "allmystuff.graphView.v1";
  let view = $state<ViewMode>(loadView());

  function loadView(): ViewMode {
    try {
      return localStorage.getItem(VIEW_STORE_KEY) === "grid" ? "grid" : "radial";
    } catch {
      return "radial";
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
  type Edge = { id: string; x1: number; y1: number; x2: number; y2: number; cx: number; cy: number; color: string };
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
      });
    }
    return out;
  });

  // ---- pan / zoom ---------------------------------------------------
  let panX = $state(0);
  let panY = $state(0);
  let zoom = $state(1);
  let dragging = $state(false);
  let dragStart = $state<{ x: number; y: number; panX: number; panY: number } | null>(null);
  const MIN_ZOOM = 0.4;
  const MAX_ZOOM = 2.2;

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
    // Let node clicks through — and never capture the pointer on the
    // floating controls, or their buttons swallow the click (capturing
    // re-targets the pointerup, so the browser never composes a click).
    if (target?.closest?.(".node, .zoombar, .arm-banner")) return;
    if (app.dragFrom) {
      app.cancelConnect();
      return;
    }
    app.selectNode(null);
    dragging = true;
    dragStart = { x: e.clientX, y: e.clientY, panX, panY };
    (e.currentTarget as Element).setPointerCapture?.(e.pointerId);
  }
  function onPointerMove(e: PointerEvent) {
    if (!dragging || !dragStart) return;
    panX = dragStart.panX + (e.clientX - dragStart.x);
    panY = dragStart.panY + (e.clientY - dragStart.y);
  }
  function onPointerUp(e: PointerEvent) {
    dragging = false;
    dragStart = null;
    (e.currentTarget as Element).releasePointerCapture?.(e.pointerId);
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
    } else {
      app.selectNode(app.selectedNodeId === n.id ? null : n.id);
    }
  }
</script>

<div
  class="canvas"
  class:dragging
  class:armed
  bind:this={canvas}
  onwheel={onWheel}
  onpointerdown={onPointerDown}
  onpointermove={onPointerMove}
  onpointerup={onPointerUp}
  onpointercancel={onPointerUp}
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
      {@const shared = n.relationship.kind === "shared"}
      {@const unclaimed = n.relationship.kind === "unclaimed"}
      {@const meshonly = !isAppNode(n)}
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div
        class="node"
        class:self={n.kind === "this"}
        class:shared
        class:unclaimed
        class:meshonly
        class:selected={app.selectedNodeId === n.id}
        class:armed={armed && targetable(n)}
        class:offline={!n.online}
        style="left: {p.x - NODE_W / 2}px; top: {p.y - NODE_H / 2}px; width: {NODE_W}px; min-height: {NODE_H}px;"
        onclick={(e) => {
          e.stopPropagation();
          onNodeClick(n);
        }}
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
        <div class="node-top">
          <span class="avatar">{nodeAvatar(n)}</span>
          <div class="node-id">
            <div class="node-label" title={displayName(n)}>{displayName(n)}</div>
            <div class="node-sub">
              {#if shared && n.relationship.kind === "shared"}
                shared with {n.relationship.person.name}
              {:else if meshonly}
                on the mesh · not running AllMyStuff
              {:else if n.summary}
                {n.summary.cpu}
              {:else}
                device
              {/if}
            </div>
          </div>
          <span class="dot" class:on={n.online} title={n.online ? "online" : "offline"}></span>
        </div>
        <div class="node-meta">
          {#if n.kind === "this"}<span class="tag you">this device</span>{/if}
          {#if meshonly}<span class="tag meshonly">not on AllMyStuff</span>
          {:else if shared}<span class="tag guest">guest</span>
          {:else if unclaimed}
            <!-- A device whose advert names an owner that isn't us is
                 claimed by someone else — say that, not "unclaimed". -->
            {#if n.owner && !app.isMe(n.owner)}
              <span class="tag theirs">someone else's</span>
            {:else}
              <span class="tag unclaimed">{n.claimable ? "claimable" : "unclaimed"}</span>
            {/if}
          {:else if n.kind !== "this"}<span class="tag mine">yours</span>{/if}
          {#if app.isFleetMember(n.id)}<span class="tag fleet" title="In your owned fleet (shared key)">🔗 fleet</span>{/if}
          {#if n.summary}<span class="tag soft">{n.summary.device_count} things</span>{/if}
          {#if n.summary}<span class="tag soft">{humanBytes(n.summary.ram_bytes)}</span>{/if}
        </div>
        {#if n.networks && n.networks.length}
          <div class="node-nets" title="On {n.networks.join(', ')}">
            {#each n.networks as net}<span class="net-chip">{net}</span>{/each}
          </div>
        {/if}
      </div>
    {/each}
  </div>

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

<style>
  .canvas {
    position: relative;
    flex: 1;
    overflow: hidden;
    cursor: grab;
    touch-action: none;
    user-select: none;
  }
  .canvas.dragging {
    cursor: grabbing;
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
  @keyframes flow {
    to {
      stroke-dashoffset: -30;
    }
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
  .section.mine {
    border-color: oklch(0.64 0.255 350 / 0.45);
    background: oklch(0.64 0.255 350 / 0.05);
  }
  .section.unknown {
    border-style: dotted;
    background: transparent;
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
    color: var(--accent-ink);
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
    box-shadow: var(--shadow-lg);
  }
  .node.self {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-soft), var(--shadow-md);
  }
  .node.shared {
    border-color: oklch(0.74 0.085 72 / 0.55);
    background: linear-gradient(180deg, var(--surface-2), var(--surface));
  }
  .node.unclaimed {
    border-style: dashed;
    border-color: var(--line-strong);
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
  .dot {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--line-strong);
    flex-shrink: 0;
  }
  .dot.on {
    background: var(--ok);
    box-shadow: 0 0 0 3px oklch(0.8 0.17 150 / 0.16);
  }
  .node-meta {
    display: flex;
    flex-wrap: wrap;
    gap: 0.25rem;
  }
  .node-nets {
    display: flex;
    flex-wrap: wrap;
    gap: 0.2rem;
    margin-top: 0.1rem;
  }
  .net-chip {
    font-size: 0.6rem;
    font-weight: 650;
    background: var(--violet-soft);
    border: 1px solid oklch(0.62 0.2 292 / 0.35);
    color: var(--violet);
    border-radius: var(--r-pill);
    padding: 0.02rem 0.36rem;
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
  .tag.you {
    background: var(--accent-soft);
    color: var(--accent-ink);
  }
  .tag.mine {
    background: var(--ok-soft);
    color: var(--ok);
  }
  .tag.guest {
    background: var(--bronze-soft);
    color: var(--bronze);
  }
  .tag.unclaimed {
    background: var(--surface-2);
    color: var(--ink-soft);
    border: 1px dashed var(--line-strong);
  }
  .tag.theirs {
    background: var(--violet-soft);
    color: var(--violet);
  }
  .tag.fleet {
    background: var(--accent-soft);
    color: var(--accent-ink);
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
