<script lang="ts">
  // A small modal that collects the fleet's custody (MFA) code before an
  // owner-authority governance action — evict, grant/withdraw role. It's only
  // shown when `app.fleetCodePrompt` is set, which the store does when fleet
  // MFA is enrolled (otherwise the action runs straight through). On submit it
  // calls the pending action with the code; success closes it, a refusal
  // (wrong code, or the daemon's reason) stays open with the error shown.
  import { app } from "../store.svelte";

  const prompt = $derived(app.fleetCodePrompt);

  let code = $state("");
  let busy = $state(false);
  let error = $state<string | null>(null);

  // Reset the field whenever a fresh action opens the prompt.
  $effect(() => {
    if (prompt) {
      code = "";
      error = null;
      busy = false;
    }
  });

  function cancel() {
    app.fleetCodePrompt = null;
  }

  async function submit() {
    const p = prompt;
    if (!p || !code.trim()) return;
    busy = true;
    error = null;
    try {
      await p.run(code.trim());
      // No toast — the prompt closing (and the fleet roster updating) is the
      // confirmation; an error stays inline below.
      app.fleetCodePrompt = null;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }
</script>

{#if prompt}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="scrim" onclick={cancel}>
    <div class="card" onclick={(e) => e.stopPropagation()}>
      <div class="title">🛡️ {prompt.title}</div>
      <p class="lead">
        This fleet has an authenticator enrolled on this device, so a fresh
        code is needed to authorise the change.
      </p>
      <input
        class="code"
        type="text"
        inputmode="numeric"
        autocomplete="one-time-code"
        placeholder="6-digit code or a recovery code"
        bind:value={code}
        onkeydown={(e) => e.key === "Enter" && submit()}
      />
      {#if error}<div class="err" role="alert">⚠ {error}</div>{/if}
      <div class="row">
        <button class="btn" onclick={cancel} disabled={busy}>Cancel</button>
        <button class="btn primary" onclick={submit} disabled={busy || !code.trim()}>
          {busy ? "Authorising…" : "Confirm"}
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  .scrim {
    position: fixed;
    inset: 0;
    z-index: 200;
    background: oklch(0 0 0 / 0.45);
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 1rem;
  }
  .card {
    width: 22rem;
    max-width: 100%;
    background: var(--surface);
    border: 1px solid var(--line-strong);
    border-radius: var(--r-md);
    box-shadow: var(--shadow-lg);
    padding: 1rem 1.1rem;
  }
  .title {
    font-size: 1rem;
    font-weight: 700;
    margin-bottom: 0.4rem;
  }
  .lead {
    font-size: 0.82rem;
    color: var(--ink-soft);
    line-height: 1.45;
    margin: 0 0 0.7rem;
  }
  .code {
    width: 100%;
    box-sizing: border-box;
    border: 1px solid var(--line-strong);
    border-radius: var(--r-sm);
    padding: 0.5rem 0.6rem;
    font-size: 0.95rem;
    font-family: var(--mono);
    background: var(--surface);
    color: var(--ink);
  }
  .code:focus {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  .err {
    margin-top: 0.5rem;
    font-size: 0.8rem;
    color: var(--danger);
    background: var(--danger-soft);
    border-radius: var(--r-sm);
    padding: 0.4rem 0.55rem;
  }
  .row {
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
    margin-top: 0.9rem;
  }
  .btn {
    border: 1px solid var(--line-strong);
    border-radius: var(--r-sm);
    padding: 0.45rem 0.85rem;
    font-size: 0.84rem;
    font-weight: 650;
    font-family: inherit;
    background: var(--surface);
    color: var(--ink-soft);
    cursor: pointer;
  }
  .btn.primary {
    background: var(--accent-soft);
    border-color: var(--accent);
    color: var(--accent-ink);
  }
  .btn:disabled {
    opacity: 0.55;
    cursor: default;
  }
</style>
