<script lang="ts">
  // The body of a dedicated console window (opened with `?console=<node>`):
  // boot the store, wait for the target machine to be *ready* — found on
  // the mesh, its AllMyStuff presence landed, and its relationship
  // resolved (yours / shared) — then open the session in this window and
  // keep the OS window title naming the machine. The main window never
  // renders this — it opens console windows instead of popovers.
  //
  // Readiness is staged, never a one-shot gate: this window's store boots
  // from nothing, and the facts arrive in order (mesh node → presence →
  // fleet roster, which is what flips your owner machine to "yours"). A
  // gate that fired on the first stage would refuse machines that are
  // perfectly yours a beat later.
  import { onMount } from "svelte";
  import { app } from "../store.svelte";
  import { setWindowTitle } from "../tauri";
  import { displayName, isAppNode } from "../types";
  import Console from "./Console.svelte";
  import Toasts from "./Toasts.svelte";

  let { target }: { target: string } = $props();

  let attempted = $state(false);
  const node = $derived(app.machineByAnyId(target));

  const stage = $derived.by(() => {
    const n = node;
    if (!n) return "finding" as const;
    if (!isAppNode(n)) return "presence" as const;
    if (n.relationship.kind === "unclaimed") return "relationship" as const;
    return "ready" as const;
  });

  onMount(() => {
    void app.init();
  });

  $effect(() => {
    if (attempted || app.consoleNodeId || stage !== "ready") return;
    attempted = true;
    app.openConsoleHere(node!.id);
  });

  $effect(() => {
    const n = app.consoleNode;
    if (n) void setWindowTitle(`${displayName(n)} — AllMyStuff console`);
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
  {:else if stage === "relationship"}
    <div class="notice">
      <div class="glyph">🔗</div>
      <p><b>{displayName(node!)}</b> is here — resolving whether it's yours
        (fleet roster loading)… If this machine was never claimed or shared,
        do that from the graph first and this window will pick it up.</p>
    </div>
  {:else if stage === "presence"}
    <div class="notice">
      <div class="glyph">📡</div>
      <p><b>{displayName(node!)}</b> is on the mesh — waiting for its
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
  .notice b {
    color: #d7d2ec;
  }
</style>
