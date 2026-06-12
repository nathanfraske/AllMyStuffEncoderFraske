<script lang="ts">
  // The "a new device wants to join" popup — opened from the top-bar nudge.
  // For each waiting device it shows the bilateral confirmation grid (this
  // device's suffix + the code we sent ↔ the peer's suffix + the code it
  // sent) so the two of you can read all four aloud before letting it in.
  //
  // Approve lets the device onto the network. Decline is a *cancel*, not a
  // deny: it dismisses the nudge but leaves the device approvable later under
  // Settings → Networks. (A real block lives elsewhere, coming later.)
  import { app, type PendingJoin } from "../store.svelte";
  import type { PeerInfo } from "../types";

  const joins = $derived(app.freshJoins);

  function close() {
    app.approvalsOpen = false;
  }

  /** Our own device's 5-char display suffix, pulled from the mesh identity. */
  function suffixOf(id: string | undefined): string {
    if (!id) return "";
    const dash = id.lastIndexOf("-");
    if (dash > 0) {
      const s = id.slice(dash + 1);
      if (s.length === 5 && /^[0-9a-zA-Z]+$/.test(s)) return s;
    }
    return "";
  }
  const ourSuffix = $derived(suffixOf(app.identity?.device_id));

  type ApprovalState = "fresh" | "waiting-peer" | "confirm-needed";
  function approvalState(p: PeerInfo): ApprovalState {
    if (p.local_approve_sent && !p.remote_approve_seen) return "waiting-peer";
    if (!p.local_approve_sent && p.remote_approve_seen) return "confirm-needed";
    return "fresh";
  }

  function approve(j: PendingJoin) {
    void app.approveJoin(j);
  }
  function decline(j: PendingJoin) {
    app.dismissJoin(j.peer.device_id);
  }
</script>

<svelte:window onkeydown={(e) => e.key === "Escape" && close()} />

