<script lang="ts">
  // CEC Support — the technician's help-desk tab. Secret: it only appears once
  // this install is in the CEC context (see SettingsPanel + App.cecRevealed).
  //
  // The technician fills in their Agent Name (the name a customer sees in the
  // "*so-and-so* is trying to connect" prompt), types the number the customer
  // read out, and connects — the customer then appears on the device graph as
  // an ordinary peer with the normal screen/control features, gated by the
  // customer approving this technician. The dialed customers are listed below
  // from CEC state (not a graph group — the CEC mesh is Silent, no roster). The
  // customer-side flow (answering the 3-choice prompt, the standing grant list)
  // is shown too, so a build that hosts can drive it from here.
  import { onMount } from "svelte";
  import { app, CEC_STALE_AFTER_S } from "../../store.svelte";
  import { isTauri, type CecScope } from "../../tauri";

  const web = !isTauri();
  const status = $derived(app.cecStatusInfo);
  const requests = $derived(app.cecRequests);
  const grants = $derived(app.cecGrantList);

  // The customers this technician has dialed — the live CEC connections, read
  // from CEC state (`cec_dialed`), most-recently-used first so active ones stay
  // on top and stale ones sink to where they're easy to prune. Each is an
  // ordinary graph peer; there is no "fleet group" to filter the graph by (the
  // CEC mesh is Silent, no roster).
  const customers = $derived(app.cecCustomersByRecent);

  // Inline rename: which customer (by node id) is being labelled, and the draft.
  let editingNode = $state<string | null>(null);
  let aliasDraft = $state("");

  function startRename(node: string, number: string) {
    editingNode = node;
    aliasDraft = app.cecAliases[number] ?? "";
  }
  function saveRename(number: string) {
    app.setCecAlias(number, aliasDraft);
    editingNode = null;
    aliasDraft = "";
  }
  function cancelRename() {
    editingNode = null;
    aliasDraft = "";
  }

  const scopeLabel: Record<CecScope, string> = {
    once: "Approve Once",
    three_hours: "Auto-Approve for 3 hours",
    forever: "Auto-Approve Forever",
  };

  /** The customer's number as their mesh reads it — `123 456 789`, matching the
   *  Silent room's label ("CEC Support …"). Falls back to the raw string if it
   *  isn't the expected 9 digits. */
  function groupNumber(n: string): string {
    const d = (n || "").replace(/\D/g, "");
    return d.length === 9 ? `${d.slice(0, 3)} ${d.slice(3, 6)} ${d.slice(6)}` : n || "—";
  }

  /** Seconds since a connection was last used (dialed, or its console session
   *  went active). `last_used` is epoch seconds from the node. */
  function idleSeconds(lastUsed: number): number {
    return Math.max(0, Math.round(Date.now() / 1000 - (lastUsed || 0)));
  }

  /** "used just now" / "used 12m ago" / "used 3d ago" — the time-since-last-used
   *  metric so a technician can tell active connections from stale ones. */
  function lastUsedLabel(lastUsed: number): string {
    if (!lastUsed) return "used recently";
    const s = idleSeconds(lastUsed);
    if (s < 45) return "used just now";
    const m = Math.round(s / 60);
    if (m < 60) return `used ${m}m ago`;
    const h = Math.round(m / 60);
    if (h < 24) return `used ${h}h ago`;
    const d = Math.round(h / 24);
    return `used ${d}d ago`;
  }

  /** Whether a connection has gone stale (unused past the threshold) — surfaced
   *  as a badge so the cleanup candidates stand out. */
  function isStale(lastUsed: number): boolean {
    return !!lastUsed && idleSeconds(lastUsed) > CEC_STALE_AFTER_S;
  }

  function connect(e: SubmitEvent) {
    e.preventDefault();
    void app.dialCec();
  }

  onMount(() => {
    void app.loadCec();
  });
</script>

