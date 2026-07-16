<script lang="ts">
  // Venues — the named "where a mesh calls out" sets (signaling / STUN / TURN).
  // A mesh's effective servers are its venues combined (the union). This tab
  // manages the library: the built-in Public venue is pinned first and is
  // read-only; the rest can be created (static or remote), edited, exported and
  // deleted. The model + store already own all the logic — this is the UI.
  import { app } from "../../store.svelte";
  import type { TurnEntry } from "../../types";
  import { newVenueId, type Venue } from "../../venues";
  import { exportVenue, tryParseVenue, venueFromExport } from "../../venue-settings";
  import { exportNetworkFile, isMobile } from "../../tauri";

  // Same rule as NetworksSection: export needs the desktop's save dialog +
  // backend write command, neither of which exists on a phone. Hide it.
  const canExport = !isMobile();

  const venues = $derived(app.venues);
  const draft = $derived(app.venueDraft);
  // Which venues are actually used by a live mesh — the same merger the venues
  // pill shows. The on/off switch (shared with the pill) is offered only for
  // these, since toggling a venue no mesh rides has nothing to act on.
  const usedIds = $derived(new Set(app.meshVenues().map((v) => v.id)));

  // The inline editor's working copy — local state so typing isn't written back
  // (and reconnected) on every keystroke; the store only hears about it on Save.
  let label = $state("");
  let kind = $state<"static" | "remote">("static");
  let url = $state("");
  let signaling = $state<string[]>([]);
  let stun = $state<string[]>([]);
  let turn = $state<TurnEntry[]>([]);
  let editingId = $state<string | null>(null);
  let busy = $state(false);

  // Mirror the chosen draft into the editor fields whenever it changes (opening
  // a fresh "New venue", or an existing one for editing). A null draft closes
  // the editor; we don't clobber it here on every render — only on identity
  // change — so in-progress edits survive background updates.
  let syncedId = $state<symbol | null>(null);
  const draftKey = $derived(draft ? Symbol.for(draft.id) : null);
  $effect(() => {
    if (draftKey === syncedId) return;
    syncedId = draftKey;
    if (!draft) return;
    label = draft.label;
    kind = draft.url ? "remote" : "static";
    url = draft.url ?? "";
    signaling = [...draft.signaling];
    stun = [...draft.stun];
    turn = draft.turn.map((t) => ({ ...t }));
    editingId = app.venueById(draft.id) ? draft.id : null;
  });

  /** A one-line summary of where a venue points + how much it carries. */
  function summarise(v: Venue): string {
    if (v.builtin) return "built-in · MyOwnMesh reference servers";
    if (v.url) {
      let host = v.url;
      try {
        host = new URL(v.url).host || v.url;
      } catch {
        /* keep the raw url */
      }
      const when = v.fetchedAt ? ` · fetched ${relTime(v.fetchedAt)}` : " · not fetched yet";
      return `remote · from ${host}${when}`;
    }
    const relays = v.signaling.filter((s) => s.trim()).length;
    const turns = v.turn.filter((t) => t.url.trim()).length;
    const parts: string[] = [`${relays} relay${relays === 1 ? "" : "s"}`];
    if (turns) parts.push(`${turns} TURN`);
    return `static · ${parts.join(" · ")}`;
  }

  function relTime(ms: number): string {
    const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
    if (s < 60) return "just now";
    const m = Math.round(s / 60);
    if (m < 60) return `${m}m ago`;
    const h = Math.round(m / 60);
    if (h < 24) return `${h}h ago`;
    return `${Math.round(h / 24)}d ago`;
  }

  function newVenue() {
    app.venueDraft = { id: newVenueId(), label: "", signaling: [], stun: [], turn: [] };
  }
  function edit(v: Venue) {
    app.venueDraft = v;
  }
  function cancel() {
    app.venueDraft = null;
  }

  async function save() {
    const name = label.trim();
    if (!name) {
      app.toast("warn", "Give the venue a name");
      return;
    }
    if (kind === "remote" && !url.trim()) {
      app.toast("warn", "A remote venue needs a URL to fetch from");
      return;
    }
    busy = true;
    try {
      const id = editingId ?? newVenueId();
      const v: Venue =
        kind === "remote"
          ? // A remote venue keeps whatever it last fetched (so the mesh stays
            // up until the next refresh); Fetch now / refresh repopulates these.
            {
              id,
              label: name,
              url: url.trim(),
              signaling: editingId ? signaling : [],
              stun: editingId ? stun : [],
              turn: editingId ? turn : [],
            }
          : { id, label: name, signaling, stun, turn };
      await app.saveVenue(v);
      app.venueDraft = null;
    } finally {
      busy = false;
    }
  }

  // "Fetch now" on a remote venue: persist it first (so it exists to refresh),
  // then pull its servers. Saving here also closes the editor — refreshing a
  // live venue is the point of the button.
  async function saveAndFetch() {
    const name = label.trim();
    if (!name) {
      app.toast("warn", "Give the venue a name");
      return;
    }
    if (!url.trim()) {
      app.toast("warn", "Enter a URL to fetch from");
      return;
    }
    busy = true;
    try {
      const id = editingId ?? newVenueId();
      await app.saveVenue({
        id,
        label: name,
        url: url.trim(),
        signaling: editingId ? signaling : [],
        stun: editingId ? stun : [],
        turn: editingId ? turn : [],
      });
      await app.refreshVenue(id);
      app.venueDraft = null;
    } finally {
      busy = false;
    }
  }

  async function exportOne(v: Venue) {
    const safe = (v.label || "venue").replace(/[^a-z0-9._-]+/gi, "-").replace(/^-+|-+$/g, "") || "venue";
    await exportNetworkFile(`${safe}.venue.json`, exportVenue(v));
  }

  // Import mirrors NetworksSection's file flow: a hidden <input>, read its text,
  // parse the venue envelope, build a fresh local venue, save it, and (for a
  // remote one) pull its servers straight away.
  let importInput = $state<HTMLInputElement | null>(null);
  function onImportFile(e: Event) {
    const input = e.currentTarget as HTMLInputElement;
    const file = input.files?.[0] ?? null;
    input.value = ""; // re-picking the same file still fires onchange
    if (!file) return;
    file
      .text()
      .then(async (text) => {
        const env = tryParseVenue(text);
        if (!env) {
          app.toast("warn", "That isn't a venue file");
          return;
        }
        const v = venueFromExport(env);
        await app.saveVenue(v);
        if (v.url) await app.refreshVenue(v.id);
        // No toast — the imported venue appears as a row in the Venues list.
      })
      .catch((err) => app.toast("warn", `Couldn't read that file: ${String(err)}`));
  }
