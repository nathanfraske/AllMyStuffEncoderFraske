<script lang="ts">
  // Share Flow — the side-by-side builder for a device share. The Sender (right)
  // makes things available; the Receiver (left) gets them. It composes the same
  // capabilities the rest of the app uses — each toggle resolves through the
  // store's startShareFlow, which rides the ordinary connect()/grant path; it
  // doesn't invent any new wiring. Opened from "New Share" in the Sharing pane,
  // or by dragging one device onto another on the graph.
  import { app, type ShareCap } from "../store.svelte";
  import { displayName, isAppNode } from "../types";

  type Side = "receiver" | "sender";

  // Which picker dropdown is open, if any.
  let picking = $state<Side | null>(null);
  // The capabilities switched on for this share.
  let chosen = $state<Set<ShareCap>>(new Set());

  const CAPS: { key: ShareCap; label: string; icon: string; popout?: boolean }[] = [
    { key: "audio", label: "Audio", icon: "🔊" },
    { key: "video", label: "Video devices", icon: "🎥" },
    { key: "screens", label: "Screens", icon: "🖥" },
    { key: "files", label: "Files", icon: "🗂", popout: true },
    { key: "terminal", label: "Terminal", icon: "📟" },
    { key: "sites", label: "Sites", icon: "🌐", popout: true },
  ];

  const sender = $derived(app.shareFlowSender);
  const receiver = $derived(app.shareFlowReceiver);

  // Candidate devices for either side: real app machines, the opposite side
  // excluded so you can't pick the same device twice.
  function candidates(exclude: string | null) {
    return app.catalog.nodes.filter((n) => isAppNode(n) && n.id !== exclude);
  }
  function nodeOf(id: string | null) {
    return id ? app.node(id) : null;
  }
  function osOf(id: string | null): string {
    return nodeOf(id)?.summary?.os ?? "";
  }
  function pick(side: Side, id: string) {
    if (side === "sender") app.shareFlowSender = id;
    else app.shareFlowReceiver = id;
    picking = null;
    // A capability the new sender can't offer can't stay selected.
    if (side === "sender") {
      chosen = new Set([...chosen].filter((c) => app.shareFlowCapAvailable(id, c)));
    }
  }
  function toggleCap(c: ShareCap) {
    if (!app.shareFlowCapAvailable(sender, c)) return;
    const next = new Set(chosen);
    if (next.has(c)) next.delete(c);
    else next.add(c);
    chosen = next;
  }

  const selectedLabels = $derived(CAPS.filter((c) => chosen.has(c.key)).map((c) => c.label));
  const canStart = $derived(!!sender && !!receiver && sender !== receiver && chosen.size > 0);

  function start() {
    const n = app.startShareFlow([...chosen]);
    if (n > 0) app.closeShareFlow();
  }
</script>

<svelte:window
  onkeydown={(e) => {
    if (!app.shareFlowOpen || e.key !== "Escape") return;
    if (picking) picking = null;
    else app.closeShareFlow();
  }}
/>

