<script lang="ts">
  // The bottom-left sidebar — one tabbed panel over two planes that share
  // this corner: **Rooms** (the zoom-like calls between machines) and
  // **Sites** (the reverse-proxied services your fleet exposes). The tab
  // strip carries live badges so a knock on a room, or a freshly-exposed
  // site, is visible without switching tabs.
  import { app } from "../store.svelte";
  import RoomsTab from "./RoomsTab.svelte";
  import SitesTab from "./SitesTab.svelte";

  // Total knocks waiting across all rooms — the attention dot on the Rooms
  // tab when you're looking at Sites.
  const roomAttention = $derived(
    Object.values(app.roomKnocks).reduce((n, ks) => n + ks.length, 0),
  );
  // How many sites you can reach across your fleet — a quiet count chip.
  const siteCount = $derived(
    app.sitesByMachine.reduce((n, g) => n + g.sites.length, 0),
  );

  function select(tab: "rooms" | "sites") {
    app.sidebarTab = tab;
    // Leaving Rooms shouldn't strand a half-made room draft open behind the
    // Sites tab — close the composer so coming back is clean.
    if (tab !== "rooms") app.roomDraftOpen = false;
  }
</script>

<div class="bar">
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

  <div class="body" role="tabpanel">
    {#if app.sidebarTab === "rooms"}
      <RoomsTab />
    {:else}
      <SitesTab />
    {/if}
  </div>
</div>

<style>
  .bar {
    position: absolute;
    left: 1rem;
    bottom: 1rem;
    width: 21rem;
    max-width: calc(100vw - 2rem);
    background: var(--surface);
    border: 1px solid var(--line);
    border-radius: var(--r-md);
    box-shadow: var(--shadow-md);
    padding: 0.6rem 0.8rem 0.8rem;
    z-index: 15;
  }
  .tabs {
    display: flex;
    gap: 0.25rem;
    margin-bottom: 0.6rem;
    border-bottom: 1px solid var(--line);
    padding-bottom: 0.5rem;
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
</style>