</script>

<div class="section">
  <div class="head">
    <h3>Venues</h3>
    <div class="seg">
      <button class="btn small" onclick={newVenue}>＋ New venue</button>
      <button class="btn small" title="Add a venue from a file someone exported" onclick={() => importInput?.click()}>↧ Import</button>
    </div>
  </div>
  <input bind:this={importInput} type="file" accept=".json,application/json" hidden onchange={onImportFile} />
  <p class="explain">A venue is where a mesh calls out — its signaling, STUN and TURN. A mesh's servers are its venues combined.</p>

  <ul class="list">
    {#each venues as v (v.id)}
      <li class:built={v.builtin}>
        <div class="v-main">
          <div class="v-name">
            {v.label}
            {#if v.builtin}<span class="chip built-chip">built-in</span>{/if}
            {#if usedIds.has(v.id) && !app.isVenueActive(v.id)}<span class="chip off-chip">off</span>{/if}
          </div>
          <div class="v-sub">{summarise(v)}</div>
        </div>
        {#if usedIds.has(v.id)}
          {@const on = app.isVenueActive(v.id)}
          <button
            class="vswitch"
            class:on
            role="switch"
            aria-checked={on}
            aria-label="{on ? 'Switch off' : 'Switch on'} {v.label}"
            title={on
              ? "On — this venue's servers are folded into every mesh that uses it. The same switch as the venues pill."
              : "Off — switch on to fold this venue's servers back in"}
            onclick={() => void app.toggleVenue(v.id, !on)}
          >
            <span class="vknob"></span>
          </button>
        {/if}
        {#if v.builtin}
          <button class="btn small" onclick={() => edit(v)}>View</button>
        {:else}
          <button class="btn small" onclick={() => edit(v)}>Edit</button>
          {#if canExport}
            <button class="btn small" title="Save this venue to a file to share" onclick={() => exportOne(v)}>Export</button>
          {/if}
          <button class="btn small danger" onclick={() => app.deleteVenue(v.id)}>Delete</button>
        {/if}
      </li>
    {/each}
  </ul>

  {#if draft}
    {@const ro = !!draft.builtin}
    <section class="editor">
      <h4>{ro ? "Venue" : editingId ? "Edit venue" : "New venue"}</h4>

      <label class="lbl" for="venue-label">Name</label>
      <input id="venue-label" class="field" placeholder="e.g. Office relays" bind:value={label} disabled={ro} />

      {#if !ro}
        <div class="toggle" role="group" aria-label="Venue kind">
          <button class="tog" class:on={kind === "static"} onclick={() => (kind = "static")}>Static</button>
          <button class="tog" class:on={kind === "remote"} onclick={() => (kind = "remote")}>Remote (fetched from a URL)</button>
        </div>
      {/if}

      {#if kind === "remote"}
        <p class="note">A remote venue's servers live at a URL — whoever hosts it can update them without anyone re-importing. The host must be online for the venue to work.</p>
        <label class="lbl" for="venue-url">URL</label>
        <div class="row">
          <input id="venue-url" class="field mono" placeholder="https://host/my.venue.json" bind:value={url} disabled={ro} />
          {#if !ro}
            <button class="btn small primary" disabled={busy} onclick={saveAndFetch}>{busy ? "Fetching…" : "Fetch now"}</button>
          {/if}
        </div>
      {:else}
        <!-- Static: the same add/remove URL-list editor the per-network Servers
             pane uses, so signaling / STUN / TURN feel identical everywhere. -->
        <section class="grp">
          <div class="grp-head">
            <h5>Signaling relays</h5>
            {#if !ro}<button class="btn small" onclick={() => (signaling = [...signaling, ""])}>＋ Add</button>{/if}
          </div>
          {#each signaling as _, i}
            <div class="row">
              <input class="field mono" placeholder="wss://…" bind:value={signaling[i]} disabled={ro} />
              {#if !ro}<button class="x" title="Remove" onclick={() => (signaling = signaling.filter((_, j) => j !== i))}>✕</button>{/if}
            </div>
          {/each}
          {#if signaling.length === 0}<p class="empty">None — peers fall back to the built-in public relays (less reliable).</p>{/if}
        </section>

        <section class="grp">
          <div class="grp-head">
            <h5>STUN servers</h5>
            {#if !ro}<button class="btn small" onclick={() => (stun = [...stun, ""])}>＋ Add</button>{/if}
          </div>
          {#each stun as _, i}
            <div class="row">
              <input class="field mono" placeholder="stun:host:3478" bind:value={stun[i]} disabled={ro} />
              {#if !ro}<button class="x" title="Remove" onclick={() => (stun = stun.filter((_, j) => j !== i))}>✕</button>{/if}
            </div>
          {/each}
          {#if stun.length === 0}<p class="empty">None.</p>{/if}
        </section>

        <section class="grp">
          <div class="grp-head">
            <h5>TURN servers</h5>
            {#if !ro}<button class="btn small" onclick={() => (turn = [...turn, { url: "", username: "", credential: "" }])}>＋ Add</button>{/if}
          </div>
          {#each turn as _, i}
            <div class="turn">
              <div class="row">
                <input class="field mono" placeholder="turn:host:3478" bind:value={turn[i].url} disabled={ro} />
                {#if !ro}<button class="x" title="Remove" onclick={() => (turn = turn.filter((_, j) => j !== i))}>✕</button>{/if}
              </div>
              <div class="row creds">
                <input class="field" placeholder="username" bind:value={turn[i].username} disabled={ro} />
                <input class="field" placeholder="credential" bind:value={turn[i].credential} disabled={ro} />
              </div>
            </div>
          {/each}
          {#if turn.length === 0}<p class="empty">None — symmetric-NAT / CGNAT peers may fail to connect.</p>{/if}
        </section>
      {/if}

      <div class="actions">
        <button class="btn small ghost" onclick={cancel}>{ro ? "Close" : "Cancel"}</button>
        {#if !ro}
          <button class="btn small primary" disabled={busy} onclick={save}>{busy ? "Saving…" : "Save venue"}</button>
        {/if}
      </div>
    </section>
  {/if}
</div>

<style>
  .section {
    display: flex;
    flex-direction: column;
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    flex-wrap: wrap;
    gap: 0.5rem;
    margin-bottom: 0.3rem;
  }
  h3 {
    margin: 0;
    font-size: 1.2rem;
  }
  .seg {
    display: flex;
    gap: 0.3rem;
  }
  .explain {
    font-size: 0.78rem;
    color: var(--ink-soft);
    margin: 0 0 0.6rem;
    line-height: 1.45;
  }
  .list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
  }
  .list li {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    background: var(--surface-2);
    border-radius: var(--r-sm);
    padding: 0.45rem 0.55rem;
  }
  .list li.built {
    box-shadow: 0 0 0 1.5px var(--c-venue);
  }
  .v-main {
    flex: 1;
    min-width: 0;
  }
  .v-name {
    font-size: 0.86rem;
    font-weight: 600;
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }
  .v-sub {
    font-size: 0.72rem;
    color: var(--ink-faint);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .built-chip {
    color: var(--c-venue-ink);
    background: var(--c-venue-soft);
    border-color: var(--c-venue);
  }
  .off-chip {
    color: var(--ink-faint);
    background: var(--surface);
    border: 1px solid var(--line-strong);
  }
  /* The shared on/off switch — the same control as the venues pill, so the
     library and the pill drive one off-list. */
  .vswitch {
    position: relative;
    width: 2.1rem;
    height: 1.15rem;
    border-radius: var(--r-pill);
    border: 1px solid var(--line-strong);
    background: var(--surface);
    padding: 0;
    flex-shrink: 0;
    transition: background 0.12s ease, border-color 0.12s ease;
  }
  .vswitch .vknob {
    position: absolute;
    top: 1px;
    left: 1px;
    width: 0.95rem;
    height: 0.95rem;
    border-radius: 50%;
    background: var(--ink-faint);
    transition: transform 0.12s ease, background 0.12s ease;
  }
  /* On = the venue concept colour (gold), buttony like the venues pill. */
  .vswitch.on {
    background: linear-gradient(
      180deg,
      color-mix(in oklch, var(--c-venue) 80%, white),
      var(--c-venue)
    );
    border-color: var(--c-venue);
    box-shadow: inset 0 1px 0 oklch(1 0 0 / 0.35),
      0 2px 8px -3px var(--c-venue-soft);
  }
  .vswitch.on .vknob {
    transform: translateX(0.92rem);
    background: linear-gradient(180deg, #fff, oklch(0.95 0.02 80));
  }
  .editor {
    margin-top: 1rem;
    border-top: 1px solid var(--line);
    padding-top: 0.9rem;
  }
  h4 {
    margin: 0 0 0.6rem;
    font-size: 0.95rem;
  }
  h5 {
    margin: 0;
    font-size: 0.84rem;
  }
  .lbl {
    display: block;
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--ink-faint);
    margin-bottom: 0.3rem;
  }
  .toggle {
    display: inline-flex;
    gap: 0.2rem;
    background: var(--surface-2);
    border-radius: var(--r-pill);
    padding: 0.2rem;
    margin: 0.7rem 0 0.5rem;
  }
  .tog {
    border: none;
    background: none;
    color: var(--ink-soft);
    font-size: 0.78rem;
    font-weight: 600;
    padding: 0.32rem 0.7rem;
    border-radius: var(--r-pill);
  }
  .tog.on {
    background: var(--surface);
    color: var(--accent-ink);
    box-shadow: var(--shadow-sm);
  }
  .note {
    font-size: 0.74rem;
    color: var(--ink-soft);
    line-height: 1.45;
    margin: 0.2rem 0 0.5rem;
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
  .field:disabled {
    opacity: 0.7;
    cursor: default;
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
  .btn.danger {
    color: var(--danger);
  }
</style>
