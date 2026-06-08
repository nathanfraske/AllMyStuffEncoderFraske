<script lang="ts">
  // Network setup — the part that makes the mesh real: name this device,
  // create or join a network, and approve the devices that ask to join.
  import { onMount } from "svelte";
  import { app } from "../store.svelte";
  import { networkDisplayName } from "../types";

  let nameInput = $state("");
  let newLabel = $state("");
  let joinId = $state("");
  let joinLabel = $state("");
  let mode = $state<"none" | "create" | "join">("none");

  const hostname = $derived(app.node(app.localId)?.hostname ?? "");
  const trimmedName = $derived(nameInput.trim());
  // Live preview of the naming rule: override shows as "Name (hostname)".
  const namePreview = $derived(
    trimmedName && trimmedName !== hostname ? `${trimmedName} (${hostname})` : hostname || trimmedName || "—",
  );
  const rosterNet = $derived(app.networks.find((n) => n.config_id === app.rosterNetwork) ?? null);

  onMount(() => {
    nameInput = app.identity?.label ?? "";
    void app.refreshNetworks();
    if (app.activeNetwork) void app.refreshRoster(app.activeNetwork.config_id);
  });

  function close() {
    app.networksOpen = false;
  }
  async function saveName() {
    await app.setIdentityLabel(trimmedName);
  }
  async function create() {
    await app.createNetwork(newLabel);
    newLabel = "";
    mode = "none";
  }
  async function join() {
    await app.joinNetwork(joinId, joinLabel);
    joinId = "";
    joinLabel = "";
    mode = "none";
  }
</script>

<svelte:window onkeydown={(e) => e.key === "Escape" && close()} />