<div class="section">
  <h3>CEC Support</h3>
  <p class="lead">
    Remote help, one number at a time. Enter your agent name and the number the
    customer read out — they appear on your device graph and in the list below,
    and you can view or control their screen once they approve.
  </p>

  {#if web}
    <section class="block">
      <p class="notice">These controls live in the desktop app — this is the in-browser preview.</p>
    </section>
  {/if}

  <!-- Agent name — the identity the customer sees -->
  <section class="block">
    <div class="title">Agent name</div>
    <p class="hint">
      This is the name the customer sees in "<i>{app.cecAgentName || "so-and-so"}</i> is trying to
      connect to your computer." Use something they'll recognise as you or CEC.
    </p>
    <input
      class="field"
      type="text"
      placeholder="e.g. Alex at CEC"
      value={app.cecAgentName}
      oninput={(e) => app.setCecAgentName(e.currentTarget.value)}
    />
  </section>

  <!-- Connect to a customer -->
  <section class="block">
    <div class="title">Connect to a customer</div>
    <p class="hint">Type the number the customer read out (e.g. <code>123 456 789</code>).</p>
    <form class="dial" onsubmit={connect}>
      <input
        class="field mono"
        type="text"
        placeholder="Customer number"
        autocomplete="off"
        spellcheck="false"
        bind:value={app.cecNumberDraft}
      />
      <button
        class="btn primary"
        type="submit"
        disabled={app.cecDialing || !app.cecNumberDraft.trim() || !app.cecAgentName.trim()}
      >
        {app.cecDialing ? "Connecting…" : "Connect"}
      </button>
    </form>
  </section>

  <!-- Client meshes — the customers this technician has dialed. Each is the
       customer's own private Silent mesh, kept here (and out of the Meshes tab)
       so client connections are managed apart from your own. Sorted most-recent
       first; a "stale" badge flags connections gone unused, and each can be
       given a private label you'll recognise. -->
  <section class="block">
    <div class="head">
      <div class="title">Client meshes</div>
      {#if app.cecStaleCount > 0}
        <button class="btn small danger" onclick={() => void app.removeStaleCec()}>
          Remove {app.cecStaleCount} stale
        </button>
      {/if}
    </div>
    {#if app.cecDialingNumber}
      <!-- The in-flight dial, visible from the first click: discovery alone can
           take up to ~45s, and an invisible wait reads as "nothing happened". -->
      <div class="row pending-row">
        <span class="dot busy"></span>
        <span class="who">
          <b>Dialing {groupNumber(app.cecDialingNumber)}…</b>
          <span class="sub">Finding the customer on their support mesh — this can take a moment.</span>
        </span>
      </div>
    {/if}
    {#if customers.length === 0}
      {#if !app.cecDialingNumber}
        <p class="notice">
          No machines yet. Dial a number above — the machines you connect to stay
          here so you can reconnect with one tap.
        </p>
      {/if}
    {:else}
      <p class="hint">
        Every machine you've connected to stays here, most recently used first.
        <b>Connect</b> reopens a session — the customer re-approves only if their
        access has lapsed. Rename one to something you'll recognise, and remove
        the ones that have cycled out.
      </p>
      <ul class="rows">
        {#each customers as c (c.node)}
          <li class="row col" class:stale={isStale(c.last_used)}>
            <div class="row-top">
              <span class="dot" class:on={c.online}></span>
              <span class="who">
                {#if editingNode === c.node}
                  <!-- svelte-ignore a11y_autofocus -->
                  <input
                    class="field rename"
                    type="text"
                    autofocus
                    placeholder={c.label || "Customer name"}
                    bind:value={aliasDraft}
                    onkeydown={(e) => {
                      if (e.key === "Enter") saveRename(c.number);
                      else if (e.key === "Escape") cancelRename();
                    }}
                  />
                {:else}
                  <b>{app.cecCustomerName(c)}</b>
                  <span class="sub">
                    <span class="mesh" title={`Mesh cec-${c.number}`}>CEC Support {groupNumber(c.number)}</span>
                    <span class="meta">
                      · {c.online ? "online" : "offline"} · {lastUsedLabel(c.last_used)}
                    </span>
                    {#if c.node === app.cecAutoOpenNode}
                      <span class="pending-tag">waiting for approval</span>
                    {/if}
                    {#if isStale(c.last_used)}<span class="stale-tag">stale</span>{/if}
                  </span>
                {/if}
              </span>
            </div>
            <div class="row-actions">
              {#if editingNode === c.node}
                <button class="btn small primary" onclick={() => saveRename(c.number)}>Save</button>
                <button class="btn small" onclick={cancelRename}>Cancel</button>
              {:else}
                <button
                  class="btn small primary"
                  disabled={app.cecDialing}
                  onclick={() => void app.reconnectCec(c.number)}
                >
                  Connect
                </button>
                <button class="btn small" onclick={() => startRename(c.node, c.number)}>Rename</button>
                <button class="btn small danger" onclick={() => app.forgetNode(c.node)}>Remove</button>
              {/if}
            </div>
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  <!-- Customer side: inbound requests + standing grants. Shown when this build
       is hosting (a customer answering the prompt from the same engine). -->
  {#if status?.hosting || requests.length > 0 || grants.length > 0}
    <section class="block">
      <div class="title">You are hosting</div>
      {#if status?.number}
        <p class="hint">
          Your number is <code>{status.number}</code> — read it to the technician so they can
          connect.
        </p>
      {/if}

      {#if requests.length > 0}
        <div class="subtitle">Trying to connect</div>
        <ul class="rows">
          {#each requests as r (r.session_id)}
            <li class="row col">
              <div class="who">
                <b>{r.agent_name || "A technician"}</b>
                <span class="sub">
                  {r.want_control ? "wants to view + control" : "wants to view"} · code
                  <code>{r.verification_code}</code>
                </span>
              </div>
              <div class="choices">
                <button class="btn small" onclick={() => app.approveCecRequest(r, "once")}>
                  {scopeLabel.once}
                </button>
                <button class="btn small" onclick={() => app.approveCecRequest(r, "three_hours")}>
                  {scopeLabel.three_hours}
                </button>
                <button class="btn small primary" onclick={() => app.approveCecRequest(r, "forever")}>
                  {scopeLabel.forever}
                </button>
                <button class="btn small danger" onclick={() => app.denyCecRequest(r)}>Decline</button>
              </div>
            </li>
          {/each}
        </ul>
      {/if}

      {#if grants.length > 0}
        <div class="subtitle">Who can reach you</div>
        <ul class="rows">
          {#each grants as g (g.technician)}
            <li class="row">
              <span class="who">
                <b>{g.agent_name || g.technician.slice(0, 10)}</b>
                <span class="sub">
                  {scopeLabel[g.scope]} · {g.control ? "view + control" : "view only"}
                </span>
              </span>
              <div class="row-actions">
                <button class="btn small danger" onclick={() => app.revokeCecTech(g.technician, g.agent_name)}>
                  Forget
                </button>
              </div>
            </li>
          {/each}
        </ul>
      {/if}
    </section>
  {/if}
</div>

<style>
  .section {
    display: flex;
    flex-direction: column;
  }
  h3 {
    margin: 0 0 0.3rem;
    font-size: 1.1rem;
  }
  .lead {
    margin: 0 0 1rem;
    font-size: 0.85rem;
    color: var(--ink-soft);
    line-height: 1.5;
  }
  .block {
    background: var(--surface-2);
    border-radius: var(--r-md, 0.6rem);
    padding: 0.9rem 1rem;
    margin-bottom: 0.9rem;
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.6rem;
    margin-bottom: 0.5rem;
  }
  .head .title {
    margin-bottom: 0;
  }
  .title {
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    font-weight: 700;
    color: var(--ink-faint);
    margin-bottom: 0.5rem;
  }
  .subtitle {
    font-size: 0.75rem;
    font-weight: 700;
    color: var(--ink-soft);
    margin: 0.9rem 0 0.4rem;
  }
  .hint {
    font-size: 0.8rem;
    color: var(--ink-soft);
    margin: 0 0 0.55rem;
    line-height: 1.45;
  }
  .field {
    width: 100%;
    box-sizing: border-box;
    background: var(--surface);
    border: 1px solid var(--line);
    border-radius: var(--r-sm);
    color: var(--ink);
    font-size: 0.9rem;
    padding: 0.5rem 0.6rem;
  }
  .field:focus {
    outline: none;
    border-color: var(--accent);
  }
  .mono {
    font-family: var(--mono);
    letter-spacing: 0.04em;
  }
  .dial {
    display: flex;
    gap: 0.5rem;
    align-items: stretch;
  }
  .dial .field {
    flex: 1;
  }
  .btn {
    border: 1px solid var(--line-strong, var(--line));
    background: var(--surface);
    color: var(--ink);
    border-radius: var(--r-sm);
    padding: 0.5rem 0.9rem;
    font-size: 0.85rem;
    font-weight: 600;
    cursor: pointer;
    white-space: nowrap;
  }
  .btn:hover:not(:disabled) {
    border-color: var(--accent);
  }
  .btn:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .btn.small {
    padding: 0.32rem 0.6rem;
    font-size: 0.78rem;
  }
  .btn.primary {
    background: var(--accent);
    border-color: var(--accent);
    color: var(--accent-contrast, #fff);
  }
  .btn.danger {
    color: var(--danger);
    border-color: var(--danger);
    background: transparent;
  }
  .btn.danger:hover:not(:disabled) {
    background: var(--danger);
    color: #fff;
  }
  .rows {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    background: var(--surface);
    border-radius: var(--r-sm);
    padding: 0.5rem 0.6rem;
  }
  .row.col {
    flex-direction: column;
    align-items: stretch;
    gap: 0.5rem;
  }
  .who {
    display: flex;
    flex-direction: column;
    min-width: 0;
    flex: 1;
  }
  .who .sub {
    font-size: 0.75rem;
    color: var(--ink-soft);
    line-height: 1.5;
  }
  .row-top {
    display: flex;
    align-items: center;
    gap: 0.6rem;
  }
  .who .mesh {
    font-weight: 600;
    color: var(--ink-soft);
  }
  .who .meta {
    color: var(--ink-faint);
  }
  .field.rename {
    padding: 0.32rem 0.5rem;
    font-size: 0.85rem;
  }
  /* The in-flight dial row + waiting-for-approval badge — a connect attempt is
     visible from the first click through to the customer's decision. */
  .pending-row {
    border: 1px dashed var(--accent);
  }
  .dot.busy {
    background: var(--accent);
  }
  .pending-tag {
    display: inline-block;
    margin-left: 0.35rem;
    font-size: 0.6rem;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--accent);
    border: 1px solid var(--accent);
    border-radius: var(--r-pill, 999px);
    padding: 0.02rem 0.4rem;
    vertical-align: middle;
  }

  /* The stale marker — a connection unused past the threshold, the cleanup
     candidate. The row gets a dashed danger outline and a small badge. */
  .stale-tag {
    display: inline-block;
    margin-left: 0.35rem;
    font-size: 0.6rem;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--danger);
    border: 1px solid var(--danger);
    border-radius: var(--r-pill, 999px);
    padding: 0.02rem 0.4rem;
    vertical-align: middle;
  }
  .row.stale {
    border: 1px dashed color-mix(in oklab, var(--danger) 45%, transparent);
  }
  .row-actions,
  .choices {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
  }
  .dot {
    width: 0.55rem;
    height: 0.55rem;
    border-radius: 50%;
    background: var(--ink-faint);
    flex-shrink: 0;
  }
  .dot.on {
    background: var(--ok, #35c26a);
  }
  .notice {
    font-size: 0.82rem;
    color: var(--ink-soft);
    margin: 0;
    line-height: 1.45;
  }
  code {
    font-family: var(--mono);
    font-size: 0.8rem;
    background: var(--surface);
    padding: 0.05rem 0.3rem;
    border-radius: var(--r-sm);
  }
</style>
