<script lang="ts">
  // The Help sidebar — the ambient half of the CEC technician surface. It
  // rides alongside Sites and Rooms only when the (secret) CEC support area
  // is unlocked, so an ordinary user never sees it. A top toggle joins the
  // shared help queue; when it's on, the customers currently pressing "Ask
  // for help" list below, longest-waiting first, each with a one-tap answer.
  //
  // This is the monitor, not the workbench: dialing by number, the dialed-
  // customer directory, and consent live in the full CEC Console (the
  // Settings tab / its popout window). Watching the queue is the thing a
  // technician wants glanceable while they work, so it earns a sidebar.
  import { app } from "../store.svelte";

  /** "123 456 789" — the spaced support number a customer reads out. */
  function groupNumber(n: string): string {
    const d = (n || "").replace(/\D/g, "");
    return d.length === 9 ? `${d.slice(0, 3)} ${d.slice(3, 6)} ${d.slice(6)}` : n || "—";
  }

  /** "just now" / "4m" / "1h 12m" — how long a hand has been up. */
  function waitingLabel(askedAt: number): string {
    const s = Math.max(0, Math.round(Date.now() / 1000 - (askedAt || 0)));
    if (s < 45) return "just now";
    const m = Math.round(s / 60);
    if (m < 60) return `${m}m`;
    return `${Math.floor(m / 60)}h ${m % 60}m`;
  }

  /** The machine hostname as a dim tail after the display name, only when it
   *  adds information (differs from what's already shown). */
  function hostTail(shown: string, hostname?: string): string {
    const h = hostname?.trim();
    if (!h || h.toLowerCase() === shown.trim().toLowerCase()) return "";
    return ` (${h})`;
  }
</script>

<div class="help">
  <label
    class="watch"
    title="Join the shared help queue and see customers who press Ask for help. Saved — stays on across restarts."
  >
    <input
      type="checkbox"
      checked={app.cecHelpWatching}
      onchange={(e) => void app.setCecHelpWatch(e.currentTarget.checked)}
    />
    <span class="watch-label">Watch the help queue</span>
  </label>

  {#if !app.cecHelpWatching}
    <p class="notice">
      Turn this on to see customers who press <b>Ask for help</b> in their CEC
      Support app. Until then this machine stays off the shared help queue.
    </p>
  {:else if app.cecHelpWaiting.length === 0}
    <p class="notice listening">
      <span class="live-dot" aria-hidden="true"></span>
      Listening — no one is asking right now.
    </p>
  {:else}
    <ul class="rows">
      {#each app.cecHelpWaiting as w (w.node)}
        {@const shownName = app.cecAliases[w.number]?.trim() || w.label?.trim() || "Customer"}
        <li class="row">
          <span class="dot" aria-hidden="true"></span>
          <div class="who">
            <b class="name">{shownName}<span class="host">{hostTail(shownName, w.hostname)}</span></b>
            <span class="sub">
              <span class="num" title={`Number ${w.number}`}>CEC {groupNumber(w.number)}</span>
              <span class="meta">· {waitingLabel(w.asked_at)}</span>
            </span>
          </div>
          <button
            class="answer"
            disabled={app.cecDialing}
            title="Answer — connect and open their screen once they approve"
            onclick={() => void app.answerHelp(w.node, shownName)}
          >
            Answer
          </button>
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .help {
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
  }
  .watch {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.4rem 0.55rem;
    background: var(--surface-2);
    border-radius: var(--r-sm);
    cursor: pointer;
    font-size: 0.82rem;
    font-weight: 600;
  }
  .watch input {
    accent-color: var(--accent);
  }
  .watch-label {
    color: var(--ink);
  }
  .notice {
    margin: 0;
    font-size: 0.78rem;
    line-height: 1.45;
    color: var(--ink-soft);
  }
  .notice.listening {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    color: var(--ok);
  }
  .live-dot {
    width: 0.5rem;
    height: 0.5rem;
    border-radius: 50%;
    background: var(--ok);
    animation: pulse 1.8s ease-out infinite;
  }
  @keyframes pulse {
    0% {
      box-shadow: 0 0 0 0 rgba(26, 143, 76, 0.5);
    }
    70% {
      box-shadow: 0 0 0 0.35rem rgba(26, 143, 76, 0);
    }
    100% {
      box-shadow: 0 0 0 0 rgba(26, 143, 76, 0);
    }
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
    gap: 0.5rem;
    padding: 0.5rem 0.55rem;
    background: var(--surface-2);
    border-radius: var(--r-sm);
  }
  .dot {
    flex-shrink: 0;
    width: 0.55rem;
    height: 0.55rem;
    border-radius: 50%;
    background: var(--warn);
  }
  .who {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 0.05rem;
  }
  .name {
    font-size: 0.84rem;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .host {
    color: var(--ink-faint);
    font-weight: 400;
  }
  .sub {
    font-size: 0.72rem;
    color: var(--ink-soft);
  }
  .num {
    font-variant-numeric: tabular-nums;
  }
  .answer {
    flex-shrink: 0;
    border: none;
    background: var(--accent);
    color: #fff;
    font: inherit;
    font-size: 0.76rem;
    font-weight: 700;
    padding: 0.3rem 0.65rem;
    border-radius: var(--r-sm);
    cursor: pointer;
  }
  .answer:disabled {
    opacity: 0.5;
    cursor: default;
  }
</style>
