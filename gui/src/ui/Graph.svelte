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

  // Radial layout: "this" in the middle, everything else on a ring with
  // the things you own first and the people you share with after, so the
  // eye reads "my fleet, then my guests."
  const layout = $derived.by((): Placed[] => {
    const cx = width / 2;
    const cy = height / 2;
    const nodes = app.catalog.nodes;
    // Centre the local machine by its definitive marker (`kind === "this"`),
    // not by id: a presence snapshot can move `localId` to the real session id
    // before a scan re-homes the node off its first-scan placeholder, and we
    // must never leave the centre empty with "me" stranded out on the ring.
    const me = nodes.find((n) => n.kind === "this") ?? nodes.find((n) => n.id === app.localId);
    const others = nodes.filter((n) => n.id !== me?.id);
    others.sort((a, b) => {
      const rank = (n: MeshNode) => (n.relationship.kind === "mine" ? 0 : 1);
      return rank(a) - rank(b) || a.label.localeCompare(b.label);
    });
    const placed: Placed[] = [];
    if (me) placed.push({ node: me, x: cx, y: cy });
    const radius = Math.max(180, Math.min(width, height) / 2 - 130);
    others.forEach((n, i) => {
      const a = -Math.PI / 2 + (i * 2 * Math.PI) / Math.max(1, others.length);
      placed.push({ node: n, x: cx + Math.cos(a) * radius, y: cy + Math.sin(a) * radius });
    });
    return placed;
  });

  const posOf = $derived.by(() => {
    const m = new Map<string, Placed>();
    for (const p of layout) m.set(p.node.id, p);
    return m;
  });

  // Edges: one per route, connecting the two nodes the capabilities live
  // on. Curved + coloured by media, with parallel routes fanned apart.
  type Edge = { id: string; x1: number; y1: number; x2: number; y2: number; cx: number; cy: number; color: string; group: boolean };
  const edges = $derived.by((): Edge[] => {
    const pairCount = new Map<string, number>();
    const out: Edge[] = [];
    for (const r of app.catalog.routes) {
      const from = app.catalog.capabilities.find((c) => c.id === r.from);
      const to = app.catalog.capabilities.find((c) => c.id === r.to);
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
        group: !!r.group,
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
    if (target?.closest?.(".node")) return; // let node clicks through
    if (app.dragFrom) {
      app.cancelConnect();
      return;
    }
    if (app.groupPickerFor) {
      app.cancelGroupConnect();
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

  // Whether a node is a valid action target right now (connect drop or
  // group destination) — drives the pulsing highlight. A device must be
  // running AllMyStuff and already claimed to be a target.
  const armed = $derived(!!app.dragFrom || !!app.groupPickerFor);
  function targetable(n: MeshNode): boolean {
    return isAppNode(n) && n.relationship.kind !== "unclaimed";
  }

  function onNodeClick(n: MeshNode) {
    if (app.dragFrom || app.groupPickerFor) {
      // Mesh-only and not-yet-claimed nodes aren't connection targets.
      if (!isAppNode(n)) {
        app.toast("warn", `${n.label} isn't running AllMyStuff`);
        return;
      }
      if (n.relationship.kind === "unclaimed") {
        app.toast("warn", `Claim ${n.label} first — open it to adopt it`);
        return;
      }
      if (app.dragFrom) app.dropConnectOnNode(n.id);
      else if (app.groupPickerFor) app.connectGroupTo(app.groupPickerFor, n.id);
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
        <circle cx="1.5" cy="1.5" r="1.5" fill="rgba(108,92,231,0.10)" />
      </pattern>
    </defs>
    <rect x="0" y="0" {width} {height} fill="url(#dots)" />
    <g transform="translate({panX} {panY}) scale({zoom})">
      {#each edges as e (e.id)}
        <path
          class="wire"
          class:group={e.group}
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
          {:else if unclaimed}<span class="tag unclaimed">{n.claimable ? "claimable" : "unclaimed"}</span>
          {:else if n.kind !== "this"}<span class="tag mine">yours</span>{/if}
          {#if n.summary}<span class="tag soft">{n.summary.device_count} things</span>{/if}
          {#if n.summary}<span class="tag soft">{humanBytes(n.summary.ram_bytes)}</span>{/if}
        </div>
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
      {#if app.dragFrom}
        Tap a device to connect — or tap empty space to cancel
        <button class="btn small" onclick={() => app.cancelConnect()}>Cancel</button>
      {:else}
        Tap a device to send the group there
        <button class="btn small" onclick={() => app.cancelGroupConnect()}>Cancel</button>
      {/if}
    </div>
  {/if}

  <div class="zoombar">
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
  .wire.group {
    stroke-width: 5;
    opacity: 0.3;
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
    border-color: #f0c27a;
    background: linear-gradient(180deg, #fffaf0, #ffffff);
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
    box-shadow: 0 0 0 3px rgba(26, 160, 109, 0.18), var(--shadow-lg);
  }
  .node-top {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  .avatar {
    font-size: 1.25rem;
    line-height: 1;
    filter: drop-shadow(0 1px 1px rgba(0, 0, 0, 0.1));
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
    box-shadow: 0 0 0 3px rgba(26, 160, 109, 0.16);
  }
  .node-meta {
    display: flex;
    flex-wrap: wrap;
    gap: 0.25rem;
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
    background: #e7f6ef;
    color: #137a52;
  }
  .tag.guest {
    background: #fdedd2;
    color: #97631a;
  }
  .tag.unclaimed {
    background: var(--surface-2);
    color: var(--ink-soft);
    border: 1px dashed var(--line-strong);
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
    filter: drop-shadow(0 3px 6px rgba(108, 92, 231, 0.25));
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
    background: var(--ink);
    color: #fff;
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
    background: rgba(255, 255, 255, 0.16);
    border-color: transparent;
    color: #fff;
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
  .zbtn.wide {
    width: 3rem;
    font-size: 0.74rem;
    font-variant-numeric: tabular-nums;
  }
</style>
