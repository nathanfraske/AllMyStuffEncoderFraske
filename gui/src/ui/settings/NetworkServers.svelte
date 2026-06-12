<script lang="ts">
  // Per-network transport servers: the signaling relay (where two devices
  // find each other), STUN (NAT reflexion), and TURN (relay when there's no
  // direct path). Defaults to MyOwnMesh's reference servers — the usual reason
  // nothing connects is two devices on different signaling relays, so pinning
  // the same one is what gets them talking.
  import { app } from "../../store.svelte";
  import { networkDisplayName, type TurnEntry } from "../../types";
  import {
    MYOWNMESH_SIGNALING,
    MYOWNMESH_STUN,
    MYOWNMESH_TURN_URL,
    MYOWNMESH_TURN_USER,
    MYOWNMESH_TURN_PASS,
  } from "../../tauri";

  let loadedId = $state<string | null>(null);
  let signaling = $state<string[]>([]);
  let stun = $state<string[]>([]);
  let turn = $state<TurnEntry[]>([]);
  let saving = $state(false);

  const configs = $derived(app.networkConfigs);
  const selectedId = $derived(app.serversNetwork);
  const selected = $derived(selectedId ? app.networkConfig(selectedId) : undefined);

  // (Re)load the editor when the selected network changes (or its config first
  // arrives). Editing in place afterward isn't clobbered by background reloads.
  $effect(() => {
    const id = app.serversNetwork;
    if (!id || id === loadedId) return;
    const cfg = app.networkConfig(id);
    if (!cfg) return;
    signaling = [...(cfg.signaling?.servers ?? [])];
    stun = (cfg.stun_servers ?? []).flatMap((s) => s.urls);
    turn = (cfg.turn_servers ?? []).map((t) => ({
      url: t.urls[0] ?? "",
      username: t.username ?? "",
      credential: t.credential ?? "",
    }));
    loadedId = id;
  });

  function applyDefaults() {
    signaling = [MYOWNMESH_SIGNALING];
    stun = [MYOWNMESH_STUN];
    turn = [{ url: MYOWNMESH_TURN_URL, username: MYOWNMESH_TURN_USER, credential: MYOWNMESH_TURN_PASS }];
  }

  async function save() {
    if (!selectedId) return;
    saving = true;
    try {
      await app.updateNetworkServers(selectedId, { signaling, stun, turn });
    } finally {
      saving = false;
    }
  }
</script>

<div class="servers">
  {#if configs.length === 0}
    <p class="hint">No networks to configure yet — create or join one under Status.</p>
  {:else}
    <!-- Network picker -->
    <div class="picker">
      {#each configs as c (c.id)}
        <button class="pick" class:active={selectedId === c.id} onclick={() => (app.serversNetwork = c.id)}>
          {networkDisplayName(c)}
        </button>
      {/each}
    </div>

    {#if selected}
      <p class="lead">
        Servers for <b>{networkDisplayName(selected)}</b>. Both ends of a network must
        share a signaling relay to find each other; STUN/TURN handle NAT. Saving
        reconnects the network.
      </p>

      <!-- Signaling -->
      <section class="grp">
        <div class="grp-head">
          <h4>Signaling relays</h4>
          <button class="btn small" onclick={() => (signaling = [...signaling, ""])}>＋ Add</button>
        </div>
        {#each signaling as _, i}
          <div class="row">
            <input class="field mono" placeholder="wss://…" bind:value={signaling[i]} />
            <button class="x" title="Remove" onclick={() => (signaling = signaling.filter((_, j) => j !== i))}>✕</button>
          </div>
        {/each}
        {#if signaling.length === 0}<p class="empty">None — peers fall back to the built-in public relays (less reliable).</p>{/if}
      </section>

      <!-- STUN -->
      <section class="grp">
        <div class="grp-head">
          <h4>STUN servers</h4>
          <button class="btn small" onclick={() => (stun = [...stun, ""])}>＋ Add</button>
        </div>
        {#each stun as _, i}
          <div class="row">
            <input class="field mono" placeholder="stun:host:3478" bind:value={stun[i]} />
            <button class="x" title="Remove" onclick={() => (stun = stun.filter((_, j) => j !== i))}>✕</button>
          </div>
        {/each}
        {#if stun.length === 0}<p class="empty">None.</p>{/if}
      </section>

      <!-- TURN -->
      <section class="grp">
        <div class="grp-head">
          <h4>TURN servers</h4>
          <button class="btn small" onclick={() => (turn = [...turn, { url: "", username: "", credential: "" }])}>＋ Add</button>
        </div>
        {#each turn as _, i}
          <div class="turn">
            <div class="row">
              <input class="field mono" placeholder="turn:host:3478" bind:value={turn[i].url} />
              <button class="x" title="Remove" onclick={() => (turn = turn.filter((_, j) => j !== i))}>✕</button>
            </div>
            <div class="row creds">
              <input class="field" placeholder="username" bind:value={turn[i].username} />
              <input class="field" placeholder="credential" bind:value={turn[i].credential} />
            </div>
          </div>
        {/each}
        {#if turn.length === 0}<p class="empty">None — symmetric-NAT / CGNAT peers may fail to connect.</p>{/if}
      </section>

      <div class="actions">
        <button class="btn small" onclick={applyDefaults}>Reset to MyOwnMesh defaults</button>
        <button class="btn small primary" disabled={saving} onclick={save}>{saving ? "Saving…" : "Save & reconnect"}</button>
      </div>
    {/if}
  {/if}
</div>

<style>
  .servers {
    padding-top: 0.6rem;
  }
  .picker {
    display: flex;
    flex-wrap: wrap;
    gap: 0.3rem;
    margin-bottom: 0.6rem;
  }
  .pick {
    border: 1px solid var(--line-strong);
    background: var(--surface);
    border-radius: var(--r-pill);
    padding: 0.3rem 0.7rem;
    font-size: 0.8rem;
    font-weight: 600;
    color: var(--ink-soft);
  }
  .pick.active {
    background: var(--accent-soft);
    border-color: var(--accent);
    color: var(--accent-ink);
  }
  .lead {
    font-size: 0.8rem;
    color: var(--ink-soft);
    line-height: 1.45;
    margin: 0 0 0.6rem;
  }
  .grp {
    border-top: 1px solid var(--line);
    padding: 0.7rem 0;
  }
  .grp-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 0.4rem;
  }
  h4 {
    margin: 0;
    font-size: 0.88rem;
  }
  .row {
    display: flex;
    gap: 0.35rem;
    margin-bottom: 0.35rem;
    align-items: center;
  }
  .creds {
    padding-left: 0.2rem;
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
  .field.mono {
    font-family: var(--mono);
  }
  .field:focus {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  .turn {
    border: 1px solid var(--line);
    border-radius: var(--r-sm);
    padding: 0.4rem;
    margin-bottom: 0.4rem;
    background: var(--surface-2);
  }
  .x {
    border: none;
    background: var(--surface-2);
    color: var(--ink-faint);
    width: 1.8rem;
    height: 1.8rem;
    border-radius: var(--r-sm);
    flex-shrink: 0;
  }
  .x:hover {
    background: var(--danger-soft);
    color: var(--danger);
  }
  .empty {
    font-size: 0.74rem;
    color: var(--ink-faint);
    margin: 0.1rem 0;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.4rem;
    margin-top: 0.8rem;
    border-top: 1px solid var(--line);
    padding-top: 0.8rem;
  }
</style>