<div class="scrim">
  <button class="backdrop" onclick={close} aria-label="Close"></button>
  <div class="popup" role="dialog" aria-modal="true" aria-label="Devices waiting to join" tabindex="-1">
    <header class="head">
      <span class="bang" aria-hidden="true">!</span>
      <div class="head-text">
        <div class="title">
          {joins.length === 1 ? "A device wants to join" : `${joins.length} devices want to join`}
        </div>
        <div class="sub">Check the codes match on both screens, then approve.</div>
      </div>
      <button class="x" onclick={close} aria-label="Close">✕</button>
    </header>

    <div class="list">
      {#each joins as j (j.peer.device_id)}
        {@const p = j.peer}
        {@const state = approvalState(p)}
        <div class="card" class:confirm={state === "confirm-needed"}>
          <div class="card-head">
            <div class="who">
              <span class="who-name">{p.label || p.device_id.slice(0, 10)}</span>
              <span class="who-net">wants onto “{j.networkName}”</span>
            </div>
            <span class="state-pill {state}">
              {state === "fresh"
                ? "new"
                : state === "confirm-needed"
                  ? "approved you — confirm"
                  : "waiting for the other device"}
            </span>
          </div>

          {#if state !== "waiting-peer"}
            <div class="grid">
              <div class="col">
                <div class="col-label">this device</div>
                <div class="tiles">
                  {#if ourSuffix}
                    <div class="tile suffix">
                      <span class="tile-k">suffix</span>
                      <span class="tile-v">{ourSuffix}</span>
                    </div>
                  {/if}
                  {#if p.verification_code_sent}
                    <div class="tile code">
                      <span class="tile-k">code</span>
                      <span class="tile-v">{p.verification_code_sent}</span>
                    </div>
                  {/if}
                </div>
              </div>

              <div class="arrow" aria-hidden="true">↔</div>

              <div class="col">
                <div class="col-label">that device</div>
                <div class="tiles">
                  {#if p.device_suffix}
                    <div class="tile suffix">
                      <span class="tile-k">suffix</span>
                      <span class="tile-v">{p.device_suffix}</span>
                    </div>
                  {/if}
                  {#if p.verification_code_received}
                    <div class="tile code">
                      <span class="tile-k">code</span>
                      <span class="tile-v">{p.verification_code_received}</span>
                    </div>
                  {/if}
                </div>
              </div>
            </div>
            <p class="match-hint">
              {state === "fresh"
                ? "All four should match what the other device shows before you approve."
                : "The other device already approved — confirm to finish letting it in."}
            </p>
          {:else}
            <p class="waiting">Waiting for the other device to confirm its side…</p>
          {/if}

          <div class="actions">
            {#if state === "waiting-peer"}
              <button class="btn small" onclick={() => decline(j)}>Dismiss</button>
            {:else}
              <button class="btn small" onclick={() => decline(j)} title="Cancel — you can still approve later from Settings">Decline</button>
              <button class="btn small primary" onclick={() => approve(j)}>
                {state === "confirm-needed" ? "Confirm" : "Approve"}
              </button>
            {/if}
          </div>
        </div>
      {/each}

      {#if joins.length === 0}
        <p class="empty">No devices are waiting right now.</p>
      {/if}
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: absolute;
    inset: 0;
    border: none;
    background: transparent;
    cursor: default;
  }
  .popup {
    position: relative;
    z-index: 1;
    width: 34rem;
    max-width: 94vw;
    max-height: 88vh;
    overflow-y: auto;
    background: var(--surface);
    border-radius: var(--r-lg);
    box-shadow: var(--shadow-lg);
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
    padding: 1.1rem 1.3rem 0.9rem;
    border-bottom: 1px solid var(--line);
  }
  .bang {
    display: grid;
    place-items: center;
    width: 2rem;
    height: 2rem;
    border-radius: 50%;
    background: var(--warn);
    color: var(--bg);
    font-weight: 800;
    font-size: 1.1rem;
    flex-shrink: 0;
  }
  .head-text {
    flex: 1;
    min-width: 0;
  }
  .title {
    font-weight: 750;
    font-size: 1.05rem;
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
  .list {
    padding: 0.9rem 1.3rem 1.2rem;
    display: flex;
    flex-direction: column;
    gap: 0.8rem;
  }
  .card {
    border: 1px solid var(--line);
    border-radius: var(--r-md);
    padding: 0.8rem 0.9rem;
    background: var(--surface);
  }
  .card.confirm {
    border-color: var(--warn);
    background: var(--warn-soft);
  }
  .card-head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 0.6rem;
    margin-bottom: 0.7rem;
  }
  .who-name {
    font-weight: 650;
    font-size: 0.95rem;
  }
  .who-net {
    display: block;
    font-size: 0.74rem;
    color: var(--ink-faint);
    margin-top: 0.1rem;
  }
  .state-pill {
    font-size: 0.66rem;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.02em;
    padding: 0.15rem 0.5rem;
    border-radius: var(--r-pill);
    white-space: nowrap;
    flex-shrink: 0;
  }
  .state-pill.fresh {
    background: var(--accent-soft);
    color: var(--accent-ink);
  }
  .state-pill.confirm-needed {
    background: var(--warn-soft);
    color: var(--warn);
  }
  .state-pill.waiting-peer {
    background: var(--surface-2);
    color: var(--ink-soft);
  }
  .grid {
    display: flex;
    align-items: center;
    gap: 0.6rem;
  }
  .col {
    flex: 1;
    min-width: 0;
  }
  .col-label {
    font-size: 0.68rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--ink-faint);
    text-align: center;
    margin-bottom: 0.35rem;
  }
  .tiles {
    display: flex;
    gap: 0.4rem;
    justify-content: center;
  }
  .tile {
    flex: 1;
    min-width: 0;
    border-radius: var(--r-sm);
    padding: 0.4rem 0.3rem;
    text-align: center;
    border: 1px solid transparent;
  }
  .tile.suffix {
    background: var(--violet-soft);
    border-color: oklch(0.62 0.2 292 / 0.4);
  }
  .tile.code {
    background: var(--warn-soft);
    border-color: oklch(0.79 0.14 75 / 0.4);
  }
  .tile-k {
    display: block;
    font-size: 0.6rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--ink-faint);
  }
  .tile-v {
    display: block;
    font-family: var(--mono);
    font-size: 1rem;
    font-weight: 700;
    letter-spacing: 0.08em;
    margin-top: 0.1rem;
  }
  .tile.suffix .tile-v {
    color: var(--violet);
  }
  .tile.code .tile-v {
    color: var(--warn);
  }
  .arrow {
    color: var(--ink-faint);
    font-size: 1.1rem;
    flex-shrink: 0;
  }
  .match-hint {
    font-size: 0.74rem;
    color: var(--ink-soft);
    text-align: center;
    line-height: 1.4;
    margin: 0.6rem 0 0;
  }
  .waiting {
    font-size: 0.82rem;
    color: var(--ink-soft);
    margin: 0.2rem 0 0;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.4rem;
    margin-top: 0.8rem;
  }
  .empty {
    font-size: 0.84rem;
    color: var(--ink-faint);
    text-align: center;
    padding: 1rem;
  }
</style>
