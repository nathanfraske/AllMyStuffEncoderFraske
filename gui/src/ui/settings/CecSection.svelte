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
  import { app } from "../../store.svelte";
  import { isTauri, type CecScope } from "../../tauri";

  const web = !isTauri();
  const status = $derived(app.cecStatusInfo);
  const requests = $derived(app.cecRequests);
  const grants = $derived(app.cecGrantList);

  // The customers this technician has dialed — the live CEC connections, read
  // from CEC state (`cec_dialed`). Each is an ordinary graph peer; there is no
  // "fleet group" to filter the graph by (the CEC mesh is Silent, no roster).
  const customers = $derived(app.cecCustomers);

  const scopeLabel: Record<CecScope, string> = {
    once: "Approve Once",
    three_hours: "Auto-Approve for 3 hours",
    forever: "Auto-Approve Forever",
  };

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

  <!-- Active CEC connections -->
  <section class="block">
    <div class="title">Active connections</div>
    {#if customers.length === 0}
      <p class="notice">No customers connected. Dial a number above to start.</p>
    {:else}
      <ul class="rows">
        {#each customers as c (c.node)}
          <li class="row">
            <span class="dot" class:on={c.online}></span>
            <span class="who">
              <b>{c.label || c.number || "Customer"}</b>
              <span class="sub">{c.online ? "online" : "offline"}</span>
            </span>
            <div class="row-actions">
              <button class="btn small" onclick={() => app.openConsole(c.node)}>Open screen</button>
              <button class="btn small danger" onclick={() => app.forgetNode(c.node)}>Disconnect</button>
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
