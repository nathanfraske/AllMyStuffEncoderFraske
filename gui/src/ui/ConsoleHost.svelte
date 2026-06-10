<script lang="ts">
  // The body of a dedicated console window (opened with `?console=<node>`):
  // boot the store, wait for the target machine to appear (the scan and
  // presence land asynchronously), open the session in *this* window, and
  // keep the OS window title naming the machine. The main window never
  // renders this — it opens console windows instead of popovers.
  import { onMount } from "svelte";
  import { app } from "../store.svelte";
  import { setWindowTitle } from "../tauri";
  import { displayName, isAppNode } from "../types";
  import Console from "./Console.svelte";
  import Toasts from "./Toasts.svelte";

  let { target }: { target: string } = $props();

  // One attempt per window: if the gate refuses (machine vanished, not
  // yours any more) we show the failure instead of retrying into a toast
  // storm.
  let attempted = $state(false);
  const node = $derived(app.machineByAnyId(target));

  onMount(() => {
    void app.init();
  });

  $effect(() => {
    if (attempted || app.consoleNodeId) return;
    const n = node;
    if (!n) return; // still discovering — keep waiting
    // Hold until its presence detail has landed too (init pulls a session
    // snapshot, so this resolves in moments when the machine is really
    // there) — a half-hydrated node would be refused as "not on AllMyStuff".
    if (!isAppNode(n)) return;
    attempted = true;
    app.openConsoleHere(n.id);
  });

  $effect(() => {
    const node = app.consoleNode;
    if (node) void setWindowTitle(`${displayName(node)} — AllMyStuff console`);
  });
</script>

<div class="host">
  {#if app.consoleNodeId}
    <Console windowed={true} />
  {:else if attempted}
    <div class="notice">
      <div class="glyph">🚫</div>
      <p>Couldn't open a console for this machine — see the message that just
        popped for the reason. Close this window and try again from the graph.</p>
    </div>
  {:else if node && !isAppNode(node)}
    <div class="notice">
      <div class="glyph">📡</div>
      <p><b>{displayName(node)}</b> is on the mesh — waiting for its
        AllMyStuff presence (its devices and ownership) to land…</p>
    </div>
  {:else}
    <div class="notice">
      <div class="glyph">📡</div>
      <p>Finding this machine on the mesh…</p>
    </div>
  {/if}
  <Toasts />
</div>

<style>
  .host {
    height: 100vh;
    background: #14121f;
  }
  .notice {
    height: 100%;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 0.6rem;
    color: #9a93b8;
    text-align: center;
    padding: 2rem;
  }
  .glyph {
    font-size: 2.6rem;
  }
  .notice p {
    max-width: 26rem;
    font-size: 0.9rem;
    line-height: 1.5;
  }
</style>
