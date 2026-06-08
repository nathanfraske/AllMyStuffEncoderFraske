<script lang="ts">
  // Adding a connection is where AllMyStuff asks its one real question.
  // The mesh underneath already proves *who* a device is (cryptographic
  // identity — invisible here). All we ask the human is the thing only
  // they know: is this one of *yours*, or someone you're *sharing* with?
  import { app } from "../store.svelte";
  import type { Relationship } from "../types";

  type Choice = "mine" | "shared" | null;
  let choice = $state<Choice>(null);
  let name = $state("");

  function reset() {
    choice = null;
    name = "";
  }
  function close() {
    app.addNodeOpen = false;
    reset();
  }
  function add() {
    const label = name.trim();
    if (!label) return;
    const rel: Relationship =
      choice === "shared"
        ? { kind: "shared", person: { id: `person:${Date.now().toString(36)}`, name: label }, grants: [] }
        : { kind: "mine" };
    app.addNode(label, rel);
    close();
  }
</script>

<svelte:window onkeydown={(e) => e.key === "Escape" && close()} />

<div class="scrim">
  <button class="backdrop" onclick={close} aria-label="Close"></button>
  <div class="modal" role="dialog" aria-modal="true" aria-label="Add a connection" tabindex="-1">
    <button class="x" onclick={close} aria-label="Close">✕</button>

    {#if choice === null}
      <h3>Add to your world</h3>
      <p class="lead">Connecting is secure either way — you just tell us who's on the other end.</p>
      <div class="choices">
        <button class="choice mine" onclick={() => (choice = "mine")}>
          <span class="big">🧦</span>
          <b>A device that's mine</b>
          <span>Something you own or manage. It joins your fleet and connects freely with the rest of your stuff.</span>
        </button>
        <button class="choice shared" onclick={() => (choice = "shared")}>
          <span class="big">🧑‍🤝‍🧑</span>
          <b>Someone I'm sharing with</b>
          <span>A friend or family member. Nothing flows until you allow it — one thing at a time.</span>
        </button>
      </div>
    {:else}
      <h3>{choice === "mine" ? "Name this device" : "Who are you sharing with?"}</h3>
      <p class="lead">
        {#if choice === "mine"}
          Give it a friendly name so it's easy to spot on the graph.
        {:else}
          Use their name — you'll choose exactly what they can reach next.
        {/if}
      </p>
      <input
        class="field"
        placeholder={choice === "mine" ? "e.g. Garage NUC" : "e.g. Alex"}
        bind:value={name}
        onkeydown={(e) => e.key === "Enter" && add()}
      />
      <div class="actions">
        <button class="btn ghost" onclick={reset}>← Back</button>
        <button class="btn primary" disabled={!name.trim()} onclick={add}>
          {choice === "mine" ? "Add device" : "Start sharing"}
        </button>
      </div>
    {/if}
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
    width: 30rem;
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
  .choices {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0.7rem;
  }
  .choice {
    text-align: left;
    border: 1.5px solid var(--line-strong);
    background: var(--surface);
    border-radius: var(--r-md);
    padding: 1rem;
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
    transition: border-color 0.12s ease, transform 0.08s ease, box-shadow 0.12s ease;
  }
  .choice:hover {
    transform: translateY(-2px);
    box-shadow: var(--shadow-md);
  }
  .choice.mine:hover {
    border-color: var(--m-input);
  }
  .choice.shared:hover {
    border-color: var(--warn);
  }
  .choice .big {
    font-size: 1.8rem;
  }
  .choice b {
    font-size: 0.96rem;
  }
  .choice span:last-child {
    font-size: 0.8rem;
    color: var(--ink-soft);
    line-height: 1.4;
  }
  .field {
    width: 100%;
    border: 1.5px solid var(--line-strong);
    border-radius: var(--r-sm);
    padding: 0.7rem 0.8rem;
    font-size: 1rem;
    font-family: inherit;
    margin-bottom: 1rem;
  }
  .field:focus {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  .actions {
    display: flex;
    justify-content: space-between;
    gap: 0.5rem;
  }
</style>
