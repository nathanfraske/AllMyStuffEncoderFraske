<script lang="ts">
  // The console's soft keyboard — how a phone types into the remote
  // machine. Not an on-screen QWERTY: the OS keyboard is the one people
  // can actually type on, so an invisible input summons it and this
  // component translates what it produces (beforeinput text, deletes,
  // line breaks) into key events down the control route. What the OS
  // keyboard can't say — Esc, Tab, arrows, chord modifiers — lives on a
  // strip pinned just above it.
  //
  // Modifiers are one-shot: tap ctrl, tap the letter on the OS keyboard,
  // and the chord lands (the modifier goes down on arm, lifts after the
  // next real key — sticky-keys style, the only workable model when the
  // two keyboards can't be held at once). Tapping an armed modifier
  // disarms it.
  //
  // The strip rides the visual viewport: when the OS keyboard slides up,
  // WKWebView shrinks the visual viewport rather than the layout, so a
  // plain fixed-bottom bar would sit UNDER the keys. Tracking
  // visualViewport keeps the strip exactly above them.
  import { onMount } from "svelte";
  import type { InputAction } from "../types";

  let {
    send,
    onclose,
    rightInset = "0px",
  }: {
    send: (a: InputAction) => void;
    onclose: () => void;
    // How far to hold the strip's right edge off the pane's right side, so
    // it clears the console's control rail (the phone's vertical bar lives
    // there). A CSS length; "0px" on shells with no right-edge rail. The
    // caller owns the rail's width, so the strip never guesses it.
    rightInset?: string;
  } = $props();

  let inputEl = $state<HTMLInputElement | null>(null);
  let lift = $state(0);

  // The input always holds this sentinel (zero-width spaces) so Backspace
  // has something to delete — an empty field swallows the delete on some
  // soft keyboards (no beforeinput fires for a no-op edit), and the
  // sentinel makes every press observable. Never rendered: the input is
  // invisible.
  const SENTINEL = "\u200b".repeat(8);
  const rearm = () => {
    const el = inputEl;
    if (!el) return;
    el.value = SENTINEL;
    el.setSelectionRange(SENTINEL.length, SENTINEL.length);
  };

  const MODS = [
    { code: "ControlLeft", key: "Control", label: "ctrl" },
    { code: "AltLeft", key: "Alt", label: "alt" },
    { code: "MetaLeft", key: "Meta", label: "cmd" },
    { code: "ShiftLeft", key: "Shift", label: "shift" },
  ];
  let armed = $state<string[]>([]); // armed modifier codes, in arm order

  function toggleMod(code: string) {
    const m = MODS.find((x) => x.code === code)!;
    if (armed.includes(code)) {
      send({ kind: "key", key: m.key, code: m.code, down: false });
      armed = armed.filter((c) => c !== code);
    } else {
      send({ kind: "key", key: m.key, code: m.code, down: true });
      armed = [...armed, code];
    }
  }

  // One-shot discharge: armed modifiers lift (in reverse arm order) after
  // the key they modified, like sticky keys everywhere.
  function dischargeMods() {
    for (const code of [...armed].reverse()) {
      const m = MODS.find((x) => x.code === code)!;
      send({ kind: "key", key: m.key, code: m.code, down: false });
    }
    armed = [];
  }

  function tapKey(key: string, code?: string) {
    send({ kind: "key", key, code, down: true });
    send({ kind: "key", key, code, down: false });
    dischargeMods();
  }

  function typed(text: string) {
    for (const ch of text) {
      if (ch === "\u200b") continue; // a sentinel char leaking through
      send({ kind: "key", key: ch === "\n" ? "Enter" : ch, down: true });
      send({ kind: "key", key: ch === "\n" ? "Enter" : ch, down: false });
    }
    dischargeMods();
  }

  function onBeforeInput(e: InputEvent) {
    // Composition (CJK, dictation) delivers its result at compositionend;
    // the per-keystroke composition edits are noise here.
    if (e.inputType === "insertCompositionText") return;
    e.preventDefault();
    rearm();
    switch (e.inputType) {
      case "insertText":
      case "insertFromPaste":
      case "insertReplacementText":
        if (e.data) typed(e.data);
        break;
      case "insertLineBreak":
      case "insertParagraph":
        tapKey("Enter", "Enter");
        break;
      case "deleteContentBackward":
        tapKey("Backspace", "Backspace");
        break;
      case "deleteContentForward":
        tapKey("Delete", "Delete");
        break;
      default:
        // Whatever else the IME says (word-deletes, keyboard-bar paste
        // variants): deletes degrade to one Backspace, inserts send
        // their text — swallowing them silently reads as a broken key.
        if (e.inputType.startsWith("delete")) tapKey("Backspace", "Backspace");
        else if (e.data) typed(e.data);
    }
  }

  function onCompositionEnd(e: CompositionEvent) {
    if (e.data) typed(e.data);
    rearm();
  }

  // A hardware keyboard (or an OS keyboard that speaks real key events)
  // reaching this input: named keys forward directly; single characters
  // are left to beforeinput so nothing lands twice. Enter/Backspace/Delete
  // are beforeinput's too (they edit the sentinel).
  function onKeyDown(e: KeyboardEvent) {
    if (e.isComposing) return;
    const named =
      e.key.length > 1 &&
      ![
        "Enter",
        "Backspace",
        "Delete",
        "Unidentified",
        "Process",
        "Dead",
        "Shift",
        "Control",
        "Alt",
        "Meta",
      ].includes(e.key);
    if (!named) return;
    e.preventDefault();
    tapKey(e.key, e.code || undefined);
  }

  // Strip buttons must never steal focus from the input — a blur here
  // drops the OS keyboard mid-chord. pointerdown preventDefault keeps
  // focus where it is; the click still fires.
  function keepFocus(e: PointerEvent) {
    e.preventDefault();
  }

  onMount(() => {
    rearm();
    inputEl?.focus();
    const vv = window.visualViewport;
    const track = () => {
      if (vv) lift = Math.max(0, window.innerHeight - vv.height - vv.offsetTop);
    };
    track();
    vv?.addEventListener("resize", track);
    vv?.addEventListener("scroll", track);
    return () => {
      vv?.removeEventListener("resize", track);
      vv?.removeEventListener("scroll", track);
      // Whatever was armed lifts with the strip — a closed keyboard must
      // not leave the remote holding ctrl.
      dischargeMods();
    };
  });
