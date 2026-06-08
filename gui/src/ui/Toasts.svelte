<script lang="ts">
  import { app } from "../store.svelte";
</script>

<div class="toasts" aria-live="polite">
  {#each app.toasts as t (t.id)}
    <div class="toast {t.kind}">
      <span class="ic">{t.kind === "ok" ? "✓" : t.kind === "warn" ? "!" : "›"}</span>
      {t.text}
    </div>
  {/each}
</div>

<style>
  .toasts {
    position: fixed;
    bottom: 1.2rem;
    left: 50%;
    transform: translateX(-50%);
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    z-index: 80;
    align-items: center;
    pointer-events: none;
  }
  .toast {
    background: var(--ink);
    color: #fff;
    padding: 0.5rem 0.9rem;
    border-radius: var(--r-pill);
    font-size: 0.84rem;
    font-weight: 550;
    box-shadow: var(--shadow-lg);
    display: flex;
    align-items: center;
    gap: 0.5rem;
    animation: pop 0.16s ease;
  }
  @keyframes pop {
    from {
      transform: translateY(8px);
      opacity: 0;
    }
  }
  .ic {
    display: grid;
    place-items: center;
    width: 1.15rem;
    height: 1.15rem;
    border-radius: 50%;
    font-size: 0.72rem;
    background: rgba(255, 255, 255, 0.18);
  }
  .toast.ok .ic {
    background: var(--ok);
  }
  .toast.warn .ic {
    background: var(--warn);
  }
</style>
