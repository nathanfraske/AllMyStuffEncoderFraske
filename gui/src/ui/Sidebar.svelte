<script lang="ts">
  // The left-docked sidebar — one panel over the two planes that share this
  // side: **Rooms** (zoom-like calls between machines) and **Sites** (the
  // reverse-proxied services your fleet exposes). Like the device-details
  // drawer it's a real docked panel, not a floating popup: resizeable from
  // its right edge and collapsible to a thin rail, so it never blocks the
  // graph. The tab strip carries live badges (room knocks, reachable sites).
  import { app } from "../store.svelte";
  import RoomsTab from "./RoomsTab.svelte";
  import SitesTab from "./SitesTab.svelte";

  const WIDTH_KEY = "allmystuff.sidebar.width.v1";
  const COLLAPSED_KEY = "allmystuff.sidebar.collapsed.v1";
  const MIN_W = 232;
  const MAX_W = 520;
  // Default to the narrow (min) width, expanded — the panel stays out of the
  // graph's way but its content is laid out to wrap multi-line when narrow.
  const DEFAULT_W = MIN_W;

  function loadWidth(): number {
    try {
      const v = Number(localStorage.getItem(WIDTH_KEY));
      return v >= MIN_W && v <= MAX_W ? v : DEFAULT_W;
    } catch {
      return DEFAULT_W;
    }
  }
  function loadCollapsed(): boolean {
    try {
      return localStorage.getItem(COLLAPSED_KEY) === "1";
    } catch {
      return false;
    }
  }

  let width = $state(loadWidth());
  let collapsed = $state(loadCollapsed());
  let resizing = $state(false);
  let el = $state<HTMLElement | null>(null);

  const roomAttention = $derived(
    Object.values(app.roomKnocks).reduce((n, ks) => n + ks.length, 0),
  );
  const siteCount = $derived(app.sitesByMachine.reduce((n, g) => n + g.sites.length, 0));

  function setCollapsed(v: boolean) {
    collapsed = v;
    try {
      localStorage.setItem(COLLAPSED_KEY, v ? "1" : "0");
    } catch {
      /* private mode — just don't persist */
    }
  }

  function select(tab: "rooms" | "sites") {
    app.sidebarTab = tab;
    if (collapsed) setCollapsed(false);
    if (tab !== "rooms") app.roomDraftOpen = false;
  }

  // The grab handle does double duty: drag to resize, click (no drag) to
  // collapse/expand. `armed` = pressed; `moved` tells the two apart.
  let armed = false;
  let moved = false;
  let startX = 0;
  function startResize(e: PointerEvent) {
    armed = true;
    moved = false;
    startX = e.clientX;
    (e.currentTarget as Element).setPointerCapture?.(e.pointerId);
    e.preventDefault();
  }
  function onResizeMove(e: PointerEvent) {
    if (!armed || !el) return;
    if (!moved && Math.abs(e.clientX - startX) < 4) return;
    moved = true;
    resizing = true;
    // The panel is flush against the stage's left edge; its width is the
    // distance from that edge to the pointer.
    const left = el.getBoundingClientRect().left;
    width = Math.min(MAX_W, Math.max(MIN_W, e.clientX - left));
  }
  function endResize(e: PointerEvent) {
    if (!armed) return;
    armed = false;
    (e.currentTarget as Element).releasePointerCapture?.(e.pointerId);
    if (moved) {
      resizing = false;
      try {
        localStorage.setItem(WIDTH_KEY, String(Math.round(width)));
      } catch {
        /* private mode — the width just doesn't persist */
      }
    } else {
      // A click, not a drag → collapse.
      setCollapsed(true);
    }
  }
</script>

<aside
  class="sidebar"
  class:collapsed
  class:resizing
  bind:this={el}
  style={collapsed ? "" : `width: ${width}px`}
