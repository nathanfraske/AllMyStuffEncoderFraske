<script lang="ts">
  // The remote console — a pikvm-style session window for another machine.
  // A video-inputs tab bar across the top picks which of the remote's
  // sources you're looking at (its screen, its cameras); the bar underneath
  // is the handle for audio passthrough and keyboard/mouse control. It owns
  // the real routes the session runs on, so toggles here actually wire (and
  // unwire) the mesh.
  import { app } from "../store.svelte";
  import { displayName, originIcon, mediaColor, MEDIA, type Capability, type MediaKind } from "../types";

  const node = $derived(app.consoleNode);
  const inputs = $derived(node ? app.consoleVideoInputs(node.id) : []);
  const selectedId = $derived(app.consoleInput);
  const selected = $derived<Capability | null>(
    (selectedId ? app.capability(selectedId) : null) ?? null,
  );

  // Routes running between this machine and the remote — the live session.
  const sessionRoutes = $derived.by(() => {
    const remote = app.consoleNodeId;
    if (!remote) return [];
    return app.catalog.routes.filter((r) => {
      const f = app.capability(r.from);
      const t = app.capability(r.to);
      if (!f || !t) return false;
      const ends = [f.node, t.node];
      return ends.includes(remote) && ends.includes(app.localId);
    });
  });

  // The selected input streams pixels only once video transport lands; today
  // the session is wired and audio flows, and we say so plainly.
  const screenLive = $derived(
    !!selected &&
      selected.media === "display" &&
      sessionRoutes.some((r) => r.from === selected.id && r.media === "display"),
  );

  function inputIcon(c: Capability): string {
    return originIcon(c.origin, c.media);
  }
</script>

<svelte:window onkeydown={(e) => node && e.key === "Escape" && app.closeConsole()} />

