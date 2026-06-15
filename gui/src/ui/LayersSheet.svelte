<script lang="ts">
  // "How it works" sheet — the full-page connection-layers graphic, opened
  // from the top-bar "?" button. It answers the four questions people actually
  // ask before they trust the app with their machines: what the mesh is, how a
  // fleet differs, what the Signal/STUN/TURN servers are for, and how sharing
  // fits. The artwork is the self-contained SVG in public/ (the same one the
  // website ships), so the picture stays identical across app and site.
  let { onclose }: { onclose: () => void } = $props();
</script>

<svelte:window onkeydown={(e) => e.key === "Escape" && onclose()} />

<div class="scrim">
  <button class="backdrop" onclick={onclose} aria-label="Close"></button>
  <div class="popup" role="dialog" aria-modal="true" aria-label="How AllMyStuff connects" tabindex="-1">
    <header class="head">
      <div class="head-text">
        <div class="title">How AllMyStuff connects</div>
        <div class="sub">Venue and mesh, your fleet, and sharing — you connect where both venue and mesh line up.</div>
      </div>
      <button class="x" onclick={onclose} aria-label="Close">✕</button>
    </header>
    <div class="art">
      <img
        src="/connection-layers.svg"
        alt="How AllMyStuff connects: a venue is a signal layer (Signal/STUN/TURN) that hosts meshes; a mesh is your channel that travels with you between venues; you can see another person only where you share both the same venue and the same mesh; your fleet is your own devices; and sharing grants a specific thing to the owner so any of their devices can use it."
      />
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
    width: min(1080px, 94vw);
    max-height: 92vh;
    display: flex;
    flex-direction: column;
    background: var(--surface);
    border: 1px solid var(--line);
    border-radius: var(--r-lg);
    box-shadow: var(--shadow-lg);
    overflow: hidden;
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
    padding: 0.9rem 1.2rem;
    border-bottom: 1px solid var(--line);
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
  /* The art scrolls inside the sheet — the graphic is a tall portrait poster,
     so on shorter windows it stays fully readable by scrolling rather than
     shrinking past legibility. */
  .art {
    overflow: auto;
    padding: 1.1rem;
  }
  .art img {
    display: block;
    width: 100%;
    height: auto;
    border-radius: var(--r-md);
  }
</style>
