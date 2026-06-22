<script lang="ts">
  // Networks pane — the real, multi-network system. Sub-tabs (MyOwnLLM-style):
  //   • Status  — this device's name, add-a-device handle, your networks, and
  //               who's waiting to join.
  //   • Servers — per-network signaling / STUN / TURN.
  //   • Devices — every machine you can see and which network(s) it's on
  //               (you're on as many networks as you've joined, and a device
  //               may share only some of them).
  import { onMount } from "svelte";
  import { app } from "../../store.svelte";
  import { networkDisplayName } from "../../types";
  import { PUBLIC_VENUE_ID } from "../../venues";
  import NetworkServers from "./NetworkServers.svelte";
  import NetworkDevices from "./NetworkDevices.svelte";

  let nameInput = $state("");
  let joinId = $state("");
  let joinVenue = $state(PUBLIC_VENUE_ID);
  let mode = $state<"none" | "join">("none");
  let copied = $state("");

  const sub = $derived(app.networksSubtab);
  const hostname = $derived(app.node(app.localId)?.hostname ?? "");
  const trimmedName = $derived(nameInput.trim());
  const namePreview = $derived(
    trimmedName && trimmedName !== hostname ? `${trimmedName} (${hostname})` : hostname || trimmedName || "—",
  );
  const rosterNet = $derived(app.networks.find((n) => n.config_id === app.rosterNetwork) ?? null);
  const pending = $derived(app.pendingPeers);

  onMount(() => {
    nameInput = app.identity?.label ?? "";
    void app.refreshNetworks();
    void app.loadNetworkConfigs();
    if (app.activeNetwork) void app.refreshRoster(app.activeNetwork.config_id);
  });

  async function saveName() {
    await app.setIdentityLabel(trimmedName);
  }
  async function join() {
    await app.joinNetwork(joinId, [joinVenue]);
    joinId = "";
    joinVenue = PUBLIC_VENUE_ID;
    mode = "none";
  }
  async function copyHandle(handle: string) {
    try {
      await navigator.clipboard.writeText(handle);
      copied = handle;
      setTimeout(() => (copied = ""), 1500);
    } catch {
      app.toast("warn", "Couldn't copy — select it by hand");
    }
  }

  // The fleet mesh has no plain "leave" — leaving it *is* leaving the fleet. So
  // the button warns first (a toast + an armed second click), then routes to the
  // real exit, `leaveFleet`, which releases this device and tears down the mesh.
  let armedFleetLeave = $state(false);
  function fleetMeshLeave() {
    if (!armedFleetLeave) {
      armedFleetLeave = true;
      app.toast("warn", "This is your fleet mesh — leaving it leaves the fleet. Click again to confirm.");
      setTimeout(() => (armedFleetLeave = false), 4000);
      return;
    }
    armedFleetLeave = false;
    void app.leaveFleet();
  }

  // Import = the no-typing way onto a network: pick a settings file the other
  // device exported and the network (handle + servers) is recreated here.
  let importInput = $state<HTMLInputElement | null>(null);
  function onImportFile(e: Event) {
    const input = e.currentTarget as HTMLInputElement;
    const file = input.files?.[0] ?? null;
    input.value = ""; // so re-picking the same file still fires onchange
    if (!file) return;
    file
      .text()
      .then((text) => app.importNetworkSettings(text))
      .catch((err) => app.toast("warn", `Couldn't read that file: ${String(err)}`));
  }
</script>

