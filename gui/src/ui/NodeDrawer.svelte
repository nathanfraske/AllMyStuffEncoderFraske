<script lang="ts">
  import { app } from "../store.svelte";
  import {
    MEDIA,
    displayName,
    isAppNode,
    originIcon,
    humanBytes,
    mediaColor,
    siteIcon,
    type Capability,
    type Grant,
    type GrantRole,
    type ListeningService,
    type MediaKind,
  } from "../types";

  const node = $derived(app.selectedNode);
  const caps = $derived(node ? app.capsOf(node.id) : []);
  const shared = $derived(node?.relationship.kind === "shared");
  // A device on the mesh that isn't running AllMyStuff: nothing to wire.
  const meshonly = $derived(!!node && !isAppNode(node));
  // This device declares an owner that isn't us — it can't be adopted.
  // Compare by canonical pubkey (`isMe`): presence carries the owner as a
  // bare pubkey while our local id is the display form.
  const ownedByOther = $derived(!!node?.owner && !app.isMe(node.owner));
  // A remote machine you can open a console session on.
  const isRemoteApp = $derived(!!node && node.kind !== "this" && !meshonly);

  // Whether this device is part of *your* fleet — your own machine, one you
  // own, or a co-member sharing your fleet key. Its raw capabilities ("Its
  // stuff") are only shown for these: a guest's or a stranger's machine on
  // the mesh isn't yours to wire up from here.
  const inMyFleet = $derived(
    !!node &&
      (node.kind === "this" ||
        node.relationship.kind === "mine" ||
        app.isFleetMember(node.id) ||
        (!!node.owner && app.isMe(node.owner))),
  );

  // Capabilities grouped by media for tidy sections.
  const grouped = $derived.by(() => {
    const m = new Map<MediaKind, Capability[]>();
    for (const c of caps) {
      const arr = m.get(c.media) ?? [];
      arr.push(c);
      m.set(c.media, arr);
    }
    return [...m.entries()];
  });

  // Within a media group, sends (sources) and receives (sinks) are listed
  // apart — "what this can give you" vs "what you can give it" — with
  // duplex endpoints in their own cluster.
  type FlowCluster = { key: string; label: string; arrow: string; items: Capability[] };
  function byFlow(list: Capability[]): FlowCluster[] {
    const cluster = (key: string, label: string, arrow: string, flow: string) => ({
      key,
      label,
      arrow,
      items: list.filter((c) => c.flow === flow),
    });
    return [
      cluster("sends", "Sends", "↥", "source"),
      cluster("receives", "Receives", "↧", "sink"),
      cluster("both", "Both ways", "⇅", "duplex"),
    ].filter((c) => c.items.length > 0);
  }

  // Routes touching this node, with the far end + direction. Resolved
  // through the display fallback so terminal sessions (whose endpoints
  // are deliberately not catalog capabilities) still show — and can be
  // disconnected — here.
  const connections = $derived.by(() => {
    if (!node) return [];
    const out: { id: string; label: string; media: MediaKind; dir: "out" | "in" }[] = [];
    for (const r of app.catalog.routes) {
      const from = app.capabilityForDisplay(r.from);
      const to = app.capabilityForDisplay(r.to);
      if (!from || !to) continue;
      if (from.node === node.id) {
        out.push({ id: r.id, label: `${from.label} → ${to.label}`, media: r.media, dir: "out" });
      } else if (to.node === node.id) {
        out.push({ id: r.id, label: `${from.label} → ${to.label}`, media: r.media, dir: "in" });
      }
    }
    return out;
  });

  // Friendly share presets — what you can let a guest do — minus what's
  // already granted.
  interface Preset { label: string; media: MediaKind; role: GrantRole }
  const PRESETS: Preset[] = [
    { label: "See your screen", media: "display", role: "consume" },
    { label: "Hear your audio", media: "audio", role: "consume" },
    { label: "Send you their camera", media: "video", role: "provide" },
    { label: "Speak to you (their mic)", media: "audio", role: "provide" },
    { label: "Share files both ways", media: "storage", role: "both" },
  ];

  // The share partner this node belongs to, with the *person-wide* grant
  // list (a grant covers every node they bring, wherever it's recorded)
  // and their other machines for the "applies to all of these" hint.
  const partner = $derived(
    node && node.relationship.kind === "shared"
      ? app.sharePartners.find(
          (p) => node.relationship.kind === "shared" && p.person.id === node.relationship.person.id,
        ) ?? null
      : null,
  );
  const grants = $derived(partner?.grants ?? []);

  const availablePresets = $derived(
    PRESETS.filter((p) => !grants.some(({ grant: g }) => g.media === p.media && g.role === p.role)),
  );

  let addingGrant = $state(false);
  /** Whether the capability list is expanded — starts folded so the drawer
   *  leads with the relationship, not a wall of devices. */
  let stuffOpen = $state(false);
  /** Whether the "Its sites" section is expanded (its own collapse, under
   *  "Its stuff"). Opening it fetches a fleet member's site list. */
  let sitesOpen = $state(false);
  /** Per-row name drafts for the expose inputs in "Its sites". */
  let exposeNames = $state<Record<string, string>>({});

  const deviceSites = $derived(node ? app.deviceServices(node.id) : []);
  function exposeNameFor(svc: ListeningService): string {
    if (!node) return "";
    return exposeNames[svc.id] ?? (app.deviceExposeName(node.id, svc.id) || app.defaultSiteName(svc));
  }
  // Pull a fleet member's site list whenever the section is open for it.
  $effect(() => {
    if (sitesOpen && node) app.ensureDeviceSites(node.id);
  });

  // ---- sidebar sizing --------------------------------------------------
  //
  // The drawer is a real sidebar beside the graph now (it shares the flex
  // row, so the graph reflows to make room) rather than a panel floating
  // over it. It's resizable from its left edge and collapsible to a thin
  // rail, so reading a device's detail never blocks graph navigation.
  const WIDTH_KEY = "allmystuff.drawer.width.v1";
  const MIN_W = 280;
  const MAX_W = 560;
  const DEFAULT_W = 384;

  function loadWidth(): number {
    try {
      const v = Number(localStorage.getItem(WIDTH_KEY));
      return v >= MIN_W && v <= MAX_W ? v : DEFAULT_W;
    } catch {
      return DEFAULT_W;
    }
  }

  let width = $state(loadWidth());
  let collapsed = $state(false);
  let resizing = $state(false);

  // A fresh selection always re-opens the panel — you clicked a node to see
  // it. Tracked by id so a presence refresh (same node, new object) doesn't
  // keep springing a deliberately-collapsed panel back open.
  let shownId = $state<string | null>(null);
  $effect(() => {
    const id = node?.id ?? null;
    if (id !== shownId) {
      shownId = id;
      collapsed = false;
    }
  });

  function startResize(e: PointerEvent) {
    resizing = true;
    (e.currentTarget as Element).setPointerCapture?.(e.pointerId);
    e.preventDefault();
  }
  function onResizeMove(e: PointerEvent) {
    if (!resizing) return;
    // The drawer is flush against the window's right edge, so its width is
    // simply the distance from the pointer to that edge.
    width = Math.min(MAX_W, Math.max(MIN_W, window.innerWidth - e.clientX));
  }
  function endResize(e: PointerEvent) {
    if (!resizing) return;
    resizing = false;
    (e.currentTarget as Element).releasePointerCapture?.(e.pointerId);
    try {
      localStorage.setItem(WIDTH_KEY, String(Math.round(width)));
    } catch {
      /* private mode — the width just doesn't persist */
    }
  }

  function addGrant(p: Preset) {
    if (!node) return;
    const g: Grant = {
      id: `grant:${Date.now()}:${Math.random().toString(36).slice(2, 6)}`,
      media: p.media,
      role: p.role,
      capability: null,
      label: p.label,
    };
    app.grant(node.id, g);
    addingGrant = false;
  }

  function makeShared() {
    if (!node) return;
    app.markShared(node.id);
  }
  /** Adopt this device — gated: only takes if it's in claim mode (Task 4). */
  function claimThis() {
    if (!node) return;
    app.claim(node.id);
  }
