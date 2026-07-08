// Swipe-to-dismiss for the docked side panels (mobile only).
//
// On a phone the sites/rooms sidebar and the device drawer float over the
// graph as full-height overlays. The natural way to put one away is to flick
// it back toward the edge it's docked to — swipe the left panel left, the
// right drawer right. This action adds exactly that, as a `use:` on the
// panel's root, leaving every other close affordance (the collapse chevron,
// the grab handle) untouched.
//
// It reads touch travel and only fires on a release that was a deliberate
// horizontal flick toward the docked edge — so vertical scrolling of the
// panel body is never stolen, and a tap on a button inside stays a tap.

export interface SwipeToCloseOptions {
  /** The screen edge the panel is docked to; a swipe *toward* it closes. */
  toward: "left" | "right";
  /** Called when a qualifying swipe completes. */
  onClose: () => void;
  /** Gate — the gesture only arms when this returns true (e.g. mobile and
   *  the panel currently open). Re-read at the start of every touch. */
  enabled?: () => boolean;
}

// px the finger must travel toward the edge for a release to dismiss.
const THRESHOLD = 56;
// The horizontal travel must beat the vertical by this factor to count as a
// sideways flick rather than a scroll.
const DOMINANCE = 1.3;
// Slop before we commit to "this is horizontal" vs "this is a scroll".
const DECIDE_SLOP = 10;

// A touch that begins on one of these keeps its native behaviour — the
// resizer runs its own pointer-capture drag, and a caret drag inside a text
// field must not be read as a dismiss.
const SKIP_SELECTOR = ".resizer, input, textarea, select";

export function swipeToClose(el: HTMLElement, options: SwipeToCloseOptions) {
  let opts = options;
  let startX = 0;
  let startY = 0;
  let tracking = false;
  let decided = false;
  let horizontal = false;

  const stop = () => {
    tracking = false;
    decided = false;
    horizontal = false;
  };

  const onStart = (e: TouchEvent) => {
    stop();
    if (opts.enabled && !opts.enabled()) return;
    if (e.touches.length !== 1) return;
    const target = e.target as HTMLElement | null;
    if (target?.closest(SKIP_SELECTOR)) return;
    startX = e.touches[0].clientX;
    startY = e.touches[0].clientY;
    tracking = true;
  };

  const onMove = (e: TouchEvent) => {
    if (!tracking) return;
    if (e.touches.length !== 1) {
      stop();
      return;
    }
    const dx = e.touches[0].clientX - startX;
    const dy = e.touches[0].clientY - startY;
    if (!decided) {
      if (Math.abs(dx) < DECIDE_SLOP && Math.abs(dy) < DECIDE_SLOP) return;
      decided = true;
      horizontal = Math.abs(dx) > Math.abs(dy) * DOMINANCE;
      // A vertical intent is a scroll — bow out and let the body scroll.
      if (!horizontal) {
        tracking = false;
        return;
      }
    }
    // Claim the horizontal gesture so the webview doesn't treat it as scroll
    // or back-navigation. (Requires a non-passive listener — see below.)
    e.preventDefault();
  };

  const onEnd = (e: TouchEvent) => {
    if (!tracking || !horizontal) {
      stop();
      return;
    }
    const dx = e.changedTouches[0].clientX - startX;
    const towardEdge = opts.toward === "left" ? -dx : dx;
    stop();
    if (towardEdge >= THRESHOLD) opts.onClose();
  };

  el.addEventListener("touchstart", onStart, { passive: true });
  // Non-passive so `preventDefault` in `onMove` actually claims the gesture;
  // Svelte's own `ontouchmove` binding is passive, where it'd be a no-op.
  el.addEventListener("touchmove", onMove, { passive: false });
  el.addEventListener("touchend", onEnd, { passive: true });
  el.addEventListener("touchcancel", stop, { passive: true });

  return {
    update(next: SwipeToCloseOptions) {
      opts = next;
    },
    destroy() {
      el.removeEventListener("touchstart", onStart);
      el.removeEventListener("touchmove", onMove);
      el.removeEventListener("touchend", onEnd);
      el.removeEventListener("touchcancel", stop);
    },
  };
}
