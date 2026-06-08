<script lang="ts">
  // Groups are isolatable bundles you move as one — the "RDC kit": your
  // monitor + keyboard + mouse + speaker + mic, pointed at one machine so
  // your desk becomes its terminal. Build one from a device's stuff, then
  // send the whole bundle anywhere.
  import { app } from "../store.svelte";
  import { originIcon } from "../types";

  let creating = $state(false);
  let groupName = $state("");
  let picked = $state<Set<string>>(new Set());

  const sourceNode = $derived(app.selectedNode);
  const sourceCaps = $derived(sourceNode ? app.capsOf(sourceNode.id) : []);

  function toggle(id: string) {
    const next = new Set(picked);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    picked = next;
  }
  function create() {
    if (!sourceNode || picked.size === 0 || !groupName.trim()) return;
    app.createGroup(groupName.trim(), sourceNode.id, [...picked]);
    creating = false;
    groupName = "";
    picked = new Set();
  }

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
    <h4>Groups</h4>
    <button class="btn small" onclick={() => (creating = !creating)}>{creating ? "Close" : "＋ New"}</button>
  </div>

  {#if creating}
    <div class="create">
      {#if !sourceNode}
        <p class="hint">Open a device on the graph, then pick what to bundle.</p>
      {:else}
        <p class="hint">Bundling from <b>{sourceNode.label}</b></p>
        <div class="picks">
          {#each sourceCaps as c (c.id)}
            <button class="pick" class:on={picked.has(c.id)} onclick={() => toggle(c.id)}>
              <span>{originIcon(c.origin, c.media)}</span>
              {c.label}
            </button>
          {/each}
        </div>
        <div class="create-row">
          <input class="field" placeholder="Group name (e.g. My desk)" bind:value={groupName} />
          <button class="btn primary small" disabled={picked.size === 0 || !groupName.trim()} onclick={create}>
            Make group
          </button>
        </div>
      {/if}
    </div>
  {/if}

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
    {#if app.catalog.groups.length === 0}
      <li class="empty">No groups yet. Bundle a device's screen, keyboard and audio into a “desk” you can beam anywhere.</li>
    {/if}
  </ul>
</div>

<style>
  .bar {
    position: absolute;
    left: 1rem;
    bottom: 1rem;
    width: 19rem;
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
    margin-bottom: 0.3rem;
  }
  h4 {
    margin: 0;
    font-size: 0.82rem;
    color: var(--ink-soft);
  }
  .hint {
    font-size: 0.78rem;
    color: var(--ink-soft);
    margin: 0.3rem 0;
  }
  .picks {
    display: flex;
    flex-wrap: wrap;
    gap: 0.3rem;
    margin: 0.3rem 0 0.5rem;
    max-height: 8rem;
    overflow-y: auto;
  }
  .pick {
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    font-size: 0.74rem;
    border: 1px solid var(--line-strong);
    background: var(--surface);
    border-radius: var(--r-pill);
    padding: 0.22rem 0.55rem;
    color: var(--ink);
  }
  .pick.on {
    background: var(--accent-soft);
    border-color: var(--accent);
    color: var(--accent-ink);
  }
  .create-row {
    display: flex;
    gap: 0.4rem;
  }
  .field {
    flex: 1;
    min-width: 0;
    border: 1px solid var(--line-strong);
    border-radius: var(--r-sm);
    padding: 0.4rem 0.55rem;
    font-size: 0.82rem;
    font-family: inherit;
  }
  .field:focus {
    outline: none;
    border-color: var(--accent);
  }
  .groups {
    list-style: none;
    margin: 0.4rem 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
    max-height: 12rem;
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
  .groups li.empty {
    display: block;
    font-size: 0.76rem;
    color: var(--ink-faint);
    line-height: 1.4;
    background: transparent;
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