</script>

<div class="keys" style:bottom="{lift}px" style:right={rightInset}>
  <button class="k" onpointerdown={keepFocus} onclick={() => tapKey("Escape", "Escape")}>esc</button>
  <button class="k" onpointerdown={keepFocus} onclick={() => tapKey("Tab", "Tab")}>tab</button>
  {#each MODS as m (m.code)}
    <button
      class="k mod"
      class:armed={armed.includes(m.code)}
      onpointerdown={keepFocus}
      onclick={() => toggleMod(m.code)}>{m.label}</button
    >
  {/each}
  <button class="k" onpointerdown={keepFocus} onclick={() => tapKey("ArrowLeft", "ArrowLeft")}>←</button>
  <button class="k" onpointerdown={keepFocus} onclick={() => tapKey("ArrowUp", "ArrowUp")}>↑</button>
  <button class="k" onpointerdown={keepFocus} onclick={() => tapKey("ArrowDown", "ArrowDown")}>↓</button>
  <button class="k" onpointerdown={keepFocus} onclick={() => tapKey("ArrowRight", "ArrowRight")}>→</button>
  <button class="k done" onclick={onclose} aria-label="Hide keyboard">⌄</button>
  <!-- The invisible line to the OS keyboard. Attributes shut down
       everything that would rewrite what the user typed. -->
  <input
    bind:this={inputEl}
    class="ghost"
    type="text"
    autocapitalize="off"
    autocomplete="off"
    spellcheck={false}
    aria-label="Type on the remote machine"
    onbeforeinput={onBeforeInput}
    oncompositionend={onCompositionEnd}
    onkeydown={onKeyDown}
    onblur={onclose}
  />
</div>

<style>
  .keys {
    position: fixed;
    left: 0;
    /* `right` is set inline (rightInset) so the strip stops short of the
       control rail on the right edge instead of sliding under it. */
    right: 0;
    z-index: 70;
    display: flex;
    align-items: center;
    gap: 0.3rem;
    padding: 0.4rem 0.5rem calc(0.4rem + env(safe-area-inset-bottom, 0px) * 0.4) calc(0.5rem + env(safe-area-inset-left, 0px));
    background: oklch(0.17 0.026 285 / 0.94);
    backdrop-filter: blur(10px);
    /* A floating tray (rounded top, boxed on three sides) rather than a
       full-bleed band — it reads as one control that leaves the picture
       room, and its right edge sits clear of the rail. */
    border: 1px solid var(--line-strong);
    border-bottom: none;
    border-radius: var(--r-md, 12px) var(--r-md, 12px) 0 0;
    box-shadow: 0 -6px 16px -8px rgba(0, 0, 0, 0.6);
    overflow-x: auto;
  }
  .k {
    flex-shrink: 0;
    border: 1px solid var(--line-strong);
    background: var(--surface);
    color: var(--ink-soft);
    border-radius: var(--r-sm);
    padding: 0.5rem 0.65rem;
    font-size: 0.82rem;
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
  .k.done {
    /* Pinned at the right even while the strip scrolls — the way out
       must never be off-screen. */
    position: sticky;
    right: 0;
    margin-left: auto;
    color: var(--ink);
    background: oklch(0.2 0.028 285);
    box-shadow: -8px 0 10px -6px rgba(0, 0, 0, 0.7);
  }
  @media (max-width: 420px) {
    .k {
      padding: 0.5rem 0.5rem;
      font-size: 0.78rem;
    }
  }
  .ghost {
    position: absolute;
    width: 1px;
    height: 1px;
    opacity: 0;
    border: none;
    padding: 0;
    pointer-events: none;
  }
</style>
