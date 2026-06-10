<script lang="ts">
  import { onMount } from "svelte";
  import { app } from "../store.svelte";
  import { appVersion, setWindowTitle } from "../tauri";
  import { networkDisplayName } from "../types";
  import Graph from "./Graph.svelte";
  import NodeDrawer from "./NodeDrawer.svelte";
  import BundlesBar from "./BundlesBar.svelte";
  import SettingsPanel from "./SettingsPanel.svelte";
  import ApprovalsPopup from "./ApprovalsPopup.svelte";
  import ShareSheet from "./ShareSheet.svelte";
  import Console from "./Console.svelte";
  import Toasts from "./Toasts.svelte";

  // Which build this is. Comes from gui/src-tauri/Cargo.toml via Tauri
  // (kept in sync by scripts/bump-version.sh); empty in the in-browser
  // preview, where the badge below simply doesn't render.
  let version = $state("");

  onMount(() => {
    // Wire up live backend data (scan + presence + routes) if the Tauri
    // backend is here; otherwise the demo graph stands in so the app is
    // never empty.
    void app.init();

    // Surface the running version like MyOwnMesh / MyOwnLLM — in the brand
    // and stamped into the window title.
    void appVersion().then((v) => {
      if (v) {
        version = v;
        void setWindowTitle(`AllMyStuff ${v}`);
      }
    });
  });
</script>

<div class="shell">
  <header class="topbar">
    <div class="brand">
      <span class="logo">🧦</span>
      <div class="brandtext">
        <div class="name">
          AllMyStuff{#if version}<span class="ver" title="Running version">v{version}</span>{/if}
        </div>
        <div class="tag">everything you own, wired together</div>
      </div>
    </div>

    <div class="summary">
      <span class="chip"><b>{app.mineCount}</b> yours</span>
      <span class="chip"><b>{app.sharedCount}</b> shared</span>
      <button
        class="chip net"
        class:live={app.backendConnected && app.networks.length > 0}
        onclick={() => app.openSettings("networks")}
        title="Manage your networks"
      >
        <span class="net-dot"></span>
        {!app.backendConnected
          ? "demo mode"
          : app.networks.length > 1
            ? `${app.networks.length} networks`
            : app.activeNetwork
              ? networkDisplayName(app.activeNetwork)
              : "no network"}
      </button>
    </div>

    <div class="actions">
      <!-- A device asking to join: the outlined, pulsing nudge that opens the
           approval popup (the code-grid panel). -->
      {#if app.freshJoins.length > 0}
        <button class="nudge" onclick={() => app.openApprovals()} title="A device wants to join your network">
          <span class="nudge-mark" aria-hidden="true">!</span>
          {app.freshJoins.length}
          {app.freshJoins.length === 1 ? "device wants in" : "devices want in"}
        </button>
      {/if}
      <button class="btn" onclick={() => app.openSettings("networks")} title="Networks, identity & approvals">🌐 Networks</button>
      <button class="btn" onclick={() => app.hydrateFromBackend()} title="Scan this machine">↻ Scan</button>
      <button class="btn gear" class:has-alert={app.freshJoins.length > 0} onclick={() => app.openSettings()} title="Settings" aria-label="Settings">
        ⚙
        {#if app.freshJoins.length > 0}<span class="gear-badge" aria-hidden="true"></span>{/if}
      </button>
    </div>
  </header>

  <main class="stage">
    <Graph />
    <BundlesBar />
    <NodeDrawer />
  </main>

  {#if app.settingsOpen}
    <SettingsPanel />
  {/if}
  {#if app.approvalsOpen}
    <ApprovalsPopup />
  {/if}
  <Console />
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
  .ver {
    margin-left: 0.4rem;
    font-size: 0.68rem;
    font-weight: 600;
    color: var(--ink-faint);
    vertical-align: 0.12em;
    letter-spacing: 0;
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
    align-items: center;
  }
  /* The "a device wants to join" nudge — outlined and gently pulsing, with a
     bold exclamation, so a new device asking to join is impossible to miss
     but never alarming. Tapping it opens the code-grid approval popup. */
  .nudge {
    display: inline-flex;
    align-items: center;
    gap: 0.45rem;
    border: 1.5px solid var(--warn);
    background: #fff8ee;
    color: #97631a;
    padding: 0.34rem 0.7rem 0.34rem 0.5rem;
    border-radius: var(--r-pill);
    font-size: 0.8rem;
    font-weight: 650;
    box-shadow: 0 0 0 0 rgba(224, 137, 42, 0.5);
    animation: nudge-pulse 1.8s ease-out infinite;
  }
  .nudge:hover {
    background: #fdedd2;
  }
  .nudge-mark {
    display: grid;
    place-items: center;
    width: 1.15rem;
    height: 1.15rem;
    border-radius: 50%;
    background: var(--warn);
    color: #fff;
    font-weight: 800;
    font-size: 0.78rem;
    line-height: 1;
  }
  @keyframes nudge-pulse {
    0% {
      box-shadow: 0 0 0 0 rgba(224, 137, 42, 0.45);
    }
    70% {
      box-shadow: 0 0 0 8px rgba(224, 137, 42, 0);
    }
    100% {
      box-shadow: 0 0 0 0 rgba(224, 137, 42, 0);
    }
  }
  .gear {
    position: relative;
    font-size: 1rem;
    padding: 0.5rem 0.7rem;
  }
  .gear-badge {
    position: absolute;
    top: 0.3rem;
    right: 0.35rem;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--warn);
    box-shadow: 0 0 0 2px var(--surface);
  }
  .stage {
    position: relative;
    flex: 1;
    min-height: 0;
    display: flex;
  }
</style>
