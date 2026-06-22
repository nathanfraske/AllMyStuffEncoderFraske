<script lang="ts">
  // Per-network "Venue" pane. A mesh calls out at one or more venues — named
  // signaling / STUN / TURN sets — and its effective servers are their union.
  // The primary UI is a venue picker; the raw signaling/STUN/TURN editor is
  // kept as an "Edit servers directly" escape hatch (still writing through
  // `updateNetworkServers`, exactly as before).
  import { app } from "../../store.svelte";
  import { type TurnEntry } from "../../types";
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
  let advanced = $state(false);
  let saveAsName = $state("");

  const configs = $derived(app.networkConfigs);
  const selectedId = $derived(app.serversNetwork);
  const selected = $derived(selectedId ? app.networkConfig(selectedId) : undefined);
  const venues = $derived(app.venues);
  // The venue(s) this mesh currently uses (by its wire id). Picking one is the
  // common case; until bridging ships, "current" is the first of them.
  const chosen = $derived(selected ? app.venuesForNetwork(selected.network_id) : []);
  const chosenIds = $derived(new Set(chosen.map((v) => v.id)));
  const currentLabel = $derived(chosen.map((v) => v.label).join(", ") || "—");

  // (Re)load the raw editor when the selected network changes (or its config
  // first arrives). Editing in place afterward isn't clobbered by reloads.
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

  async function pick(venueId: string) {
    if (!selectedId) return;
    await app.setNetworkVenues(selectedId, [venueId]);
  }

  function manageVenues() {
    app.settingsTab = "venues";
  }

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

  function saveAsVenue() {
    if (!selectedId) return;
    const name = saveAsName.trim();
    if (!name) {
      app.toast("warn", "Name the venue first");
      return;
    }
    const v = app.saveServersAsVenue(selectedId, name);
    if (v) {
      app.toast("ok", `Saved “${v.label}” — this mesh now calls out at it`);
      saveAsName = "";
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
          {app.meshLabel(c)}
        </button>
      {/each}
    </div>

    {#if selected}
      <p class="lead">
        <b>{app.meshLabel(selected)}</b> calls out at <b>{currentLabel}</b>. A
        venue is a named set of signaling / STUN / TURN servers — picking one
        reconnects the mesh through it.
      </p>

      <!-- Venue picker -->
      <section class="grp">
        <div class="grp-head">
          <h4>Venue</h4>
          <button class="btn small" onclick={manageVenues}>Manage venues →</button>
        </div>
        <div class="venues">
          {#each venues as v (v.id)}
            <button class="venue" class:on={chosenIds.has(v.id)} onclick={() => pick(v.id)}>
              <span class="dot" aria-hidden="true"></span>
              <span class="vt">
                <span class="vl">{v.label}{#if v.builtin}<span class="chip mini">built-in</span>{/if}</span>
                <span class="vs">{v.url ? "remote" : "static"}</span>
              </span>
            </button>
          {/each}
        </div>

        <!-- Multi-venue tease — deliberately inert until bridging ships. -->
        <button class="add-venue" disabled title="One mesh across several venues — coming soon">＋ add another venue — coming soon</button>
        <p class="tease">one mesh across several venues — venue bridging is on the way</p>
      </section>

      <!-- Advanced: today's raw editor, preserved as an escape hatch. -->
      <section class="grp adv">
        <button class="disclose" aria-expanded={advanced} onclick={() => (advanced = !advanced)}>
          <span class="caret" class:open={advanced} aria-hidden="true">▸</span>
          Edit servers directly
        </button>
        {#if advanced}
          <p class="lead">
            Set this mesh's signaling / STUN / TURN by hand. Both ends must share a
            signaling relay to find each other; STUN/TURN handle NAT. Saving
            reconnects the mesh. Tip: capture these as a reusable venue below.
          </p>

          <!-- Signaling -->
          <div class="sub">
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
          </div>

          <!-- STUN -->
          <div class="sub">
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
          </div>

          <!-- TURN -->
          <div class="sub">
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
          </div>

          <div class="actions">
            <button class="btn small" onclick={applyDefaults}>Reset to MyOwnMesh defaults</button>
            <button class="btn small primary" disabled={saving} onclick={save}>{saving ? "Saving…" : "Save & reconnect"}</button>
          </div>

          <div class="save-as">
            <input class="field" placeholder="Save these servers as a venue named…" bind:value={saveAsName} />
            <button class="btn small" onclick={saveAsVenue}>Save as a venue</button>
          </div>
        {/if}
      </section>
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
  .venues {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
  }
  .venue {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    text-align: left;
    border: 1px solid var(--line-strong);
    background: var(--surface);
    border-radius: var(--r-sm);
    padding: 0.5rem 0.6rem;
  }
  .venue:hover {
    border-color: var(--accent);
  }
  .venue.on {
    background: var(--accent-soft);
    border-color: var(--accent);
  }
  .dot {
    width: 0.85rem;
    height: 0.85rem;
    border-radius: 50%;
    border: 2px solid var(--line-strong);
    flex-shrink: 0;
  }
  .venue.on .dot {
    border-color: var(--accent);
    background:
      radial-gradient(circle, var(--accent) 0 38%, transparent 42%);
  }
  .vt {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .vl {
    font-size: 0.84rem;
    font-weight: 600;
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }
  .vs {
    font-size: 0.7rem;
    color: var(--ink-faint);
  }
  .chip.mini {
    font-size: 0.6rem;
    padding: 0.04rem 0.4rem;
    color: var(--accent-ink);
    background: var(--accent-soft);
    border-color: var(--accent);
  }
  .add-venue {
    margin-top: 0.45rem;
    width: 100%;
    border: 1px dashed var(--line-strong);
    background: transparent;
    color: var(--ink-faint);
    border-radius: var(--r-sm);
    padding: 0.45rem;
    font-size: 0.8rem;
    font-weight: 600;
    opacity: 0.55;
    cursor: default;
  }
  .tease {
    font-size: 0.7rem;
    color: var(--ink-faint);
    text-align: center;
    margin: 0.25rem 0 0;
  }
  .adv {
    margin-top: 0.2rem;
  }
  .disclose {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    border: none;
    background: none;
    color: var(--ink-soft);
    font-size: 0.82rem;
    font-weight: 600;
    padding: 0.1rem 0;
  }
  .caret {
    display: inline-block;
    transition: transform 0.12s ease;
    color: var(--ink-faint);
  }
  .caret.open {
    transform: rotate(90deg);
  }
  .sub {
    border-top: 1px solid var(--line);
    padding: 0.6rem 0 0;
    margin-top: 0.6rem;
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
  .save-as {
    display: flex;
    gap: 0.4rem;
    margin-top: 0.6rem;
  }
  .hint {
    font-size: 0.78rem;
    color: var(--ink-soft);
    margin: 0 0 0.5rem;
    line-height: 1.45;
  }
</style>
