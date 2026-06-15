<script lang="ts">
  // The Sites tab of the bottom-left sidebar. Two halves:
  //
  //  * **This machine** — the TCP services the scan found this box listening
  //    on, each with an Expose toggle. Exposing one advertises it to the
  //    mesh so your fleet can reach it through the reverse proxy; nothing is
  //    exposed by default (a local dev server never auto-broadcasts).
  //  * **Reachable machines** — your fleet's machines that expose sites,
  //    grouped per machine. Map one to a local port (direct when free, else
  //    remapped) and open it: a web site in the browser, a bare TCP service
  //    by copying its localhost:<port> address for whatever client speaks it.
  import { app } from "../store.svelte";
  import { displayName, siteIcon, siteIsWeb, type SiteAdvert } from "../types";

  function portLabel(svc: { port: number }): string {
    return `:${svc.port}`;
  }
</script>

<div class="sites">
  <!-- This machine's services, with expose toggles. -->
  <section class="block">
    <h5 class="block-head">This machine</h5>
    {#if app.myListening.length === 0}
      <p class="hint faint">
        No listening services found{app.backendConnected ? "" : " (demo)"} — start a local
        server and it'll appear here to expose.
      </p>
    {:else}
      <ul class="list">
        {#each app.myListening as svc (svc.id)}
          {@const exposed = app.isExposed(svc.id)}
          <li class="row">
            <span class="s-icon" aria-hidden="true">{siteIcon(svc.scheme)}</span>
            <span class="s-main">
              <span class="s-name">{svc.name}<span class="s-port">{portLabel(svc)}</span></span>
              <span class="s-sub">
                {#if svc.loopback}<span class="tag">local-only</span>{/if}
                {#if svc.process}{svc.process}{/if}
              </span>
            </span>
            <button
              class="btn small"
              class:primary={!exposed}
              class:ghost={exposed}
              title={exposed ? "Stop advertising this service to your mesh" : "Advertise this service so your fleet can reach it"}
              onclick={() => app.toggleExpose(svc.id)}
            >
              {exposed ? "✓ Exposed" : "Expose"}
            </button>
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  <!-- Sites exposed by your other machines. -->
  <section class="block">
    <h5 class="block-head">Around your fleet</h5>
    {#if app.sitesByMachine.length === 0}
      <p class="hint faint">
        No sites from your other machines yet. Expose a service on one of them, or claim more
        devices into your fleet.
      </p>
    {:else}
      {#each app.sitesByMachine as group (group.node.id)}
        <div class="machine">
          <div class="machine-head">
            <span class="m-dot" class:on={group.node.online}></span>
            {displayName(group.node)}
          </div>
          <ul class="list">
            {#each group.sites as site (site.id)}
              {@const mapping = app.siteMappingFor(group.node.id, site.id)}
              <li class="row">
                <span class="s-icon" aria-hidden="true">{siteIcon(site.scheme)}</span>
                <span class="s-main">
                  <span class="s-name">{site.label}<span class="s-port">:{site.port}</span></span>
                  {#if mapping}
                    <span class="s-sub mapped" title="Reachable locally at this address">
                      {app.siteUrl(mapping)}
                    </span>
                  {:else if site.loopback}
                    <span class="s-sub"><span class="tag">local-only</span></span>
                  {/if}
                </span>
                {#if mapping}
                  <button
                    class="btn small primary"
                    title={siteIsWeb(site) ? "Open in your browser" : "Copy its local address"}
                    onclick={() => app.openSite(mapping)}
                  >
                    {siteIsWeb(site) ? "Open" : "Copy"}
                  </button>
                  <button
                    class="btn small ghost s-unmap"
                    title="Unmap — stop proxying it here"
                    aria-label="Unmap"
                    onclick={() => app.unmapSite(group.node.id, site.id)}>✕</button
                  >
                {:else}
                  <button
                    class="btn small"
                    title="Map this site to a local port through the mesh proxy"
                    onclick={() => app.mapSite(group.node.id, site as SiteAdvert)}>Map</button
                  >
                {/if}
              </li>
            {/each}
          </ul>
        </div>
      {/each}
    {/if}
  </section>
</div>

<style>
  .sites {
    display: flex;
    flex-direction: column;
    gap: 0.7rem;
    max-height: 22rem;
    overflow-y: auto;
  }
  .block-head {
    margin: 0 0 0.4rem;
    font-size: 0.7rem;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--ink-faint);
  }
  .hint {
    font-size: 0.78rem;
    color: var(--ink-soft);
    margin: 0;
    line-height: 1.4;
  }
  .hint.faint {
    color: var(--ink-faint);
  }
  .machine {
    margin-bottom: 0.5rem;
  }
  .machine-head {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    font-size: 0.78rem;
    font-weight: 600;
    color: var(--ink-soft);
    margin-bottom: 0.3rem;
  }
  .m-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--line-strong);
    flex-shrink: 0;
  }
  .m-dot.on {
    background: var(--ok);
  }
  .list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 0.45rem;
    background: var(--surface-2);
    border-radius: var(--r-sm);
    padding: 0.4rem 0.4rem 0.4rem 0.5rem;
  }
  .s-icon {
    font-size: 0.95rem;
    flex-shrink: 0;
  }
  .s-main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .s-name {
    font-size: 0.82rem;
    font-weight: 600;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .s-port {
    color: var(--ink-faint);
    font-weight: 500;
    margin-left: 0.2rem;
    font-family: var(--mono);
    font-size: 0.76rem;
  }
  .s-sub {
    font-size: 0.68rem;
    color: var(--ink-faint);
    display: flex;
    align-items: center;
    gap: 0.3rem;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .s-sub.mapped {
    color: var(--accent-ink);
    font-family: var(--mono);
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
  .s-unmap {
    flex-shrink: 0;
    padding: 0.25rem 0.4rem;
    color: var(--ink-faint);
  }
  .s-unmap:hover {
    color: var(--danger);
  }
</style>
