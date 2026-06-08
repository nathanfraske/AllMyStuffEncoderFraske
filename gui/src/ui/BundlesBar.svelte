<script lang="ts">
  // Bundles are pre-set kits with category slots — "My desk" (screen +
  // keyboard/mouse + mic + speakers), "Call kit", etc. Pick one, it auto-
  // fills from this machine's devices, then point it at another machine and
  // the whole thing connects (and disconnects) as one.
  import { app } from "../store.svelte";
  import { BUNDLE_TEMPLATES } from "../types";

  const draft = $derived(BUNDLE_TEMPLATES.find((t) => t.id === app.bundleDraftId) ?? null);

  function memberLabels(ids: string[]): string {
    return ids
      .map((id) => app.capability(id)?.label ?? "")
      .filter(Boolean)
      .slice(0, 3)
      .join(", ");
  }
</script>

<div class="bar">
  <div class="bar-head">
    <h4>Bundles</h4>
    {#if draft}
      <button class="btn small" onclick={() => app.cancelBundle()}>Close</button>
    {/if}
  </div>

  {#if draft}
    <!-- Filling a bundle: each slot auto-filled from this machine, editable. -->
    <div class="draft">
      <p class="hint">{draft.icon} <b>{draft.name}</b> — pick what fills each slot, then send it to a machine.</p>
      <div class="slots">
        {#each draft.slots as slot (slot.id)}
          {@const cands = app.bundleCandidates(slot)}
          <label class="slot">
            <span class="slot-label">{slot.label}</span>
            <select
              value={app.bundleSlots[slot.id] ?? ""}
              onchange={(e) => app.setBundleSlot(slot.id, e.currentTarget.value)}
            >
              <option value="">— none —</option>
              {#each cands as c (c.id)}
                <option value={c.id}>{c.label}</option>
              {/each}
            </select>
          </label>
        {/each}
      </div>
      <button class="btn primary small wide" onclick={() => app.sendBundle()}>Send to a machine →</button>
    </div>
  {:else}
    <!-- Choose a pre-set bundle to fill. -->
    <div class="templates">
      {#each BUNDLE_TEMPLATES as t (t.id)}
        <button class="tpl" onclick={() => app.startBundle(t.id)} title={t.blurb}>
          <span class="tpl-icon">{t.icon}</span>
          <span class="tpl-name">{t.name}</span>
        </button>
      {/each}
    </div>
  {/if}

  {#if app.catalog.groups.length > 0}
    <ul class="groups">
      {#each app.catalog.groups as g (g.id)}
        <li class:armed={app.groupPickerFor === g.id}>
          <div class="g-main">
            <div class="g-name">📦 {g.name}</div>
            <div class="g-sub">{g.members.length} things · {memberLabels(g.members)}…</div>
          </div>
          {#if app.groupPickerFor === g.id}
            <button class="btn small" onclick={() => app.cancelGroupConnect()}>Cancel</button>
          {:else}
            <button class="btn small primary" onclick={() => app.startGroupConnect(g.id)}>Send…</button>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .bar {
    position: absolute;
    left: 1rem;
    bottom: 1rem;
    width: 20rem;
    max-width: calc(100vw - 2rem);
    background: var(--surface);
    border: 1px solid var(--line);
    border-radius: var(--r-md);
    box-shadow: var(--shadow-md);
    padding: 0.7rem 0.8rem 0.8rem;
    z-index: 15;
  }
  .bar-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 0.4rem;
  }
  h4 {
    margin: 0;
    font-size: 0.82rem;
    color: var(--ink-soft);
  }
  .hint {
    font-size: 0.78rem;
    color: var(--ink-soft);
    margin: 0 0 0.5rem;
    line-height: 1.4;
  }
  .templates {
    display: flex;
    gap: 0.4rem;
    flex-wrap: wrap;
  }
  .tpl {
    flex: 1;
    min-width: 5rem;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.2rem;
    border: 1px solid var(--line-strong);
    background: var(--surface);
    border-radius: var(--r-md);
    padding: 0.6rem 0.4rem;
    cursor: pointer;
    transition: border-color 0.12s ease, transform 0.08s ease, box-shadow 0.12s ease;
  }
  .tpl:hover {
    transform: translateY(-2px);
    border-color: var(--accent);
    box-shadow: var(--shadow-sm);
  }
  .tpl-icon {
    font-size: 1.4rem;
  }
  .tpl-name {
    font-size: 0.76rem;
    font-weight: 600;
    text-align: center;
  }
  .slots {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    margin-bottom: 0.6rem;
  }
  .slot {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  .slot-label {
    flex: 0 0 8rem;
    font-size: 0.78rem;
    color: var(--ink-soft);
  }
  select {
    flex: 1;
    min-width: 0;
    border: 1px solid var(--line-strong);
    border-radius: var(--r-sm);
    padding: 0.3rem 0.4rem;
    font-size: 0.78rem;
    font-family: inherit;
    background: var(--surface);
  }
  .wide {
    width: 100%;
  }
  .groups {
    list-style: none;
    margin: 0.5rem 0 0;
    padding: 0.5rem 0 0;
    border-top: 1px solid var(--line);
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
    max-height: 11rem;
    overflow-y: auto;
  }
  .groups li {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    background: var(--surface-2);
    border-radius: var(--r-sm);
    padding: 0.45rem 0.55rem;
  }
  .groups li.armed {
    background: var(--accent-soft);
    box-shadow: 0 0 0 1.5px var(--accent);
  }
  .g-main {
    flex: 1;
    min-width: 0;
  }
  .g-name {
    font-size: 0.85rem;
    font-weight: 600;
  }
  .g-sub {
    font-size: 0.72rem;
    color: var(--ink-faint);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
</style>