<div class="scrim">
  <button class="backdrop" onclick={close} aria-label="Close"></button>
  <div class="panel" role="dialog" aria-modal="true" aria-label="Networks" tabindex="-1">
    <button class="x" onclick={close} aria-label="Close">✕</button>
    <h3>Networks</h3>

    <!-- This device -->
    <section>
      <h4>This device</h4>
      <p class="hint">
        Its name defaults to the machine name. Set an override and it shows as
        <b>Name ({hostname || "hostname"})</b> everywhere.
      </p>
      <div class="row">
        <input class="field" placeholder={hostname || "device name"} bind:value={nameInput} />
        <button class="btn small primary" onclick={saveName}>Save</button>
      </div>
      <div class="preview">Shows as <b>{namePreview}</b></div>
      {#if app.identity?.device_id}
        <div class="devid" title={app.identity.device_id}>id {app.identity.device_id.slice(0, 12)}…</div>
      {/if}
    </section>

    <!-- Networks -->
    <section>
      <div class="sec-head">
        <h4>Your networks</h4>
        <div class="seg">
          <button class="btn small" class:on={mode === "create"} onclick={() => (mode = mode === "create" ? "none" : "create")}>＋ Create</button>
          <button class="btn small" class:on={mode === "join"} onclick={() => (mode = mode === "join" ? "none" : "join")}>⇲ Join</button>
        </div>
      </div>

      {#if mode === "create"}
        <div class="row">
          <input class="field" placeholder="Name (optional, e.g. Home)" bind:value={newLabel} />
          <button class="btn small primary" onclick={create}>Create</button>
        </div>
        <p class="hint">A fresh network id is generated for you — share it from “Add a machine”.</p>
      {:else if mode === "join"}
        <div class="row">
          <input class="field" placeholder="Network handle (paste from the other device)" bind:value={joinId} />
        </div>
        <div class="row">
          <input class="field" placeholder="Name (optional)" bind:value={joinLabel} />
          <button class="btn small primary" disabled={!joinId.trim()} onclick={join}>Join</button>
        </div>
      {/if}

      <ul class="nets">
        {#each app.networks as n (n.config_id)}
          <li class:on={app.rosterNetwork === n.config_id}>
            <button class="net-main" onclick={() => app.refreshRoster(n.config_id)}>
              <span class="net-name">{networkDisplayName(n)}</span>
              <span class="net-sub">{n.network_id}{#if n.phase} · {n.phase}{/if}</span>
            </button>
            <button class="btn small danger" onclick={() => app.leaveNetwork(n.config_id)}>Leave</button>
          </li>
        {/each}
        {#if app.networks.length === 0}
          <li class="empty">No networks yet — create one, or join with a handle from another device.</li>
        {/if}
      </ul>
    </section>

    <!-- Approvals / roster -->
    {#if rosterNet}
      <section>
        <h4>Devices on “{networkDisplayName(rosterNet)}”</h4>

        {#if app.pendingPeers.length > 0}
          <div class="subhead">Waiting for you</div>
          <ul class="people">
            {#each app.pendingPeers as p (p.device_id)}
              <li>
                <div class="p-main">
                  <div class="p-name">{p.label || p.device_id.slice(0, 10)}</div>
                  <div class="p-sub">
                    {#if p.device_suffix}#{p.device_suffix}{/if}
                    {#if p.verification_code_received}· code {p.verification_code_received}{/if}
                  </div>
                </div>
                <button class="btn small primary" onclick={() => app.approveDevice(rosterNet.config_id, p.device_id, p.label)}>Approve</button>
              </li>
            {/each}
          </ul>
        {/if}

        <div class="subhead">Approved</div>
        <ul class="people">
          {#each app.roster as r (r.device_id)}
            <li>
              <div class="p-main">
                <div class="p-name">{r.label || r.device_id.slice(0, 10)}</div>
                <div class="p-sub">{r.device_id.slice(0, 12)}…</div>
              </div>
              <button class="btn small danger" onclick={() => app.removeDevice(rosterNet.config_id, r.device_id)}>Remove</button>
            </li>
          {/each}
          {#if app.roster.length === 0 && app.pendingPeers.length === 0}
            <li class="empty">No devices yet. Install AllMyStuff on another machine and join this network.</li>
          {/if}
        </ul>
      </section>
    {/if}
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
  .panel {
    position: relative;
    z-index: 1;
    width: 34rem;
    max-width: 94vw;
    max-height: 88vh;
    overflow-y: auto;
    background: var(--surface);
    border-radius: var(--r-lg);
    padding: 1.4rem 1.5rem;
    box-shadow: var(--shadow-lg);
    animation: rise 0.16s ease;
  }
  @keyframes rise {
    from {
      transform: translateY(12px) scale(0.98);
      opacity: 0;
    }
  }
  .x {
    position: absolute;
    top: 0.9rem;
    right: 0.9rem;
    border: none;
    background: var(--surface-2);
    color: var(--ink-soft);
    width: 1.9rem;
    height: 1.9rem;
    border-radius: 50%;
  }
  h3 {
    margin: 0 0 0.8rem;
    font-size: 1.25rem;
  }
  section {
    border-top: 1px solid var(--line);
    padding: 0.9rem 0;
  }
  section:first-of-type {
    border-top: none;
    padding-top: 0;
  }
  h4 {
    margin: 0 0 0.4rem;
    font-size: 0.92rem;
  }
  .hint {
    font-size: 0.78rem;
    color: var(--ink-soft);
    margin: 0 0 0.5rem;
    line-height: 1.4;
  }
  .row {
    display: flex;
    gap: 0.4rem;
    margin-bottom: 0.4rem;
  }
  .field {
    flex: 1;
    min-width: 0;
    border: 1px solid var(--line-strong);
    border-radius: var(--r-sm);
    padding: 0.45rem 0.6rem;
    font-size: 0.86rem;
    font-family: inherit;
  }
  .field:focus {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  .preview {
    font-size: 0.8rem;
    color: var(--ink-soft);
  }
  .devid {
    font-size: 0.72rem;
    color: var(--ink-faint);
    margin-top: 0.2rem;
  }
  .sec-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .seg {
    display: flex;
    gap: 0.3rem;
  }
  .btn.on {
    background: var(--accent-soft);
    border-color: var(--accent);
    color: var(--accent-ink);
  }
  .nets,
  .people {
    list-style: none;
    margin: 0.5rem 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
  }
  .nets li,
  .people li {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    background: var(--surface-2);
    border-radius: var(--r-sm);
    padding: 0.4rem 0.5rem;
  }
  .nets li.on {
    box-shadow: 0 0 0 1.5px var(--accent);
  }
  .net-main {
    flex: 1;
    min-width: 0;
    text-align: left;
    border: none;
    background: none;
    cursor: pointer;
    padding: 0.1rem;
  }
  .net-name,
  .p-name {
    font-size: 0.86rem;
    font-weight: 600;
  }
  .net-sub,
  .p-sub {
    font-size: 0.72rem;
    color: var(--ink-faint);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .p-main {
    flex: 1;
    min-width: 0;
  }
  .subhead {
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--ink-faint);
    margin-top: 0.6rem;
  }
  li.empty {
    display: block;
    background: transparent;
    font-size: 0.78rem;
    color: var(--ink-faint);
    line-height: 1.4;
  }
  .btn.danger {
    color: var(--danger);
  }
</style>
