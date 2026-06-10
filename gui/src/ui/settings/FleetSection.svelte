<script lang="ts">
  // Fleet pane — the "Owned" roster: the devices you've claimed, linked by a
  // single shared key gossiped between them. For now this only groups your
  // machines internally; a later edition lets you hand that key to other
  // things.
  import { onMount } from "svelte";
  import { app } from "../../store.svelte";

  const fleet = $derived(app.ownedFleet);
  const members = $derived(fleet?.members ?? []);
  const hasFleet = $derived(!!fleet && !!fleet.key && members.length > 0);
  // Membership is the permission: you can leave, and kick others, only
  // while this device is in the fleet itself.
  const selfIsMember = $derived(members.some((m) => app.isMe(m.device)));

  let revealed = $state(false);
  let copied = $state(false);
  // Two-step confirm: first click arms (shows "sure?"), second acts. The
  // armed id is the member's device (or "leave" for the leave button).
  let armed = $state<string | null>(null);

  onMount(() => void app.loadOwnedFleet());

  function confirmThen(id: string, act: () => void) {
    if (armed === id) {
      armed = null;
      act();
    } else {
      armed = id;
      setTimeout(() => {
        if (armed === id) armed = null;
      }, 3500);
    }
  }

  // Show the key as a short, safe-to-glance fingerprint unless revealed.
  const keyShown = $derived.by(() => {
    const k = fleet?.key ?? "";
    if (!k) return "";
    if (revealed) return k;
    return `${k.slice(0, 6)}…${k.slice(-4)}`;
  });

  async function copyKey() {
    if (!fleet?.key) return;
    try {
      await navigator.clipboard.writeText(fleet.key);
      copied = true;
      setTimeout(() => (copied = false), 1500);
    } catch {
      app.toast("warn", "Couldn't copy the key");
    }
  }

  function nodeLabel(device: string): string {
    const n = app.catalog.nodes.find((x) => x.id === device) ?? null;
    return n?.label ?? "";
  }
</script>