<div class="section">
  <div class="head">
    <h3>Meshes</h3>
    <div class="subtabs">
      <button class:active={sub === "status"} onclick={() => (app.networksSubtab = "status")}>Status</button>
      <button class:active={sub === "servers"} onclick={() => { app.networksSubtab = "servers"; void app.loadNetworkConfigs(); }}>Venue</button>
      <button class:active={sub === "devices"} onclick={() => (app.networksSubtab = "devices")}>Devices</button>
    </div>
  </div>

  {#if sub === "servers"}
    <NetworkServers />
  {:else if sub === "devices"}
    <NetworkDevices />
  {:else}
    <!-- This device -->
    <section class="block">
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

    <!-- Your networks (with add/join + per-network handle) -->
    <section class="block">
      <div class="sec-head">
        <h4>Your meshes — joined {app.networks.length}</h4>
        <div class="seg">
          <button class="btn small" class:on={mode === "join"} onclick={() => (mode = mode === "join" ? "none" : "join")}>⇲ Join</button>
          <button class="btn small" title="Add a network from a settings file another device exported" onclick={() => importInput?.click()}>↧ Import</button>
        </div>
      </div>
      <input bind:this={importInput} type="file" accept=".json,application/json" hidden onchange={onImportFile} />
      <p class="hint">
        You can be on as many networks as you like — devices on any of them show up,
        so it's never just “the” mesh. Share a mesh's handle to add a device to it.
      </p>

      {#if mode === "join"}
        <div class="row">
          <input
            class="field"
            placeholder="mesh name OR leave blank to generate one"
            bind:value={joinId}
            onkeydown={(e) => e.key === "Enter" && join()}
          />
          <select class="venue-pick" title="Which venue this mesh calls out at" bind:value={joinVenue}>
            {#each app.venues as v (v.id)}
              <option value={v.id}>{v.label}</option>
            {/each}
          </select>
          <button class="btn small primary" onclick={join}>Join</button>
        </div>
        <p class="hint">A mesh is just a name you agree on — anyone who uses the same name meets here. Leave it blank for a memorable generated one. The venue is where it calls out (Public by default).</p>
      {/if}

      <ul class="nets">
        {#each app.networks as n (n.config_id)}
          {@const fleetMesh = app.isFleetMesh(n)}
          <li class:on={app.rosterNetwork === n.config_id} class:fleet={fleetMesh}>
            <button class="net-main" onclick={() => app.refreshRoster(n.config_id)}>
              <span class="net-name">{app.meshLabel(n)}{#if fleetMesh}<span class="badge fleet-badge" title="The closed mesh that backs your fleet">🔗 fleet</span>{/if}{#if app.sessionNetwork === n.config_id}<span class="badge">active</span>{/if}</span>
              <span class="net-sub">{n.network_id}{#if n.phase} · {n.phase}{/if}</span>
            </button>
            <button class="btn small" title="Copy this mesh's handle to add a device" onclick={() => copyHandle(n.network_id)}>{copied === n.network_id ? "Copied ✓" : "Copy id"}</button>
            <!-- Export and Copy id work the same for every mesh — even the
                 fleet's. The fleet mesh differs in three ways: it can't be
                 *disabled*; its venue is owner-only (members and managers ride
                 the owner's choice, broadcast to them); and leaving it leaves
                 the fleet, so its Leave warns and routes to the real exit. -->
            <button class="btn small" title="Save this mesh's full settings to a file to import on another device" onclick={() => app.exportNetwork(n.config_id)}>Export</button>
            {#if fleetMesh && !app.isFleetOwner}
              <button class="btn small locked" disabled title="The fleet's venue is set by the fleet owner — every device rides the owner's choice. Managers manage members, not core settings.">🔒 Venue</button>
            {:else}
              <button class="btn small" title="Choose where this mesh calls out (its venue)" onclick={() => { app.serversNetwork = n.config_id; app.networksSubtab = "servers"; void app.loadNetworkConfigs(); }}>Venue</button>
            {/if}
            {#if fleetMesh}
              <button
                class="btn small locked"
                disabled
                title="This is your fleet mesh — it can't be disabled. Leave the fleet to leave this mesh."
              >🔒 Can't disable</button>
              <button
                class="btn small danger"
                class:armed={armedFleetLeave}
                title="Leaving this mesh leaves your fleet — this device is released back to unclaimed."
                onclick={fleetMeshLeave}
              >{armedFleetLeave ? "Leaves the fleet — sure?" : "Leave"}</button>
            {:else}
              <button class="btn small" title="Switch this mesh off without deleting it (the pill menu can turn it back on)" onclick={() => app.toggleNetworkEnabled(n.config_id, false)}>Disable</button>
              <button class="btn small danger" onclick={() => app.leaveNetwork(n.config_id)}>Leave</button>
            {/if}
          </li>
        {/each}
        {#each app.disabledNets as c (c.id)}
          <li class="off">
            <div class="net-main">
              <span class="net-name">{networkDisplayName(c)}<span class="badge off-badge">disabled</span></span>
              <span class="net-sub">{c.network_id} · kept for later — devices there can't see you</span>
            </div>
            <button class="btn small primary" onclick={() => app.toggleNetworkEnabled(c.id, true)}>Enable</button>
          </li>
        {/each}
        {#if app.networks.length === 0 && app.disabledNets.length === 0}
          <li class="empty">No networks yet — create one, or join with a handle from another device.</li>
        {/if}
      </ul>
    </section>

    <!-- Add a device -->
    <section class="block">
      <h4>Add a device</h4>
      <p class="hint">
        Machines aren't added by hand — install AllMyStuff on the other device,
        join one of your networks with its handle (Copy id above), then approve
        it below. It then appears on the graph on its own.
      </p>
    </section>

    <!-- Approvals / roster -->
    {#if rosterNet}
      <section class="block">
        <h4>Devices on “{app.meshLabel(rosterNet)}”</h4>

        {#if pending.length > 0}
          <div class="subhead">Waiting for you</div>
          <ul class="people">
            {#each pending as p (p.device_id)}
              <li>
                <div class="p-main">
                  <div class="p-name">{p.label || p.device_id.slice(0, 10)}</div>
                  <div class="p-sub">
                    {#if p.device_suffix}#{p.device_suffix}{/if}
                    {#if p.verification_code_received}· code {p.verification_code_received}{/if}
                  </div>
                </div>
                <button class="btn small" onclick={() => app.dismissJoin(p.device_id)} title="Not now (you can still approve later)">Not now</button>
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
          {#if app.roster.length === 0 && pending.length === 0}
            <li class="empty">No devices yet. Install AllMyStuff on another machine and join this mesh.</li>
          {/if}
        </ul>
      </section>
    {/if}
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
  .subtabs {
    display: flex;
    gap: 0.2rem;
    background: var(--surface-2);
    border-radius: var(--r-pill);
    padding: 0.2rem;
  }
  .subtabs button {
    border: none;
    background: none;
    color: var(--ink-soft);
    font-size: 0.8rem;
    font-weight: 600;
    padding: 0.32rem 0.7rem;
    border-radius: var(--r-pill);
  }
  .subtabs button.active {
    background: var(--surface);
    color: var(--accent-ink);
    box-shadow: var(--shadow-sm);
  }
  .block {
    border-top: 1px solid var(--line);
    padding: 0.9rem 0;
  }
  h4 {
    margin: 0 0 0.4rem;
    font-size: 0.92rem;
  }
  .hint {
    font-size: 0.78rem;
    color: var(--ink-soft);
    margin: 0 0 0.5rem;
    line-height: 1.45;
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
  .venue-pick {
    flex-shrink: 0;
    max-width: 11rem;
    border: 1px solid var(--line-strong);
    border-radius: var(--r-sm);
    background: var(--surface);
    color: var(--ink);
    padding: 0.45rem 0.5rem;
    font-size: 0.82rem;
    font-family: inherit;
  }
  .venue-pick:focus {
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
    gap: 0.4rem;
    background: var(--surface-2);
    border-radius: var(--r-sm);
    padding: 0.4rem 0.5rem;
  }
  .nets li.on {
    box-shadow: 0 0 0 1.5px var(--accent);
  }
  .nets li.off {
    opacity: 0.7;
    border: 1px dashed var(--line-strong);
    background: transparent;
  }
  .nets li.off .net-main {
    cursor: default;
  }
  .off-badge {
    color: var(--ink-faint);
    background: var(--surface-2);
    border: 1px solid var(--line-strong);
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
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }
  .badge {
    font-size: 0.6rem;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.03em;
    color: var(--ok);
    background: var(--ok-soft);
    border-radius: var(--r-pill);
    padding: 0.05rem 0.4rem;
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
  .nets li.fleet {
    box-shadow: inset 0 0 0 1.5px var(--accent-soft);
  }
  .badge.fleet-badge {
    color: var(--accent-ink);
    background: var(--accent-soft);
  }
  .btn.locked {
    opacity: 0.8;
    cursor: not-allowed;
    color: var(--ink-faint);
  }
</style>
