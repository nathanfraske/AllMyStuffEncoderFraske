<script lang="ts">
  import { app } from "../store.svelte";
  import {
    MEDIA,
    displayName,
    isAppNode,
    originIcon,
    flowWord,
    humanBytes,
    mediaColor,
    type Capability,
    type Grant,
    type GrantRole,
    type MediaKind,
  } from "../types";

  const node = $derived(app.selectedNode);
  const caps = $derived(node ? app.capsOf(node.id) : []);
  const shared = $derived(node?.relationship.kind === "shared");
  // A device on the mesh that isn't running AllMyStuff: nothing to wire.
  const meshonly = $derived(!!node && !isAppNode(node));
  // This device declares an owner that isn't us — it can't be adopted.
  const ownedByOther = $derived(!!node?.owner && node.owner !== app.localId);
  // A remote machine you can open a console session on.
  const isRemoteApp = $derived(!!node && node.kind !== "this" && !meshonly);

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

  // Routes touching this node, with the far end + direction.
  const connections = $derived.by(() => {
    if (!node) return [];
    const out: { id: string; label: string; media: MediaKind; dir: "out" | "in" }[] = [];
    for (const r of app.catalog.routes) {
      const from = app.capability(r.from);
      const to = app.capability(r.to);
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

  const grants = $derived(
    node && node.relationship.kind === "shared" ? node.relationship.grants : [],
  );

  const availablePresets = $derived(
    PRESETS.filter((p) => !grants.some((g) => g.media === p.media && g.role === p.role)),
  );

  let addingGrant = $state(false);

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
    app.setRelationship(node.id, {
      kind: "shared",
      person: { id: `person:${node.id}`, name: node.label },
      grants: [],
    });
  }
  /** Adopt this device — gated: only takes if it's in claim mode (Task 4). */
  function claimThis() {
    if (!node) return;
    app.claim(node.id);
  }
</script>

{#if node}
  <aside class="drawer" aria-label="{displayName(node)} details">
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
            <span class="pill soft">{node.claimable ? "claimable" : "unclaimed"}</span>
          {:else}
            <span class="pill mine">yours</span>
          {/if}
          {#if app.isFleetMember(node.id)}<span class="pill fleet" title="In your owned fleet (shared key)">🔗 fleet</span>{/if}
          <span class="state" class:on={node.online}>{node.online ? "online" : "offline"}</span>
        </div>
      </div>
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

    {#if node.kind === "this"}
      <button class="btn small rescan" onclick={() => app.hydrateFromBackend()}>↻ Re-scan this machine</button>
    {/if}

    <!-- Open a remote console session: the pikvm-style handle for this
         machine's screen, audio passthrough and control. -->
    {#if isRemoteApp && (node.relationship.kind === "mine" || node.relationship.kind === "shared")}
      <button class="btn primary console-open" onclick={() => app.openConsole(node.id)}>
        🖥 Open console
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
          <p class="muted">
            This device was started in <b>claim mode</b> — it's offering itself
            for adoption. Make it one of yours, or mark it shared.
          </p>
          <div class="claim">
            <button class="btn small primary" onclick={claimThis}>Claim this device</button>
            <button class="linklike" onclick={makeShared}>I'm sharing with someone →</button>
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
        {/if}
        <ul class="grants">
          {#each grants as g (g.id)}
            <li>
              <span class="g-dot" style="background: {mediaColor(g.media)}"></span>
              <span class="g-label">{g.label || `${g.role} ${MEDIA[g.media].label}`}</span>
              <button class="revoke" title="Remove" onclick={() => app.revokeGrant(node.id, g.id)}>✕</button>
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
        {#if node.claimable || node.owner === app.localId}
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
          <!-- Hand this machine off: put it into claim mode so another of
               your devices can adopt it (Task 4). -->
          <label class="adopt">
            <input
              type="checkbox"
              checked={node.claimable ?? false}
              onchange={(e) => app.setLocalClaimable(e.currentTarget.checked)}
            />
            <span>Allow another of my devices to adopt this one</span>
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

    <!-- Capabilities -->
    <section class="block">
      <h4>Its stuff</h4>
      {#each grouped as [media, list]}
        <div class="cap-group">
          <div class="cap-group-head" style="color: {mediaColor(media)}">
            {MEDIA[media].icon} {MEDIA[media].label}
          </div>
          {#each list as c (c.id)}
            <div class="cap" class:is-default={c.default}>
              <span class="cap-icon">{originIcon(c.origin, c.media)}</span>
              <div class="cap-id">
                <div class="cap-label">
                  {c.label}
                  {#if c.default}<span class="def" title="This category's current default">default</span>{/if}
                </div>
                <div class="cap-flow">{flowWord(c.flow)}</div>
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
    </section>
    {/if}
  </aside>
{/if}

<style>
  .drawer {
    position: absolute;
    top: 0;
    right: 0;
    height: 100%;
    width: 24rem;
    max-width: 92vw;
    background: var(--surface);
    border-left: 1px solid var(--line);
    box-shadow: var(--shadow-lg);
    overflow-y: auto;
    padding: 1rem 1.1rem 2rem;
    z-index: 20;
    animation: slidein 0.16s ease;
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
    background: #e7f6ef;
    color: #137a52;
  }
  .pill.guest {
    background: #fdedd2;
    color: #97631a;
  }
  .pill.soft {
    background: var(--surface-2);
    color: var(--ink-soft);
  }
  .pill.fleet {
    background: var(--accent-soft);
    color: var(--accent-ink);
  }
  .claim {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    margin-top: 0.4rem;
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
    background: #fdeaee;
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
    color: #97631a;
    background: #fdedd2;
    border-radius: var(--r-pill);
    padding: 0.05rem 0.34rem;
  }
  .cap.is-default .cap-icon {
    filter: drop-shadow(0 0 0.5px #e0a13a);
  }
  .cap-flow {
    font-size: 0.72rem;
    color: var(--ink-faint);
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
    color: #fff;
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