{#if app.shareFlowOpen}
  <div class="scrim">
    <button class="backdrop" aria-label="Close" onclick={() => app.closeShareFlow()}></button>
    <div class="sheet" role="dialog" aria-modal="true" aria-label="Share Flow" tabindex="-1">
      <header class="head">
        <div class="head-text">
          <div class="title"><span class="t-icon">🔗</span> Share Flow</div>
          <div class="sub">Build and start a device share</div>
        </div>
        <button class="x" onclick={() => app.closeShareFlow()} aria-label="Close">✕</button>
      </header>

      <div class="body">
        <!-- Receiver (left) -->
        <section class="col receiver">
          <div class="col-kicker">Receiver</div>
          <div class="col-sub">Device receiving the share</div>
          {@render picker("receiver", receiver)}
          <p class="note">
            ⓘ The receiver can access whatever the sender makes available. Both
            parties can revoke at any time.
          </p>
        </section>

        <!-- Middle: direction + actions -->
        <div class="mid">
          <div class="arrows" aria-hidden="true">▼▼▼</div>
          <button class="btn primary start" disabled={!canStart} onclick={start}>🔗 Start Share</button>
          <button class="btn ghost stop" onclick={() => app.stopShareFlow()}>⤓ Stop Share</button>
          <div class="dir">SENDER → RECEIVER</div>
        </div>

        <!-- Sender (right) -->
        <section class="col sender">
          <div class="col-kicker">Sender</div>
          <div class="col-sub">Device sharing its stuff</div>
          {@render picker("sender", sender)}

          <div class="sharing-line">
            <span class="sl-k">Sharing</span>
            <span class="sl-v" class:none={selectedLabels.length === 0}>
              {selectedLabels.length === 0 ? "No capabilities selected" : selectedLabels.join(", ")}
            </span>
          </div>

          <div class="what">What to share</div>
          <div class="caps">
            {#each CAPS as c (c.key)}
              {@const avail = app.shareFlowCapAvailable(sender, c.key)}
              <button
                class="cap"
                class:on={chosen.has(c.key)}
                disabled={!avail}
                title={avail ? (c.popout ? `${c.label} (opens a popout)` : c.label) : `${c.label} — not offered by this device`}
                onclick={() => toggleCap(c.key)}
              >
                <span class="cap-i" aria-hidden="true">{c.icon}</span>
                {c.label}{#if c.popout}<span class="cap-pop">popout</span>{/if}
              </button>
            {/each}
          </div>
        </section>
      </div>
    </div>
  </div>
{/if}

<!-- A device picker card: the chosen device, or a "+" to pick one. Clicking
     opens a dropdown of the candidate machines. -->
{#snippet picker(side: Side, id: string | null)}
  {@const n = nodeOf(id)}
  <div class="pick-wrap">
    <button class="pick" class:filled={!!n} onclick={() => (picking = picking === side ? null : side)}>
      {#if n}
        <span class="pick-icon" aria-hidden="true">{n.kind === "this" ? "💻" : "🖥"}<span class="pick-dot" class:on={n.online}></span></span>
        <span class="pick-name">{displayName(n)}</span>
        <span class="pick-os">{osOf(id) || "device"}</span>
      {:else}
        <span class="pick-plus" aria-hidden="true">＋</span>
        <span class="pick-name muted">Pick a device</span>
      {/if}
    </button>
    {#if picking === side}
      <div class="menu" role="listbox">
        {#each candidates(side === "sender" ? receiver : sender) as cand (cand.id)}
          <button class="menu-item" role="option" aria-selected={cand.id === id} onclick={() => pick(side, cand.id)}>
            <span class="mi-icon" aria-hidden="true">{cand.kind === "this" ? "💻" : "🖥"}</span>
            <span class="mi-name">{displayName(cand)}</span>
            <span class="mi-dot" class:on={cand.online}></span>
          </button>
        {:else}
          <div class="menu-empty">No other devices to pick</div>
        {/each}
      </div>
    {/if}
  </div>
{/snippet}

<style>
  .backdrop {
    position: absolute;
    inset: 0;
    border: none;
    background: transparent;
    cursor: default;
  }
  .sheet {
    position: relative;
    z-index: 1;
    width: min(1040px, 95vw);
    max-height: 92vh;
    display: flex;
    flex-direction: column;
    background: var(--surface);
    border: 1px solid var(--line);
    border-radius: var(--r-lg);
    box-shadow: var(--shadow-lg);
    overflow: hidden;
    animation: rise 0.16s ease;
  }
  @keyframes rise {
    from {
      transform: translateY(12px) scale(0.98);
      opacity: 0;
    }
  }
  .head {
    display: flex;
    align-items: center;
    gap: 0.7rem;
    padding: 0.9rem 1.2rem;
    border-bottom: 1px solid var(--line);
  }
  .head-text {
    flex: 1;
    min-width: 0;
  }
  .title {
    font-weight: 750;
    font-size: 1.05rem;
    color: var(--c-share-ink);
  }
  .t-icon {
    filter: grayscale(0.1);
  }
  .sub {
    font-size: 0.78rem;
    color: var(--ink-faint);
    margin-top: 0.1rem;
  }
  .x {
    border: none;
    background: var(--surface-2);
    color: var(--ink-soft);
    width: 1.9rem;
    height: 1.9rem;
    border-radius: 50%;
    flex-shrink: 0;
  }
  .x:hover {
    background: var(--line-strong);
  }
  .body {
    display: grid;
    grid-template-columns: 1fr auto 1fr;
    gap: 1.2rem;
    padding: 1.3rem 1.4rem 1.6rem;
    overflow: auto;
  }
  .col-kicker {
    font-size: 0.72rem;
    font-weight: 800;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--c-share-ink);
  }
  .col-sub {
    font-size: 0.78rem;
    color: var(--ink-faint);
    margin: 0.1rem 0 0.7rem;
  }
  .note {
    font-size: 0.74rem;
    color: var(--ink-faint);
    line-height: 1.5;
    margin: 0.8rem 0 0;
    border: 1px solid var(--line);
    border-radius: var(--r-sm);
    padding: 0.55rem 0.7rem;
    background: var(--surface-2);
  }

  /* ---- picker ---- */
  .pick-wrap {
    position: relative;
  }
  .pick {
    width: 100%;
    min-height: 7rem;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 0.35rem;
    border: 1.5px dashed var(--line-strong);
    border-radius: var(--r-md);
    background: var(--surface-2);
    color: var(--ink);
    padding: 1rem;
    transition: border-color 0.12s ease, background 0.12s ease;
  }
  .pick:hover {
    border-color: var(--c-share);
    background: var(--surface);
  }
  .pick.filled {
    border-style: solid;
  }
  .pick-icon {
    position: relative;
    font-size: 1.8rem;
    line-height: 1;
  }
  .pick-plus {
    font-size: 1.8rem;
    color: var(--ink-faint);
  }
  .pick-dot {
    position: absolute;
    right: -2px;
    bottom: 0;
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--line-strong);
    border: 2px solid var(--surface);
  }
  .pick-dot.on {
    background: var(--ok);
  }
  .pick-name {
    font-weight: 700;
    font-size: 0.95rem;
  }
  .pick-name.muted {
    color: var(--ink-faint);
    font-weight: 600;
  }
  .pick-os {
    font-size: 0.74rem;
    color: var(--ink-faint);
  }
  .menu {
    position: absolute;
    top: calc(100% + 0.3rem);
    left: 0;
    right: 0;
    z-index: 3;
    background: var(--surface);
    border: 1px solid var(--line-strong);
    border-radius: var(--r-md);
    box-shadow: var(--shadow-lg);
    padding: 0.3rem;
    max-height: 16rem;
    overflow: auto;
  }
  .menu-item {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    width: 100%;
    border: none;
    background: none;
    color: var(--ink);
    padding: 0.45rem 0.5rem;
    border-radius: var(--r-sm);
    text-align: left;
    font-size: 0.86rem;
  }
  .menu-item:hover {
    background: var(--surface-2);
  }
  .mi-name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .mi-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--line-strong);
    flex-shrink: 0;
  }
  .mi-dot.on {
    background: var(--ok);
  }
  .menu-empty {
    font-size: 0.8rem;
    color: var(--ink-faint);
    padding: 0.5rem;
  }

  /* ---- middle ---- */
  .mid {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 0.6rem;
    align-self: center;
  }
  .arrows {
    color: var(--c-share);
    font-size: 0.7rem;
    letter-spacing: 0.1em;
    opacity: 0.7;
  }
  .start {
    background: linear-gradient(180deg, var(--c-share-ink), var(--c-share));
    border-color: var(--c-share);
    color: #fff;
    box-shadow: var(--shadow-sm), 0 4px 12px -4px var(--c-share),
      inset 0 1px 0 oklch(1 0 0 / 0.25);
    white-space: nowrap;
  }
  .start:disabled {
    filter: grayscale(0.5);
  }
  .stop {
    white-space: nowrap;
  }
  .dir {
    font-size: 0.66rem;
    font-weight: 700;
    letter-spacing: 0.05em;
    color: var(--ink-faint);
  }

  /* ---- sender capabilities ---- */
  .sharing-line {
    display: flex;
    gap: 0.5rem;
    align-items: baseline;
    margin-top: 0.9rem;
  }
  .sl-k {
    font-size: 0.7rem;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--ink-faint);
  }
  .sl-v {
    font-size: 0.82rem;
    font-weight: 600;
    color: var(--c-share-ink);
  }
  .sl-v.none {
    color: var(--ink-faint);
    font-style: italic;
    font-weight: 500;
  }
  .what {
    font-size: 0.7rem;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--ink-faint);
    margin: 0.9rem 0 0.5rem;
  }
  .caps {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }
  .cap {
    display: inline-flex;
    align-items: center;
    gap: 0.5rem;
    border: 1px solid var(--line-strong);
    background: var(--surface-2);
    color: var(--ink);
    border-radius: var(--r-pill);
    padding: 0.4rem 0.8rem;
    font-size: 0.84rem;
    font-weight: 600;
    transition: border-color 0.12s ease, background 0.12s ease, color 0.12s ease;
  }
  .cap:hover:not(:disabled) {
    border-color: var(--c-share);
  }
  .cap.on {
    background: var(--c-share-soft);
    border-color: var(--c-share);
    color: var(--c-share-ink);
  }
  .cap:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .cap-i {
    font-size: 0.95rem;
  }
  .cap-pop {
    margin-left: auto;
    font-size: 0.62rem;
    font-weight: 600;
    color: var(--ink-faint);
    text-transform: uppercase;
    letter-spacing: 0.03em;
  }

  @media (max-width: 720px) {
    .body {
      grid-template-columns: 1fr;
    }
    .mid {
      order: 3;
    }
  }
</style>