</script>

{#if node}
  <aside
    class="drawer"
    class:collapsed
    class:resizing
    style={collapsed ? "" : `width: ${width}px`}
    aria-label="{displayName(node)} details"
  >
    {#if collapsed}
      <!-- Collapsed: a thin rail that stays out of the graph's way while
           keeping the selection — click to bring the detail back. -->
      <div class="rail">
        <button
          class="rail-btn"
          onclick={() => (collapsed = false)}
          title="Expand details"
          aria-label="Expand details">‹</button
        >
        <span class="rail-avatar" aria-hidden="true"
          >{meshonly ? "📡" : shared ? "🧑" : node.kind === "this" ? "💻" : "🖥"}</span
        >
        <button
          class="rail-btn"
          onclick={() => app.selectNode(null)}
          title="Close"
          aria-label="Close">✕</button
        >
      </div>
    {:else}
      <!-- Drag this edge to resize the sidebar. -->
      <div
        class="resizer"
        role="separator"
        aria-label="Resize panel"
        aria-orientation="vertical"
        onpointerdown={startResize}
        onpointermove={onResizeMove}
        onpointerup={endResize}
        onpointercancel={endResize}
      ></div>
      <div class="drawer-body">
        <header class="head">
      <span class="avatar">{meshonly ? "📡" : shared ? "🧑" : node.kind === "this" ? "💻" : "🖥"}</span>
      <div class="title">
        <div class="name">{displayName(node)}</div>
        <div class="kindline">
          {#if node.kind === "this"}this device · {/if}
          {#if meshonly}
            <span class="pill soft">not on AllMyStuff</span>
          {:else if shared && node.relationship.kind === "shared"}
            <span class="pill guest">shared with {node.relationship.person.name}</span>
          {:else if node.relationship.kind === "unclaimed"}
            <!-- A device that declares an owner that isn't us was claimed
                 by someone else — that's a fact, not a blank slate, so the
                 chip says so instead of "unclaimed". -->
            {#if ownedByOther}
              <span class="pill theirs">someone else's</span>
            {:else}
              <span class="pill soft">{node.claimable ? "claimable" : "unclaimed"}</span>
            {/if}
          {:else}
            <span class="pill mine">yours</span>
          {/if}
          {#if app.isFleetMember(node.id)}<span class="pill fleet" title="In your owned fleet (shared key)">🔗 fleet</span>{/if}
          <span class="state" class:on={node.online}>{node.online ? "online" : "offline"}</span>
        </div>
        {#if node.networks && node.networks.length}
          <div class="netline" title="On {node.networks.join(', ')}">
            <span class="netline-k">on</span>
            {#each node.networks as net}<span class="net-chip">{net}</span>{/each}
          </div>
        {/if}
      </div>
      <button
        class="x collapse"
        onclick={() => (collapsed = true)}
        title="Collapse panel"
        aria-label="Collapse panel">⟩</button
      >
      <button class="x" onclick={() => app.selectNode(null)} aria-label="Close">✕</button>
    </header>

    {#if node.summary}
      <section class="stats">
        <div class="stat"><span>System</span><b>{node.summary.os}</b></div>
        <div class="stat"><span>Chip</span><b>{node.summary.cpu}</b></div>
        <div class="stat"><span>Memory</span><b>{humanBytes(node.summary.ram_bytes)}</b></div>
        <div class="stat"><span>Things</span><b>{node.summary.device_count}</b></div>
      </section>
    {/if}

    <!-- Open a remote control session: the pikvm-style handle for this
         machine's screen, audio passthrough and control. Owner/fleet only —
         sharing is one-directional: when you share your stuff *with* someone,
         their machine isn't yours to drive, so it never offers a remote
         control (the far side would refuse it anyway). -->
    {#if isRemoteApp && node.relationship.kind === "mine"}
      <button class="btn primary console-open" onclick={() => app.openConsole(node.id)}>
        🖥 Remote Control
      </button>
    {/if}

    <!-- Open the file manager: a finder-like view of that machine's disk,
         over the mesh. Owner/fleet only — the same rule as the terminal,
         enforced again on the far side. -->
    {#if isRemoteApp && app.filesAllowed(node)}
      <button class="btn console-open" onclick={() => app.openFiles(node.id)}>
        🗂 Open Files
      </button>
    {/if}

    <!-- Open a terminal: a real shell on that machine, over the mesh.
         Only for machines that advertise the feature *and* are effectively
         yours (owner/fleet — the same rule the far side enforces). -->
    {#if isRemoteApp && app.terminalAllowed(node)}
      <button class="btn console-open" onclick={() => app.openTerminal(node.id)}>
        📟 Open Terminal
      </button>
    {/if}

    <!-- Relationship / sharing -->
    <section class="block">
      {#if meshonly}
        <p class="muted">
          This device is on your mesh, but it isn't running AllMyStuff — so it
          has no screens, mics or other things to wire up, and it can't be a
          connection target. Install AllMyStuff on it and it'll fill in here.
        </p>
        {#if node.id}<div class="devid" title={node.id}>mesh id {node.id.slice(0, 16)}…</div>{/if}
      {:else if node.relationship.kind === "unclaimed"}
        {#if ownedByOther}
          <p class="muted">
            This device already has an owner, so you can't adopt it. If they
            want to share it with you, you'll get exactly what they allow.
          </p>
          <button class="linklike" onclick={makeShared}>I'm sharing with its owner →</button>
        {:else if node.claimable}
          <!-- The forefront claim affordance — an accent call-to-action, not a
               buried button. Claiming is authorization, so the copy is the same
               shape the sharing flow uses; the big button makes "make it mine"
               the obvious next move. -->
          <div class="claim-card">
            <div class="claim-card-head">
              <span class="claim-glyph" aria-hidden="true">＋</span>
              <div>
                <div class="claim-card-title">Make {displayName(node)} yours</div>
                <div class="claim-card-sub">It's in claim mode — offering itself for adoption.</div>
              </div>
            </div>
            <p class="claim-card-what">
              Claiming links it into your fleet under a shared key, so your
              devices trust each other for screen, files and control. It's the
              same kind of authorization you'll use to share with people.
            </p>
            <button class="btn primary claim-go" onclick={claimThis}>Claim this device</button>
            <button class="linklike" onclick={makeShared}>It's someone else's — I'm just sharing with them →</button>
          </div>
        {:else}
          <p class="muted">
            This device hasn't been put up for adoption. You can't just take
            ownership — start AllMyStuff on it in claim mode (or toggle
            “allow adoption” there), then claim it from here.
          </p>
          <button class="linklike" onclick={makeShared}>I'm sharing with someone →</button>
        {/if}
      {:else if shared && node.relationship.kind === "shared"}
        <div class="block-head">
          <h4>What {node.relationship.person.name} can do</h4>
          <button class="btn small" onclick={() => (addingGrant = !addingGrant)}>
            {addingGrant ? "Done" : "Allow more"}
          </button>
        </div>
        {#if grants.length === 0}
          <p class="muted">Nothing yet — they can't reach any of your stuff until you allow it.</p>
        {:else if (partner?.nodes.length ?? 0) > 1}
          <p class="muted">
            You're sharing with {node.relationship.person.name}, not one machine — these work to
            any of their {partner?.nodes.length} devices.
          </p>
        {/if}
        <ul class="grants">
          {#each grants as { node: holder, grant: g } (g.id)}
            <li>
              <span class="g-dot" style="background: {mediaColor(g.media)}"></span>
              <span class="g-label">{g.label || `${g.role} ${MEDIA[g.media].label}`}</span>
              <button class="revoke" title="Remove" onclick={() => app.revokeGrant(holder.id, g.id)}>✕</button>
            </li>
          {/each}
        </ul>
        {#if addingGrant}
          <div class="presets">
            {#each availablePresets as p}
              <button class="preset" onclick={() => addGrant(p)}>
                <span class="g-dot" style="background: {mediaColor(p.media)}"></span>
                {p.label}
              </button>
            {/each}
            {#if availablePresets.length === 0}
              <p class="muted">They can already do everything in the presets.</p>
            {/if}
          </div>
        {/if}
        {#if node.claimable || (node.owner && app.isMe(node.owner))}
          <button class="linklike" onclick={claimThis}>This is actually my own device →</button>
        {/if}
      {:else}
        <p class="muted own-note">
          {node.kind === "this" ? "This is you." : "Yours — it connects freely with everything else you own."}
        </p>
        {#if app.isFleetMember(node.id)}
          <button class="linklike" onclick={() => app.openSettings("fleet")}>🔗 Part of your fleet — see the shared key →</button>
        {/if}
        {#if node.kind === "this"}
          <!-- The other side of claiming: this device offering *itself*. When
               it's on, a prominent banner makes the hand-off obvious — you
               finish the claim from a machine that already owns your stuff. -->
          {#if node.claimable}
            <div class="offer-banner">
              <span class="offer-glyph" aria-hidden="true">📡</span>
              <div>
                <div class="offer-title">This device is offering itself for adoption</div>
                <div class="offer-sub">
                  On a machine you already own, open this device on the graph and
                  choose <b>Claim</b> — they'll link under one shared key.
                </div>
              </div>
            </div>
          {/if}
          <label class="adopt">
            <input
              type="checkbox"
              checked={node.claimable ?? false}
              onchange={(e) => app.setLocalClaimable(e.currentTarget.checked)}
            />
            <span>{node.claimable ? "Offering this device for adoption" : "Offer this device so another of mine can adopt it"}</span>
          </label>
        {:else}
          <button class="linklike" onclick={makeShared}>Actually, I'm sharing this with someone →</button>
        {/if}
      {/if}
    </section>

    {#if meshonly}
      <!-- Nothing more to show for a non-AllMyStuff device. -->
    {:else}

    <!-- Live connections -->
    {#if connections.length}
      <section class="block">
        <h4>Connected now</h4>
        <ul class="conns">
          {#each connections as c (c.id)}
            <li>
              <span class="g-dot" style="background: {mediaColor(c.media)}"></span>
              <span class="c-label">{c.label}</span>
              <button class="revoke" title="Disconnect" onclick={() => app.disconnect(c.id)}>✕</button>
            </li>
          {/each}
        </ul>
      </section>
    {/if}

    <!-- Re-scan this machine's hardware — sits right above its own "Its
         stuff" list because a fresh scan is what fills that list. Only the
         local device can be scanned on demand; a remote's stuff comes from
         its presence advert, so the button is gated to "this". -->
    {#if node.kind === "this"}
      <button class="btn small rescan" onclick={() => app.hydrateFromBackend()}>↻ Re-scan this machine</button>
    {/if}

    <!-- Capabilities — folded by default, and only for devices in your
         fleet: a mesh peer that isn't yours has nothing here for you to
         wire, so "Its stuff" shows for your own / owned / co-fleet machines
         only. -->
    {#if inMyFleet}
    <section class="block">
      <button
        class="stuff-toggle"
        onclick={() => (stuffOpen = !stuffOpen)}
        aria-expanded={stuffOpen}
      >
        <span class="stuff-chevron" class:open={stuffOpen} aria-hidden="true">▸</span>
        <h4 class="stuff-title">Its stuff</h4>
        <span class="stuff-count">{caps.length} thing{caps.length === 1 ? "" : "s"}</span>
      </button>
      {#if stuffOpen}
      {#each grouped as [media, list]}
        <div class="cap-group">
          <div class="cap-group-head" style="color: {mediaColor(media)}">
            {MEDIA[media].icon} {MEDIA[media].label}
          </div>
          {#each byFlow(list) as cluster (cluster.key)}
            <div class="flow-cluster" class:receives={cluster.key === "receives"}>
              <div class="flow-head">
                <span class="flow-arrow">{cluster.arrow}</span>
                {cluster.label}
              </div>
              {#each cluster.items as c (c.id)}
                <div class="cap" class:is-default={c.default}>
                  <span class="cap-icon">{originIcon(c.origin, c.media)}</span>
                  <div class="cap-id">
                    <div class="cap-label">
                      {c.label}
                      {#if c.default}<span class="def" title="This category's current default">default</span>{/if}
                    </div>
                  </div>
                  <button
                    class="connect-dot"
                    style="--mc: {mediaColor(c.media)}"
                    title="Connect this somewhere"
                    onclick={() => app.startCapConnect(c.id)}
                    aria-label="Connect {c.label}"
                  >
                    ⟶
                  </button>
                </div>
              {/each}
            </div>
          {/each}
        </div>
      {/each}
      {/if}
    </section>

    <!-- Its sites — the services this machine exposes over the mesh proxy.
         For your own machine, and any co-owned fleet member, you can expose /
         rename / stop them right here (remotely for a fleet member). -->
    {#if isAppNode(node)}
    <section class="block">
      <button
        class="stuff-toggle"
        onclick={() => (sitesOpen = !sitesOpen)}
        aria-expanded={sitesOpen}
      >
        <span class="stuff-chevron" class:open={sitesOpen} aria-hidden="true">▸</span>
        <h4 class="stuff-title">Its sites</h4>
        <span class="stuff-count">
          {deviceSites.filter((s) => app.deviceIsExposed(node.id, s.id)).length} exposed
        </span>
      </button>
      {#if sitesOpen}
        {#if deviceSites.length === 0}
          <p class="sites-empty">
            {app.isMe(node.id)
              ? "No listening services found."
              : "No services reported — it may be offline or older."}
          </p>
        {:else}
          <ul class="dsites">
            {#each deviceSites as svc (svc.id)}
              {@const exposed = app.deviceIsExposed(node.id, svc.id)}
              <li class="dsite" class:lit={exposed}>
                <span class="ds-icon" aria-hidden="true">{siteIcon(svc.scheme)}</span>
                <div class="ds-main">
                  <div class="ds-name">{svc.name}<span class="ds-port">:{svc.port}</span></div>
                  <div class="ds-sub">
                    {#if svc.loopback}<span class="tag">local-only</span>{/if}
                    {#if svc.process}{svc.process}{/if}
                  </div>
                </div>
                <div class="ds-controls">
                  <input
                    class="ds-in"
                    placeholder={app.defaultSiteName(svc)}
                    value={exposeNameFor(svc)}
                    title="Name your fleet sees this site under"
                    oninput={(e) => (exposeNames[svc.id] = (e.currentTarget as HTMLInputElement).value)}
                    onkeydown={(e) => {
                      if (e.key === "Enter" && exposed) app.exposeOnDevice(node.id, svc.id, exposeNameFor(svc));
                    }}
                    onblur={() => exposed && app.exposeOnDevice(node.id, svc.id, exposeNameFor(svc))}
                  />
                  {#if exposed}
                    <button class="btn small ghost" onclick={() => app.unexposeOnDevice(node.id, svc.id)}>Stop</button>
                  {:else}
                    <button class="btn small primary" onclick={() => app.exposeOnDevice(node.id, svc.id, exposeNameFor(svc))}>Expose</button>
                  {/if}
                </div>
              </li>
            {/each}
          </ul>
        {/if}
      {/if}
    </section>
    {/if}
    {/if}
    {/if}
      </div>
    {/if}
  </aside>
{/if}

<style>
  .sites-empty {
    font-size: 0.78rem;
    color: var(--ink-faint);
    margin: 0.3rem 0 0;
  }
  .dsites {
    list-style: none;
    margin: 0.3rem 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
  }
  .dsite {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.4rem;
    background: var(--surface-2);
    border-radius: var(--r-sm);
    padding: 0.4rem 0.45rem;
  }
  .dsite.lit {
    box-shadow: inset 0 0 0 1px var(--accent);
  }
  .ds-icon {
    font-size: 0.95rem;
    flex-shrink: 0;
  }
  .ds-main {
    flex: 1;
    min-width: 6rem;
  }
  .ds-name {
    font-size: 0.82rem;
    font-weight: 600;
  }
  .ds-port {
    color: var(--ink-faint);
    font-weight: 500;
    margin-left: 0.2rem;
    font-family: var(--mono);
    font-size: 0.76rem;
  }
  .ds-sub {
    font-size: 0.68rem;
    color: var(--ink-faint);
    display: flex;
    align-items: center;
    gap: 0.3rem;
  }
  .ds-controls {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    flex: 1 1 100%;
  }
  .ds-in {
    flex: 1;
    min-width: 4rem;
    border: 1px solid var(--line-strong);
    border-radius: var(--r-sm);
    padding: 0.25rem 0.4rem;
    font-size: 0.74rem;
    font-family: inherit;
    background: var(--surface);
    color: var(--ink);
  }
  .tag {
    background: var(--surface);
    border: 1px solid var(--line);
    border-radius: var(--r-pill);
    padding: 0 0.32rem;
    font-size: 0.62rem;
    font-weight: 700;
    color: var(--ink-faint);
  }

  /* A real sidebar: it shares the stage's flex row, so selecting a node
     reflows the graph to the left instead of covering it. Width is set
     inline (resizable + persisted); this is just the first-paint fallback. */
  .drawer {
    position: relative;
    flex-shrink: 0;
    height: 100%;
    width: 24rem;
    max-width: 92vw;
    background: var(--surface);
    border-left: 1px solid var(--line);
    box-shadow: var(--shadow-lg);
    overflow: hidden;
    z-index: 20;
    animation: slidein 0.16s ease;
  }
  .drawer.collapsed {
    width: 2.75rem;
  }
  .drawer.resizing {
    user-select: none;
  }
  .drawer-body {
    height: 100%;
    overflow-y: auto;
    padding: 1rem 1.1rem 2rem;
  }
  /* The grab edge — a hair-line that lights up on hover/drag. */
  .resizer {
    position: absolute;
    left: 0;
    top: 0;
    height: 100%;
    width: 8px;
    cursor: ew-resize;
    z-index: 5;
    touch-action: none;
  }
  .resizer::after {
    content: "";
    position: absolute;
    left: 3px;
    top: 0;
    width: 2px;
    height: 100%;
    background: transparent;
    transition: background 0.12s ease;
  }
  .resizer:hover::after,
  .drawer.resizing .resizer::after {
    background: var(--accent);
  }
  /* Collapsed: a thin rail with just the affordances to come back. */
  .rail {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.7rem;
    height: 100%;
    padding: 0.7rem 0;
  }
  .rail-avatar {
    font-size: 1.35rem;
    line-height: 1;
  }
  .rail-btn {
    border: none;
    background: var(--surface-2);
    color: var(--ink-soft);
    width: 1.9rem;
    height: 1.9rem;
    border-radius: 50%;
    font-size: 0.85rem;
    cursor: pointer;
  }
  .rail-btn:hover {
    background: var(--line-strong);
    color: var(--ink);
  }
  @keyframes slidein {
    from {
      transform: translateX(20px);
      opacity: 0;
    }
  }
  .head {
    display: flex;
    align-items: flex-start;
    gap: 0.6rem;
    margin-bottom: 0.9rem;
  }
  .avatar {
    font-size: 1.7rem;
    line-height: 1;
  }
  .title {
    flex: 1;
    min-width: 0;
  }
  .name {
    font-weight: 700;
    font-size: 1.1rem;
  }
  .kindline {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    margin-top: 0.2rem;
    flex-wrap: wrap;
  }
  .pill {
    font-size: 0.68rem;
    font-weight: 650;
    padding: 0.1rem 0.5rem;
    border-radius: var(--r-pill);
  }
  .pill.mine {
    background: var(--ok-soft);
    color: var(--ok);
  }
  .pill.guest {
    background: var(--bronze-soft);
    color: var(--bronze);
  }
  .pill.soft {
    background: var(--surface-2);
    color: var(--ink-soft);
  }
  .pill.theirs {
    background: var(--violet-soft);
    color: var(--violet);
  }
  .pill.fleet {
    background: var(--accent-soft);
    color: var(--accent-ink);
  }
  .netline {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.25rem;
    margin-top: 0.3rem;
  }
  .netline-k {
    font-size: 0.66rem;
    color: var(--ink-faint);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .net-chip {
    font-size: 0.64rem;
    font-weight: 650;
    background: var(--violet-soft);
    border: 1px solid oklch(0.62 0.2 292 / 0.35);
    color: var(--violet);
    border-radius: var(--r-pill);
    padding: 0.04rem 0.4rem;
  }
  /* The claim call-to-action — a brand-accent card that leads the drawer for
     an adoptable device, so claiming reads as the headline action it is. */
  .claim-card {
    border: 1.5px solid var(--accent);
    background: var(--accent-soft);
    border-radius: var(--r-md);
    padding: 0.75rem 0.8rem;
    margin-top: 0.2rem;
  }
  .claim-card-head {
    display: flex;
    align-items: center;
    gap: 0.55rem;
  }
  .claim-glyph {
    display: grid;
    place-items: center;
    width: 1.9rem;
    height: 1.9rem;
    border-radius: 50%;
    background: var(--accent);
    color: var(--bg);
    font-weight: 800;
    font-size: 1.1rem;
    line-height: 1;
    flex-shrink: 0;
  }
  .claim-card-title {
    font-weight: 750;
    font-size: 0.95rem;
  }
  .claim-card-sub {
    font-size: 0.74rem;
    color: var(--accent-ink);
    margin-top: 0.05rem;
  }
  .claim-card-what {
    font-size: 0.78rem;
    line-height: 1.45;
    color: var(--ink-soft);
    margin: 0.6rem 0 0.7rem;
  }
  .claim-go {
    width: 100%;
    justify-content: center;
  }
  .claim-card .linklike {
    display: block;
    width: 100%;
    text-align: center;
    color: var(--ink-soft);
  }
  /* This device offering itself — a calm status banner (it's a state, not a
     call to act here), so the in-progress hand-off is impossible to miss. */
  .offer-banner {
    display: flex;
    align-items: flex-start;
    gap: 0.55rem;
    border: 1px solid var(--accent);
    background: var(--accent-soft);
    border-radius: var(--r-md);
    padding: 0.65rem 0.75rem;
    margin: 0.2rem 0 0.5rem;
  }
  .offer-glyph {
    font-size: 1.2rem;
    line-height: 1.2;
    flex-shrink: 0;
  }
  .offer-title {
    font-weight: 700;
    font-size: 0.86rem;
  }
  .offer-sub {
    font-size: 0.76rem;
    line-height: 1.45;
    color: var(--ink-soft);
    margin-top: 0.15rem;
  }
  .state {
    font-size: 0.7rem;
    color: var(--ink-faint);
  }
  .state.on {
    color: var(--ok);
  }
  .x {
    border: none;
    background: var(--surface-2);
    color: var(--ink-soft);
    width: 1.9rem;
    height: 1.9rem;
    border-radius: 50%;
    font-size: 0.8rem;
  }
  .x:hover {
    background: var(--line-strong);
  }
  .stats {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0.5rem;
    margin-bottom: 0.8rem;
  }
  .stat {
    background: var(--surface-2);
    border-radius: var(--r-sm);
    padding: 0.5rem 0.6rem;
  }
  .stat span {
    display: block;
    font-size: 0.66rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--ink-faint);
  }
  .stat b {
    font-size: 0.86rem;
  }
  .rescan {
    margin-bottom: 0.8rem;
  }
  .block {
    border-top: 1px solid var(--line);
    padding-top: 0.85rem;
    margin-top: 0.5rem;
  }
  .block-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  h4 {
    margin: 0 0 0.55rem;
    font-size: 0.82rem;
    color: var(--ink-soft);
    font-weight: 650;
  }
  .muted {
    color: var(--ink-faint);
    font-size: 0.8rem;
    margin: 0.2rem 0 0.5rem;
    line-height: 1.45;
  }
  .own-note {
    margin-top: 0;
  }
  .grants,
  .conns {
    list-style: none;
    margin: 0.4rem 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
  }
  .grants li,
  .conns li {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    background: var(--surface-2);
    border-radius: var(--r-sm);
    padding: 0.4rem 0.55rem;
    font-size: 0.82rem;
  }
  .g-dot {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    flex-shrink: 0;
  }
  .g-label,
  .c-label {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .revoke {
    border: none;
    background: transparent;
    color: var(--ink-faint);
    width: 1.4rem;
    height: 1.4rem;
    border-radius: 50%;
    font-size: 0.72rem;
  }
  .revoke:hover {
    background: var(--danger-soft);
    color: var(--danger);
  }
  .presets {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    margin: 0.3rem 0 0.5rem;
  }
  .preset {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    text-align: left;
    border: 1px dashed var(--line-strong);
    background: var(--surface);
    border-radius: var(--r-sm);
    padding: 0.45rem 0.55rem;
    font-size: 0.82rem;
    color: var(--ink);
  }
  .preset:hover {
    border-color: var(--accent);
    background: var(--accent-soft);
  }
  .linklike {
    border: none;
    background: none;
    color: var(--accent-ink);
    font-size: 0.78rem;
    padding: 0.3rem 0;
    cursor: pointer;
  }
  .linklike:hover {
    text-decoration: underline;
  }
  /* The "Its stuff" fold: a full-width header row that reads as a count
     until expanded. */
  .stuff-toggle {
    display: flex;
    align-items: center;
    gap: 0.45rem;
    width: 100%;
    border: none;
    background: none;
    padding: 0.1rem 0 0.45rem;
    text-align: left;
    cursor: pointer;
  }
  .stuff-toggle:hover .stuff-title {
    color: var(--accent-ink);
  }
  .stuff-title {
    margin: 0;
  }
  .stuff-chevron {
    font-size: 0.7rem;
    color: var(--ink-faint);
    transition: transform 0.12s ease;
  }
  .stuff-chevron.open {
    transform: rotate(90deg);
  }
  .stuff-count {
    margin-left: auto;
    font-size: 0.68rem;
    font-weight: 650;
    color: var(--ink-soft);
    background: var(--surface-2);
    border-radius: var(--r-pill);
    padding: 0.1rem 0.5rem;
  }
  .cap-group {
    margin-bottom: 0.7rem;
  }
  .cap-group-head {
    font-size: 0.72rem;
    font-weight: 700;
    letter-spacing: 0.02em;
    margin-bottom: 0.3rem;
  }
  .cap {
    display: flex;
    align-items: center;
    gap: 0.55rem;
    padding: 0.4rem 0.2rem;
  }
  .cap-icon {
    font-size: 1.1rem;
    width: 1.5rem;
    text-align: center;
  }
  .cap-id {
    flex: 1;
    min-width: 0;
  }
  .cap-label {
    font-size: 0.86rem;
    font-weight: 550;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }
  .def {
    flex-shrink: 0;
    font-size: 0.6rem;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.03em;
    color: var(--warn);
    background: var(--warn-soft);
    border-radius: var(--r-pill);
    padding: 0.05rem 0.34rem;
  }
  .cap.is-default .cap-icon {
    filter: drop-shadow(0 0 0.5px var(--warn));
  }
  /* Sends vs Receives clusters inside a media group — visually distinct
     columns of direction, so "what it gives" never reads as "what it takes". */
  .flow-cluster {
    margin: 0.15rem 0 0.35rem;
    padding-left: 0.45rem;
    border-left: 2px solid oklch(0.8 0.17 150 / 0.35);
  }
  .flow-cluster.receives {
    border-left-color: oklch(0.74 0.085 72 / 0.45);
  }
  .flow-head {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    font-size: 0.66rem;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--ink-faint);
    margin: 0.25rem 0 0.05rem;
  }
  .flow-arrow {
    font-size: 0.8rem;
    line-height: 1;
  }
  .connect-dot {
    flex-shrink: 0;
    width: 1.9rem;
    height: 1.9rem;
    border-radius: 50%;
    border: 1.5px solid var(--mc);
    color: var(--mc);
    background: var(--surface);
    font-size: 0.85rem;
    display: grid;
    place-items: center;
    transition: transform 0.08s ease, background 0.12s ease;
  }
  .connect-dot:hover {
    background: var(--mc);
    color: var(--bg);
    transform: scale(1.08);
  }
  .console-open {
    width: 100%;
    margin-bottom: 0.8rem;
  }
  .adopt {
    display: flex;
    align-items: flex-start;
    gap: 0.5rem;
    margin-top: 0.5rem;
    font-size: 0.8rem;
    color: var(--ink-soft);
    line-height: 1.4;
    cursor: pointer;
  }
  .adopt input {
    margin-top: 0.15rem;
  }
  .devid {
    font-size: 0.72rem;
    color: var(--ink-faint);
    margin-top: 0.4rem;
  }
</style>
