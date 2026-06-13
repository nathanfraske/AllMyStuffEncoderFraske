<script lang="ts">
  // The body of a dedicated video-popout window (opened with
  // `?video=<key>`): boot the store, wait for the stream's facts to land
  // — the capability for a `cap:` key (a console input the popout wires
  // itself), the route for a `share:` key (a room share it merely
  // watches) — then hand the window to the popout and keep the OS title
  // naming the stream. The main window never renders this.
  //
  // Same staged readiness as ConsoleHost: this window's store boots from
  // nothing and presence/routes arrive moments later; a gate that fired
  // on the first look would refuse streams that are perfectly there a
  // beat after.
  import { onMount } from "svelte";
  import { app } from "../store.svelte";
  import { setWindowTitle } from "../tauri";
  import VideoPopout from "./VideoPopout.svelte";
  import Toasts from "./Toasts.svelte";

  let { target }: { target: string } = $props();

  let attempted = $state(false);

  const capId = $derived(target.startsWith("cap:") ? target.slice(4) : null);
  const routeId = $derived(target.startsWith("share:") ? target.slice(6) : null);

  const ready = $derived.by(() => {
    if (capId) {
      const cap = app.capability(capId);
      if (!cap) return false;
      // The popout boots its own store from nothing, so a source peer can read
      // `unclaimed` for a beat before its ownership/fleet gossip converges.
      // Wiring then would route-propose against an unclaimed node and toast a
      // spurious "isn't yours yet — claim it first" — a red herring, since the
      // stream was already authorized wherever it was popped out from. Hold the
      // wire until the relationship settles (it never stays unclaimed for a
      // machine you own or share).
      const node = app.machineByAnyId(cap.node);
      return !!node && node.relationship.kind !== "unclaimed";
    }
    if (routeId) return app.catalog.routes.some((r) => r.id === routeId);
    return false;
  });

  onMount(() => {
    void app.init();
  });

  $effect(() => {
    if (attempted || app.videoPopoutKey || !ready) return;
    attempted = true;
    app.initVideoPopout(target);
  });

  // Title the OS window with resolved labels (the opener seeded a best
  // guess before this window existed).
  $effect(() => {
    if (capId) {
      const cap = app.capability(capId);
      if (!cap) return;
      const machine = app.machineByAnyId(cap.node);
      void setWindowTitle(`${cap.label} · ${machine?.label ?? "AllMyStuff"}`);
    } else if (routeId) {
      const route = app.catalog.routes.find((r) => r.id === routeId);
      const from = route ? app.capabilityForDisplay(route.from) : undefined;
      if (!from) return;
      const who = app.roomWho(from.node);
      const what = route!.media === "video" ? "camera" : "screen";
      void setWindowTitle(`${who.who}'s ${what} · AllMyStuff`);
    }
  });
</script>

<div class="host">
  {#if app.videoPopoutKey}
    <VideoPopout />
  {:else if !capId && !routeId}
    <div class="notice">
      <div class="glyph">🚫</div>
      <p>This window was opened for a stream it can't read. Close it and pop
        the video out again.</p>
    </div>
  {:else}
    <div class="notice">
      <div class="glyph">📡</div>
      <p>Finding this stream on the mesh…</p>
    </div>
  {/if}
  <Toasts />
</div>

<style>
  .host {
    height: 100vh;
    background: #000;
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
