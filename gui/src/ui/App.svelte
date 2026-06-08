<script lang="ts">
  import { onMount } from "svelte";
  import { app } from "../store.svelte";
  import { networkDisplayName } from "../types";
  import Graph from "./Graph.svelte";
  import NodeDrawer from "./NodeDrawer.svelte";
  import BundlesBar from "./BundlesBar.svelte";
  import AddMachineSheet from "./AddMachineSheet.svelte";
  import NetworksSheet from "./NetworksSheet.svelte";
  import ShareSheet from "./ShareSheet.svelte";
  import Toasts from "./Toasts.svelte";

  onMount(() => {
    // Wire up live backend data (scan + presence + routes) if the Tauri
    // backend is here; otherwise the demo graph stands in so the app is
    // never empty.
    void app.init();
  });
</script>

<div class="shell">
  <header class="topbar">
    <div class="brand">
      <span class="logo">🧦</span>
      <div class="brandtext">
        <div class="name">AllMyStuff</div>
        <div class="tag">everything you own, wired together</div>
      </div>
    </div>

    <div class="summary">
      <span class="chip"><b>{app.mineCount}</b> yours</span>
      <span class="chip"><b>{app.sharedCount}</b> shared</span>
      <span class="chip net" class:live={app.backendConnected}>
        <span class="net-dot"></span>
        {app.backendConnected
          ? app.activeNetwork
            ? networkDisplayName(app.activeNetwork)
            : "mesh connected"
          : "demo mode"}
      </span>
    </div>

    <div class="actions">
      <button class="btn" onclick={() => (app.networksOpen = true)} title="Networks, identity & approvals">🌐 Networks</button>
      <button class="btn" onclick={() => app.hydrateFromBackend()} title="Scan this machine">↻ Scan</button>
      <button class="btn primary" onclick={() => (app.addMachineOpen = true)}>＋ Add machine</button>
    </div>
  </header>

  <main class="stage">
    <Graph />
    <BundlesBar />
    <NodeDrawer />
  </main>

  {#if app.addMachineOpen}
    <AddMachineSheet />
  {/if}
  {#if app.networksOpen}
    <NetworksSheet />
  {/if}
  <ShareSheet />
  <Toasts />
</div>

<style>
  .shell {
    display: flex;
    flex-direction: column;
    height: 100vh;
    min-height: 0;
  }
  .topbar {
    display: flex;
    align-items: center;
    gap: 1rem;
    padding: 0.6rem 1rem;
    background: rgba(255, 255, 255, 0.7);
    backdrop-filter: blur(8px);
    border-bottom: 1px solid var(--line);
    flex-shrink: 0;
    z-index: 30;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 0.6rem;
  }
  .logo {
    font-size: 1.5rem;
    filter: drop-shadow(0 2px 3px rgba(108, 92, 231, 0.25));
  }
  .name {
    font-weight: 800;
    font-size: 1.1rem;
    letter-spacing: -0.01em;
  }
  .tag {
    font-size: 0.72rem;
    color: var(--ink-faint);
  }
  .summary {
    display: flex;
    gap: 0.4rem;
    margin-left: auto;
  }
  .chip b {
    color: var(--ink);
  }
  .chip.net {
    background: #fdeaee;
    color: var(--danger);
    border-color: #f7d6dd;
  }
  .chip.net.live {
    background: #e7f6ef;
    color: #137a52;
    border-color: #c9ebda;
  }
  .net-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: currentColor;
  }
  .actions {
    display: flex;
    gap: 0.5rem;
  }
  .stage {
    position: relative;
    flex: 1;
    min-height: 0;
    display: flex;
  }
</style>
