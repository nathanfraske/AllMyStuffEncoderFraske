<script lang="ts">
  // You don't fabricate machines — they join your mesh. This sheet explains
  // how: install AllMyStuff on the other device, join this network, approve
  // it. Then it appears on the graph on its own via presence.
  import { app } from "../store.svelte";
  import { networkDisplayName } from "../types";

  const net = $derived(app.activeNetwork);
  let copied = $state(false);

  function close() {
    app.addMachineOpen = false;
  }
  async function copyId() {
    if (!net) return;
    try {
      await navigator.clipboard.writeText(net.network_id);
      copied = true;
      setTimeout(() => (copied = false), 1500);
    } catch {
      app.toast("warn", "Couldn't copy — select it by hand");
    }
  }
  function openNetworks() {
    app.addMachineOpen = false;
    app.networksOpen = true;
  }
</script>

<svelte:window onkeydown={(e) => e.key === "Escape" && close()} />

<div class="scrim">
  <button class="backdrop" onclick={close} aria-label="Close"></button>
  <div class="modal" role="dialog" aria-modal="true" aria-label="Add a machine" tabindex="-1">
    <button class="x" onclick={close} aria-label="Close">✕</button>

    <h3>Add a machine</h3>
    <p class="lead">
      Machines aren't added by hand — they join your mesh and show up here on
      their own. Three steps on the other device:
    </p>

    <ol class="steps">
      <li><b>Install AllMyStuff</b> on the machine you want to add.</li>
      <li>
        <b>Join your network.</b>
        {#if net}
          Use this network handle:
          <div class="idrow">
            <code title={net.network_id}>{net.network_id}</code>
            <button class="btn small" onclick={copyId}>{copied ? "Copied ✓" : "Copy"}</button>
          </div>
          {#if net.label?.trim()}<span class="netname">“{networkDisplayName(net)}”</span>{/if}
        {:else}
          You're not on a network yet.
          <div class="idrow">
            <button class="btn small primary" onclick={openNetworks}>Set up a network →</button>
          </div>
        {/if}
      </li>
      <li>
        <b>Approve it.</b> When it shows up waiting, approve it under
        <button class="link" onclick={openNetworks}>Networks → Approvals</button>.
      </li>
    </ol>

    <p class="foot">
      Sharing with a <i>person</i> works the same way — they join as a guest,
      and you allow exactly what they can reach from their drawer.
    </p>
  </div>
</div>

<style>
  .backdrop {
    position: absolute;
    inset: 0;
    border: none;
    background: transparent;
    cursor: default;
  }
  .modal {
    position: relative;
    z-index: 1;
    width: 32rem;
    max-width: 92vw;
    background: var(--surface);
    border-radius: var(--r-lg);
    padding: 1.5rem;
    box-shadow: var(--shadow-lg);
    animation: rise 0.16s ease;
  }
  @keyframes rise {
    from {
      transform: translateY(12px) scale(0.98);
      opacity: 0;
    }
  }
  .x {
    position: absolute;
    top: 0.9rem;
    right: 0.9rem;
    border: none;
    background: var(--surface-2);
    color: var(--ink-soft);
    width: 1.9rem;
    height: 1.9rem;
    border-radius: 50%;
  }
  h3 {
    margin: 0 0 0.3rem;
    font-size: 1.25rem;
  }
  .lead {
    color: var(--ink-soft);
    font-size: 0.9rem;
    margin: 0 0 1rem;
    line-height: 1.45;
  }
  .steps {
    margin: 0 0 1rem;
    padding-left: 1.2rem;
    display: flex;
    flex-direction: column;
    gap: 0.7rem;
    font-size: 0.9rem;
    line-height: 1.5;
  }
  .idrow {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin-top: 0.4rem;
  }
  code {
    flex: 1;
    min-width: 0;
    background: var(--surface-2);
    border: 1px solid var(--line);
    border-radius: var(--r-sm);
    padding: 0.35rem 0.55rem;
    font-size: 0.82rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .netname {
    display: inline-block;
    margin-top: 0.3rem;
    font-size: 0.78rem;
    color: var(--ink-faint);
  }
  .link {
    border: none;
    background: none;
    color: var(--accent-ink);
    font: inherit;
    text-decoration: underline;
    cursor: pointer;
    padding: 0;
  }
  .foot {
    font-size: 0.8rem;
    color: var(--ink-faint);
    line-height: 1.45;
    margin: 0;
  }
</style>