>
  {#if collapsed}
    <!-- The thin rail: the two tab icons, click to expand into that tab. -->
    <div class="rail">
      <button class="rail-btn" title="Rooms" aria-label="Rooms" onclick={() => select("rooms")}>
        🪩
        {#if roomAttention > 0}<span class="rail-attn" aria-label="{roomAttention} asking to join"></span>{/if}
      </button>
      <button class="rail-btn" title="Sites" aria-label="Sites" onclick={() => select("sites")}>
        🌐
        {#if siteCount > 0}<span class="rail-count">{siteCount}</span>{/if}
      </button>
    </div>
  {:else}
    <div class="head">
      <div class="tabs" role="tablist" aria-label="Rooms and Sites">
        <button
          class="tab"
          class:active={app.sidebarTab === "rooms"}
          role="tab"
          aria-selected={app.sidebarTab === "rooms"}
          onclick={() => select("rooms")}
        >
          🪩 Rooms
          {#if app.rooms.length > 0}<span class="count">{app.rooms.length}</span>{/if}
          {#if roomAttention > 0 && app.sidebarTab !== "rooms"}<span class="attn" title="{roomAttention} asking to join"></span>{/if}
        </button>
        <button
          class="tab"
          class:active={app.sidebarTab === "sites"}
          role="tab"
          aria-selected={app.sidebarTab === "sites"}
          onclick={() => select("sites")}
        >
          🌐 Sites
          {#if siteCount > 0}<span class="count">{siteCount}</span>{/if}
        </button>
      </div>
      <button class="collapse" title="Collapse" aria-label="Collapse sidebar" onclick={() => setCollapsed(true)}>‹</button>
    </div>

    <div class="body" role="tabpanel">
      {#if app.sidebarTab === "rooms"}
        <RoomsTab />
      {:else}
        <SitesTab />
      {/if}
    </div>

    <!-- The grab handle: drag to resize, click to collapse. Snapped to the
         panel's edge, with a 6-dot grip near the top. -->
    <div
      class="resizer"
      role="separator"
      aria-label="Resize or collapse sidebar"
      aria-orientation="vertical"
      title="Drag to resize · click to collapse"
      onpointerdown={startResize}
      onpointermove={onResizeMove}
      onpointerup={endResize}
      onpointercancel={endResize}
    >
      <span class="grip" aria-hidden="true">
        <i></i><i></i><i></i><i></i><i></i><i></i>
      </span>
    </div>
  {/if}
</aside>

<style>
  .sidebar {
    position: relative;
    flex-shrink: 0;
    height: 100%;
    min-height: 0;
    display: flex;
    flex-direction: column;
    background: var(--surface);
    border-right: 1px solid var(--line);
    z-index: 12;
  }
  .sidebar.collapsed {
    width: 2.75rem;
  }
  .sidebar.resizing {
    user-select: none;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 0.25rem;
    padding: 0.55rem 0.6rem 0.5rem 0.8rem;
    border-bottom: 1px solid var(--line);
    flex-shrink: 0;
  }
  .tabs {
    display: flex;
    gap: 0.25rem;
    flex: 1;
    min-width: 0;
  }
  .tab {
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    border: none;
    background: transparent;
    color: var(--ink-soft);
    font: inherit;
    font-size: 0.82rem;
    font-weight: 600;
    padding: 0.25rem 0.5rem;
    border-radius: var(--r-sm);
    cursor: pointer;
    position: relative;
  }
  .tab:hover {
    background: var(--surface-2);
    color: var(--ink);
  }
  .tab.active {
    background: var(--accent-soft);
    color: var(--accent-ink);
  }
  .count {
    font-size: 0.64rem;
    font-weight: 700;
    background: var(--surface-2);
    color: var(--ink-faint);
    border-radius: var(--r-pill);
    padding: 0 0.3rem;
    line-height: 1.4;
  }
  .tab.active .count {
    background: var(--surface);
  }
  .attn {
    position: absolute;
    top: 0.15rem;
    right: 0.1rem;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--warn);
    box-shadow: 0 0 0 2px var(--surface);
  }
  .collapse {
    flex-shrink: 0;
    border: none;
    background: transparent;
    color: var(--ink-faint);
    font-size: 1.1rem;
    line-height: 1;
    padding: 0.2rem 0.4rem;
    border-radius: var(--r-sm);
    cursor: pointer;
  }
  .collapse:hover {
    background: var(--surface-2);
    color: var(--ink);
  }
  .body {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 0.7rem 0.8rem 0.9rem;
  }
  .resizer {
    position: absolute;
    right: 0;
    top: 0;
    bottom: 0;
    width: 10px;
    cursor: grab;
    touch-action: none;
  }
  .sidebar.resizing .resizer {
    cursor: grabbing;
  }
  /* The hairline that lights up the whole edge on hover/resize. */
  .resizer::after {
    content: "";
    position: absolute;
    right: 3px;
    top: 0;
    bottom: 0;
    width: 2px;
    background: transparent;
    transition: background 0.12s ease;
  }
  .resizer:hover::after,
  .sidebar.resizing .resizer::after {
    background: var(--accent);
  }
  /* The 6-dot grip, near the top of the edge, snapped to the panel. */
  .grip {
    position: absolute;
    top: 1.1rem;
    /* straddle the panel's outer (right) edge, leaning into the gap */
    right: -7px;
    display: grid;
    grid-template-columns: repeat(2, 3px);
    grid-auto-rows: 3px;
    gap: 3px;
    padding: 4px 2px;
    border-radius: var(--r-sm);
    background: var(--surface-2);
    border: 1px solid var(--line-strong);
    box-shadow: var(--shadow-sm);
  }
  .grip i {
    width: 3px;
    height: 3px;
    border-radius: 50%;
    background: var(--ink-faint);
    transition: background 0.12s ease;
  }
  .resizer:hover .grip,
  .sidebar.resizing .grip {
    border-color: var(--accent);
  }
  .resizer:hover .grip i,
  .sidebar.resizing .grip i {
    background: var(--accent-ink);
  }
  /* Collapsed rail */
  .rail {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.4rem;
    padding: 0.6rem 0;
  }
  .rail-btn {
    position: relative;
    border: none;
    background: transparent;
    font-size: 1.15rem;
    line-height: 1;
    padding: 0.4rem;
    border-radius: var(--r-sm);
    cursor: pointer;
  }
  .rail-btn:hover {
    background: var(--surface-2);
  }
  .rail-attn {
    position: absolute;
    top: 0.2rem;
    right: 0.2rem;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--warn);
    box-shadow: 0 0 0 2px var(--surface);
  }
  .rail-count {
    position: absolute;
    bottom: 0.05rem;
    right: 0.05rem;
    font-size: 0.55rem;
    font-weight: 700;
    background: var(--accent);
    color: #fff;
    border-radius: var(--r-pill);
    padding: 0 0.22rem;
  }
</style>