{#if node}
  <div class="scrim">
    <button class="backdrop" aria-label="Close console" onclick={() => app.closeConsole()}></button>
    <div class="console" role="dialog" aria-modal="true" aria-label="Console for {displayName(node)}">
      <!-- Title bar -->
      <header class="bar">
        <div class="who">
          <span class="avatar">🖥</span>
          <div class="id">
            <div class="name">{displayName(node)}</div>
            <div class="sub">
              <span class="dot" class:on={node.online}></span>
              {node.online ? "online" : "offline"} · remote console
            </div>
          </div>
        </div>
        <!-- Video inputs tab bar -->
        <div class="inputs" role="tablist" aria-label="Video inputs">
          {#each inputs as inp (inp.id)}
            <button
              class="tab"
              class:active={inp.id === selectedId}
              role="tab"
              aria-selected={inp.id === selectedId}
              title={inp.label}
              onclick={() => app.setConsoleInput(inp.id)}
            >
              <span class="tab-icon">{inputIcon(inp)}</span>
              <span class="tab-label">{inp.label}</span>
              {#if inp.default}<span class="tab-def" title="Default input">★</span>{/if}
            </button>
          {/each}
          {#if inputs.length === 0}
            <span class="no-inputs">No video inputs advertised</span>
          {/if}
        </div>
        <button class="x" onclick={() => app.closeConsole()} aria-label="Close">✕</button>
      </header>

      <!-- Video stage -->
      <div class="stage">
        {#if selected}
          <div class="screen" style="--mc: {mediaColor(selected.media)}">
            <div class="screen-glyph">{inputIcon(selected)}</div>
            <div class="screen-title">{selected.label}</div>
            {#if selected.media === "display"}
              <div class="screen-note">
                {screenLive
                  ? "Session connected — live pixel streaming is on the way."
                  : "Connecting this machine's display…"}
              </div>
            {:else}
              <div class="screen-note">
                Camera input selected — video streaming is the next transport to land.
              </div>
            {/if}
          </div>
        {:else}
          <div class="screen empty">
            <div class="screen-glyph">🪟</div>
            <div class="screen-note">Pick a video input above to view this machine.</div>
          </div>
        {/if}
      </div>

      <!-- Control / passthrough bar -->
      <footer class="controls">
        <div class="toggles">
          <button
            class="toggle"
            class:on={app.consoleAudio}
            onclick={() => app.toggleConsoleAudio()}
            title="Hear the remote and send it your audio"
          >
            <span class="t-icon">🔊</span>
            Audio passthrough
            <span class="pip" class:lit={app.consoleAudio}></span>
          </button>
          <button
            class="toggle"
            class:on={app.consoleControl}
            onclick={() => app.toggleConsoleControl()}
            title="Send this machine's keyboard & mouse to the remote"
          >
            <span class="t-icon">⌨️</span>
            Keyboard &amp; mouse
            <span class="pip" class:lit={app.consoleControl}></span>
          </button>
        </div>

        <div class="status">
          {#each sessionRoutes as r (r.id)}
            <span class="chip" style="--mc: {mediaColor(r.media as MediaKind)}">
              <span class="chip-dot"></span>{MEDIA[r.media as MediaKind].label}
            </span>
          {/each}
          {#if sessionRoutes.length === 0}
            <span class="muted">No active links yet</span>
          {/if}
        </div>

        <button class="btn end" onclick={() => app.closeConsole()}>End session</button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .scrim {
    position: fixed;
    inset: 0;
    z-index: 60;
    display: grid;
    place-items: center;
    background: rgba(20, 18, 33, 0.55);
    backdrop-filter: blur(3px);
    padding: 1.5rem;
  }
  .backdrop {
    position: absolute;
    inset: 0;
    border: none;
    background: transparent;
    cursor: default;
  }
  .console {
    position: relative;
    z-index: 1;
    width: min(60rem, 94vw);
    height: min(40rem, 86vh);
    display: flex;
    flex-direction: column;
    background: #14121f;
    border: 1px solid #2c2740;
    border-radius: var(--r-lg);
    box-shadow: var(--shadow-lg);
    overflow: hidden;
    animation: rise 0.16s ease;
  }
  @keyframes rise {
    from {
      transform: translateY(14px) scale(0.98);
      opacity: 0;
    }
  }
  .bar {
    display: flex;
    align-items: center;
    gap: 0.8rem;
    padding: 0.5rem 0.6rem;
    background: linear-gradient(180deg, #1c1830, #14121f);
    border-bottom: 1px solid #2c2740;
    flex-shrink: 0;
  }
  .who {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex-shrink: 0;
  }
  .avatar {
    font-size: 1.3rem;
  }
  .id .name {
    font-weight: 700;
    font-size: 0.92rem;
    color: #f3f1fb;
  }
  .id .sub {
    font-size: 0.7rem;
    color: #9a93b8;
    display: flex;
    align-items: center;
    gap: 0.35rem;
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: #6b6486;
  }
  .dot.on {
    background: var(--ok);
    box-shadow: 0 0 0 3px rgba(26, 160, 109, 0.25);
  }
  .inputs {
    display: flex;
    gap: 0.3rem;
    flex: 1;
    min-width: 0;
    overflow-x: auto;
    padding: 0.1rem;
  }
  .tab {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    flex-shrink: 0;
    border: 1px solid #322c47;
    background: #1a1730;
    color: #c8c2e0;
    border-radius: var(--r-pill);
    padding: 0.32rem 0.6rem;
    font-size: 0.76rem;
    font-weight: 600;
    cursor: pointer;
    transition: border-color 0.12s ease, background 0.12s ease;
  }
  .tab:hover {
    border-color: var(--accent);
  }
  .tab.active {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }
  .tab-icon {
    font-size: 0.95rem;
  }
  .tab-label {
    max-width: 9rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tab-def {
    color: #ffd86b;
    font-size: 0.7rem;
  }
  .no-inputs {
    font-size: 0.76rem;
    color: #8b84a8;
    align-self: center;
  }
  .x {
    flex-shrink: 0;
    border: none;
    background: #241f38;
    color: #c8c2e0;
    width: 1.9rem;
    height: 1.9rem;
    border-radius: 50%;
    font-size: 0.8rem;
    cursor: pointer;
  }
  .x:hover {
    background: #322c47;
    color: #fff;
  }
  .stage {
    flex: 1;
    min-height: 0;
    display: grid;
    place-items: center;
    padding: 1rem;
    background:
      radial-gradient(1200px 400px at 50% -10%, rgba(108, 92, 231, 0.12), transparent),
      repeating-linear-gradient(0deg, #100e1a, #100e1a 2px, #12101c 2px, #12101c 4px);
  }
  .screen {
    width: 100%;
    height: 100%;
    border: 1px solid #2c2740;
    border-radius: var(--r-md);
    background: radial-gradient(900px 360px at 50% 30%, rgba(108, 92, 231, 0.1), #0c0b14);
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 0.5rem;
    text-align: center;
    box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.02);
  }
  .screen-glyph {
    font-size: 3.4rem;
    filter: drop-shadow(0 4px 12px var(--mc, rgba(108, 92, 231, 0.4)));
    opacity: 0.92;
  }
  .screen-title {
    color: #efecf9;
    font-weight: 700;
    font-size: 1.05rem;
  }
  .screen-note {
    color: #9a93b8;
    font-size: 0.82rem;
    max-width: 28rem;
    line-height: 1.45;
  }
  .controls {
    display: flex;
    align-items: center;
    gap: 0.8rem;
    padding: 0.6rem 0.7rem;
    background: #1a1730;
    border-top: 1px solid #2c2740;
    flex-shrink: 0;
  }
  .toggles {
    display: flex;
    gap: 0.4rem;
  }
  .toggle {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    border: 1px solid #322c47;
    background: #14121f;
    color: #c8c2e0;
    border-radius: var(--r-pill);
    padding: 0.4rem 0.7rem;
    font-size: 0.8rem;
    font-weight: 600;
    cursor: pointer;
    transition: border-color 0.12s ease, background 0.12s ease;
  }
  .toggle:hover {
    border-color: var(--accent);
  }
  .toggle.on {
    background: rgba(26, 160, 109, 0.18);
    border-color: var(--ok);
    color: #c7efdb;
  }
  .t-icon {
    font-size: 0.95rem;
  }
  .pip {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #4a4366;
  }
  .pip.lit {
    background: var(--ok);
    box-shadow: 0 0 0 3px rgba(26, 160, 109, 0.25);
  }
  .status {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    flex: 1;
    min-width: 0;
    overflow: hidden;
    flex-wrap: wrap;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    font-size: 0.72rem;
    font-weight: 650;
    color: #d7d2ec;
    background: #14121f;
    border: 1px solid #322c47;
    border-radius: var(--r-pill);
    padding: 0.16rem 0.5rem;
  }
  .chip-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--mc);
  }
  .muted {
    color: #79739a;
    font-size: 0.76rem;
  }
  .end {
    flex-shrink: 0;
    background: #2a1622;
    border: 1px solid #5b2740;
    color: #ffb4c8;
  }
  .end:hover {
    background: #3a1c2e;
  }
</style>
