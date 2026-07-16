<script lang="ts">
  // The terminal's mobile key strip — ConsoleKeys' little sibling. xterm.js
  // already owns the typing path (its hidden textarea takes the OS
  // keyboard's characters), so unlike the console this strip needs no ghost
  // input of its own. What it adds is everything a phone keyboard can't
  // say to a shell — Esc, Tab, Ctrl, arrows — plus the one thing xterm
  // can't do for itself on iOS: a button whose tap (a real user gesture,
  // which programmatic focus isn't) summons the OS keyboard.
  //
  // Ctrl is one-shot, sticky-keys style: arm it, type the letter on the OS
  // keyboard, and the owner (Terminal.svelte) folds the two into a control
  // byte on its onData path. The armed state lives with the owner because
  // the fold happens there; this strip just renders and toggles it.
  //
  // The strip rides the visual viewport exactly like ConsoleKeys: when the
  // OS keyboard slides up, WKWebView shrinks the visual viewport rather
  // than the layout, so a plain fixed-bottom bar would sit UNDER the keys.
  import { onMount } from "svelte";

  export type StripKey = "esc" | "tab" | "up" | "down" | "left" | "right";

  let {
    ctrl,
    ontogglectrl,
    onkey,
    onkeyboard,
    onlift,
  }: {
    /** Whether the one-shot Ctrl is armed (owned by Terminal.svelte). */
    ctrl: boolean;
    ontogglectrl: () => void;
    onkey: (k: StripKey) => void;
    /** Focus the active emulator — the user gesture that opens the OS keyboard. */
    onkeyboard: () => void;
    /** How far the OS keyboard lifted the strip, so the pane can shrink to match. */
    onlift?: (px: number) => void;
  } = $props();

  let lift = $state(0);

  // Strip buttons must never steal focus from xterm's textarea — a blur
  // drops the OS keyboard mid-chord. pointerdown preventDefault keeps
  // focus where it is; the click still fires.
  function keepFocus(e: PointerEvent) {
    e.preventDefault();
  }

  onMount(() => {
    const vv = window.visualViewport;
    const track = () => {
      if (vv) lift = Math.max(0, window.innerHeight - vv.height - vv.offsetTop);
      onlift?.(lift);
    };
    track();
    vv?.addEventListener("resize", track);
    vv?.addEventListener("scroll", track);
    return () => {
      vv?.removeEventListener("resize", track);
      vv?.removeEventListener("scroll", track);
      onlift?.(0);
    };
  });
</script>

<div class="keys" style:bottom="{lift}px">
  <button class="k" onpointerdown={keepFocus} onclick={() => onkey("esc")}>esc</button>
  <button class="k" onpointerdown={keepFocus} onclick={() => onkey("tab")}>tab</button>
  <button class="k mod" class:armed={ctrl} onpointerdown={keepFocus} onclick={ontogglectrl}>ctrl</button>
  <button class="k" onpointerdown={keepFocus} onclick={() => onkey("left")}>←</button>
  <button class="k" onpointerdown={keepFocus} onclick={() => onkey("up")}>↑</button>
  <button class="k" onpointerdown={keepFocus} onclick={() => onkey("down")}>↓</button>
  <button class="k" onpointerdown={keepFocus} onclick={() => onkey("right")}>→</button>
  <button class="k kbd" onclick={onkeyboard} aria-label="Show keyboard">⌨</button>
</div>

<style>
  .keys {
    position: fixed;
    left: 0;
    right: 0;
    z-index: 70;
    display: flex;
    align-items: center;
    gap: 0.3rem;
    padding: 0.4rem 0.5rem calc(0.4rem + env(safe-area-inset-bottom, 0px) * 0.4)
      calc(0.5rem + env(safe-area-inset-left, 0px));
    background: oklch(0.17 0.026 285 / 0.94);
    backdrop-filter: blur(10px);
    border-top: 1px solid var(--line-strong);
    overflow-x: auto;
  }
  .k {
    flex-shrink: 0;
    border: 1px solid var(--line-strong);
    background: var(--surface);
    color: var(--ink-soft);
    border-radius: var(--r-sm);
    padding: 0.6rem 0.75rem;
    font-size: 0.85rem;
    font-weight: 650;
    line-height: 1;
    cursor: pointer;
  }
  .k:active {
    background: var(--surface-2);
  }
  .k.mod.armed {
    background: var(--accent-soft);
    border-color: var(--accent);
    color: var(--accent-ink);
  }
  .k.kbd {
    /* Pinned at the right even while the strip scrolls — the way to the
       keyboard must never be off-screen. */
    position: sticky;
    right: 0;
    margin-left: auto;
    color: var(--ink);
    background: oklch(0.2 0.028 285);
    box-shadow: -8px 0 10px -6px rgba(0, 0, 0, 0.7);
    font-size: 1rem;
  }
  @media (max-width: 420px) {
    .k {
      padding: 0.6rem 0.55rem;
      font-size: 0.8rem;
    }
  }
</style>