<div class="section">
  <h3>Fleet</h3>
  <p class="lead">
    The devices you've <b>claimed</b> are linked into a fleet by a shared key,
    gossiped between them as an “Owned” roster. Today the key groups your
    machines internally; later you'll be able to hand it to other things.
  </p>

  {#if hasFleet}
    <section class="block key-block">
      <div class="key-head">
        <span class="key-title">🔑 Fleet key</span>
        <span class="muted">v{fleet?.version}</span>
      </div>
      <div class="key-row">
        <code class:revealed>{keyShown}</code>
        <button class="btn small" onclick={() => (revealed = !revealed)}>{revealed ? "Hide" : "Reveal"}</button>
        <button class="btn small" onclick={copyKey}>{copied ? "Copied ✓" : "Copy"}</button>
      </div>
      <p class="hint">Every device below holds this same key. It's an internal grouping secret — keep it private.</p>
    </section>

    <section class="block">
      <h4>{members.length} device{members.length === 1 ? "" : "s"} in your fleet</h4>
      <ul class="members">
        {#each members as m (m.device)}
          {@const live = nodeLabel(m.device)}
          {@const isSelf = app.isMe(m.device)}
          <li>
            <span class="m-avatar" aria-hidden="true">{isSelf ? "💻" : "🖥"}</span>
            <div class="m-id">
              <div class="m-name">{m.label || live || m.device.slice(0, 12)}{#if isSelf} <span class="self-tag">this device</span>{/if}</div>
              <div class="m-sub" title={m.device}>{m.device.slice(0, 18)}…</div>
            </div>
            {#if selfIsMember && !isSelf}
              <button
                class="kick"
                class:armed={armed === m.device}
                title="Remove this device from the fleet (it's also released from your ownership)"
                onclick={() => confirmThen(m.device, () => void app.kickFleetMember(m.device))}
              >
                {armed === m.device ? "Kick — sure?" : "Kick"}
              </button>
            {/if}
          </li>
        {/each}
      </ul>
      {#if selfIsMember}
        <div class="leave-row">
          <button
            class="btn small leave"
            class:armed={armed === "leave"}
            title="Remove this device from the fleet (its owner is released too)"
            onclick={() => confirmThen("leave", () => void app.leaveFleet())}
          >
            {armed === "leave" ? "Leave the fleet — sure?" : "Leave the fleet"}
          </button>
          <span class="hint">
            Leaving (or being kicked) drops the shared key here and releases
            ownership — the device goes back to unclaimed.
          </span>
        </div>
      {:else}
        <p class="hint">
          This device isn't in the fleet, so it can only watch the roster —
          you can't kick devices from a fleet you aren't in.
        </p>
      {/if}
    </section>
  {:else}
    <section class="block empty-block">
      <div class="empty-orb">🔗</div>
      <div class="empty-title">No fleet yet</div>
      <p class="hint center">
        Claim a device that's offering itself for adoption — open it from the
        graph and choose <b>Claim this device</b>. It and this machine will be
        linked under a fresh shared key, and the rest of your claimed devices
        join the same fleet.
      </p>
    </section>
  {/if}
</div>

<style>
  .section {
    display: flex;
    flex-direction: column;
  }
  h3 {
    margin: 0 0 0.4rem;
    font-size: 1.2rem;
  }
  .lead {
    font-size: 0.84rem;
    color: var(--ink-soft);
    line-height: 1.5;
    margin: 0 0 0.4rem;
  }
  .block {
    border-top: 1px solid var(--line);
    padding: 0.9rem 0;
  }
  h4 {
    margin: 0 0 0.5rem;
    font-size: 0.92rem;
  }
  .hint {
    font-size: 0.78rem;
    color: var(--ink-faint);
    line-height: 1.45;
    margin: 0.4rem 0 0;
  }
  .hint.center {
    text-align: center;
    max-width: 24rem;
  }
  .muted {
    font-size: 0.74rem;
    color: var(--ink-faint);
  }
  .key-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 0.4rem;
  }
  .key-title {
    font-size: 0.9rem;
    font-weight: 650;
  }
  .key-row {
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }
  code {
    flex: 1;
    min-width: 0;
    background: var(--surface-2);
    border: 1px solid var(--line);
    border-radius: var(--r-sm);
    padding: 0.4rem 0.6rem;
    font-size: 0.82rem;
    font-family: var(--mono);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    letter-spacing: 0.02em;
  }
  code.revealed {
    word-break: break-all;
    white-space: normal;
  }
  .members {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }
  .members li {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    background: var(--surface-2);
    border-radius: var(--r-sm);
    padding: 0.5rem 0.6rem;
  }
  .m-avatar {
    font-size: 1.2rem;
  }
  .m-id {
    min-width: 0;
    flex: 1;
  }
  .m-name {
    font-size: 0.88rem;
    font-weight: 600;
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }
  .self-tag {
    font-size: 0.62rem;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.03em;
    color: var(--accent-ink);
    background: var(--accent-soft);
    border-radius: var(--r-pill);
    padding: 0.05rem 0.4rem;
  }
  .m-sub {
    font-size: 0.72rem;
    color: var(--ink-faint);
    font-family: var(--mono);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .kick {
    flex-shrink: 0;
    border: 1px solid var(--line);
    background: var(--surface);
    color: var(--ink-soft);
    border-radius: var(--r-pill);
    padding: 0.22rem 0.6rem;
    font-size: 0.72rem;
    font-weight: 650;
    cursor: pointer;
    transition: border-color 0.12s ease, color 0.12s ease, background 0.12s ease;
  }
  .kick:hover,
  .kick.armed {
    border-color: #e0939f;
    color: var(--danger);
    background: #fdeaee;
  }
  .leave-row {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    margin-top: 0.7rem;
  }
  .leave-row .hint {
    margin: 0;
    flex: 1;
  }
  .leave.armed {
    border-color: #e0939f;
    color: var(--danger);
    background: #fdeaee;
  }
  .empty-block {
    display: flex;
    flex-direction: column;
    align-items: center;
    text-align: center;
    gap: 0.3rem;
    padding: 2rem 1rem;
  }
  .empty-orb {
    font-size: 2.4rem;
    opacity: 0.8;
  }
  .empty-title {
    font-weight: 700;
    font-size: 1rem;
  }
</style>
