// Keyboard forwarding for the remote-control surfaces (the console stage,
// a room tile being driven), built around the assumption that key
// *combinations* are the norm: every event carries the physical
// `KeyboardEvent.code` alongside the layout-resolved `key` (the far side
// resolves chords through it), and held keys are tracked so they can be
// lifted in a burst when the sender can no longer promise their keyups —
// the window blurring mid-Alt+Tab, control toggling off, the session
// closing. Without the burst the remote keeps a stuck modifier, which
// reads as "the machine went haywire" until someone walks over and taps
// Ctrl on its real keyboard.

import type { InputAction } from "./types";

/** Modifier keys: kept out of the ⌘-quirk sweep (releasing a still-held
 *  Shift would betray the very next chord). */
const MODIFIER_KEYS = new Set(["Shift", "Control", "Alt", "Meta", "AltGraph"]);

export interface KeyForwarder {
  /** Forward one keydown/keyup. Auto-repeat is dropped (the remote OS
   *  does its own key repeat); callers gate and `preventDefault()`. */
  onKey(e: KeyboardEvent, down: boolean): void;
  /** Lift everything still held (in reverse press order). Call when the
   *  matching keyups can no longer arrive — blur, control off, close. */
  releaseAll(): void;
}

export function makeKeyForwarder(send: (action: InputAction) => void): KeyForwarder {
  // Held keys by physical code (key value when the code is empty) —
  // insertion order is press order.
  const held = new Map<string, { key: string; code?: string }>();

  const lift = (id: string) => {
    const h = held.get(id);
    if (!h) return;
    held.delete(id);
    send({ kind: "key", key: h.key, code: h.code, down: false });
  };

  return {
    onKey(e: KeyboardEvent, down: boolean) {
      if (e.repeat) return;
      const code = e.code || undefined;
      const id = e.code || e.key;
      if (down) {
        held.delete(id); // re-press keeps press order honest
        held.set(id, { key: e.key, code });
      } else {
        held.delete(id);
      }
      send({ kind: "key", key: e.key, code, down });
      // macOS webviews swallow the keyup of any key released while ⌘ is
      // still down — when ⌘ lifts, lift everything non-modifier with it,
      // or the remote keeps typing the last chord's letter.
      if (!down && e.key === "Meta") {
        for (const [id, h] of [...held]) {
          if (!MODIFIER_KEYS.has(h.key)) lift(id);
        }
      }
    },
    releaseAll() {
      for (const id of [...held.keys()].reverse()) lift(id);
    },
  };
}
