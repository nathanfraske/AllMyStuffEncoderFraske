<script lang="ts">
  // The body of the popped-out CEC Console window (opened with `?cec=1`).
  // Boots the store like the other host windows, force-unlocks the CEC
  // surface (this window IS the console, so it doesn't wait on the secret
  // gesture), and renders the full CecSection standalone. The main window
  // never renders this — it opens this window instead.
  import { onMount } from "svelte";
  import { app } from "../store.svelte";
  import { setWindowTitle } from "../tauri";
  import CecSection from "./settings/CecSection.svelte";
  import Toasts from "./Toasts.svelte";

  onMount(() => {
    void app.init();
    // This window is the CEC console by construction — make sure the surface
    // is unlocked and its state is loaded even though no one performed the
    // reveal gesture in this fresh window.
    app.cecEnabled = true;
    void app.loadCec();
    void setWindowTitle("CEC Console");
  });
</script>

<div class="cec-host">
  <CecSection windowed={true} />
  <Toasts />
</div>

<style>
  .cec-host {
    height: 100vh;
    overflow-y: auto;
    background: var(--bg, #f6f6fb);
    padding: 1rem 1.2rem 1.6rem;
  }
</style>
