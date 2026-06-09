<script lang="ts">
  // Devices pane — every machine you can see, and which network(s) each is on.
  // The point: you're joined to however many networks, and a device may be on
  // only some of them. This makes the overlap explicit rather than pretending
  // it's one flat mesh.
  import { app } from "../../store.svelte";
  import { displayName, isAppNode } from "../../types";
  import type { MeshNode } from "../../types";

  // This device first, then the rest by name.
  const devices = $derived(
    [...app.catalog.nodes].sort((a, b) => {
      const rank = (n: MeshNode) => (n.kind === "this" ? 0 : 1);
      return rank(a) - rank(b) || a.label.localeCompare(b.label);
    }),
  );

  function relLabel(n: MeshNode): { text: string; cls: string } {
    if (!isAppNode(n)) return { text: "not on AllMyStuff", cls: "soft" };
    if (n.relationship.kind === "shared") return { text: "shared", cls: "guest" };
    if (n.relationship.kind === "unclaimed") return { text: n.claimable ? "claimable" : "unclaimed", cls: "soft" };
    return { text: n.kind === "this" ? "this device" : "yours", cls: "mine" };
  }
</script>

<div class="devices">
  <p class="lead">
    Everything you can see across your networks. A device is only reachable on a
    network you both share — so the chips show where each one lives.
  </p>

  <ul class="list">
    {#each devices as n (n.id)}
      {@const rel = relLabel(n)}
      <li>
        <span class="avatar">{n.kind === "this" ? "💻" : isAppNode(n) ? "🖥" : "📡"}</span>
        <div class="id">
          <div class="name">{displayName(n)}</div>
          <div class="meta">
            <span class="pill {rel.cls}">{rel.text}</span>
            <span class="state" class:on={n.online}>{n.online ? "online" : "offline"}</span>
            {#if app.isFleetMember(n.id)}<span class="pill fleet">🔗 fleet</span>{/if}
          </div>
        </div>
        <div class="nets">
          {#if n.networks && n.networks.length}
            {#each n.networks as net}<span class="net-chip">{net}</span>{/each}
          {:else}
            <span class="net-chip none">—</span>
          {/if}
        </div>
      </li>
    {/each}
    {#if devices.length === 0}
      <li class="empty">No devices yet.</li>
    {/if}
  </ul>
</div>

<style>
  .devices {
    padding-top: 0.6rem;
  }
  .lead {
    font-size: 0.8rem;
    color: var(--ink-soft);
    line-height: 1.45;
    margin: 0 0 0.7rem;
  }
  .list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }
  .list li {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    background: var(--surface-2);
    border-radius: var(--r-sm);
    padding: 0.5rem 0.6rem;
  }
  .avatar {
    font-size: 1.2rem;
  }
  .id {
    flex: 1;
    min-width: 0;
  }
  .name {
    font-size: 0.88rem;
    font-weight: 600;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .meta {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    margin-top: 0.15rem;
  }
  .pill {
    font-size: 0.62rem;
    font-weight: 700;
    padding: 0.05rem 0.4rem;
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
    background: var(--surface);
    color: var(--ink-soft);
    border: 1px solid var(--line-strong);
  }
  .pill.fleet {
    background: var(--accent-soft);
    color: var(--accent-ink);
  }
  .state {
    font-size: 0.68rem;
    color: var(--ink-faint);
  }
  .state.on {
    color: var(--ok);
  }
  .nets {
    display: flex;
    flex-wrap: wrap;
    gap: 0.25rem;
    justify-content: flex-end;
    max-width: 45%;
  }
  .net-chip {
    font-size: 0.66rem;
    font-weight: 600;
    background: var(--surface);
    border: 1px solid var(--line-strong);
    color: var(--ink-soft);
    border-radius: var(--r-pill);
    padding: 0.1rem 0.45rem;
  }
  .net-chip.none {
    color: var(--ink-faint);
    border-style: dashed;
  }
  .empty {
    justify-content: center;
    color: var(--ink-faint);
    font-size: 0.82rem;
  }
</style>
