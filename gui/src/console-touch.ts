// Touch semantics for the console stage — the trackpad model, the
// TeamViewer way. The finger is never a stylus pointing at pixels: the
// remote cursor stays where it is, a one-finger drag STEERS it by
// deltas, a quick tap clicks AT THE CURSOR (your finger never covers
// the target you're aiming at), a second tap that stays down and drags
// is the click-hold-drag, and a long-press is the right button
// (press-and-hold context menus and right-drags both work). Two fingers
// leave the remote alone and belong to the VIEW: a pinch zooms the
// picture (and pans it while zoomed), a flat two-finger drag is the
// scroll wheel, and a two-finger tap is a right click. Mice and pens
// never come through here — a real pointer keeps the direct absolute
// mapping. The host keeps the virtual cursor and the follow-camera;
// this module only decides WHAT a touch means.

export type ViewTransform = { scale: number; x: number; y: number };

export interface TouchMouseHost {
  /** Pointer forwarding is live (control on, over a desktop picture). */
  active(): boolean;
  /** Steer the remote cursor by this many CSS px of finger travel. */
  moveBy(dx: number, dy: number): void;
  /** Press/release a remote button at the cursor's current position. */
  button(button: number, down: boolean): void;
  /** Remote scroll, in wheel lines. */
  wheel(dx: number, dy: number): void;
  /** The stage's current view transform (pinch zoom/pan). */
  view(): ViewTransform;
  /** Apply a view transform — the host clamps pan and snaps scale. */
  setView(t: ViewTransform): void;
  /** The fixed point the canvas is centered on, in client coordinates
   *  (the transform translates/scales about it). */
  viewCenter(): { x: number; y: number };
  /** A two-finger gesture began — close menus, cancel pending UI. */
  onGesture?(): void;
}

// Tap/drag discrimination. Slops are in CSS px — fingers wobble ~an
// order of magnitude more than mice, hence the generous 12.
const TAP_SLOP = 12; // a press that wanders past this is a glide, not a tap
const TAP_MS = 350; // a press longer than this never becomes a tap
const DOUBLE_MS = 350; // tap→press gap that arms the drag
const DOUBLE_SLOP = 64; // …and how far the second press may land from the tap
const LONG_MS = 550; // hold this long without moving = right button
const TWO_TAP_MS = 300; // both fingers down+up this fast = right click
const PINCH_LOG = 0.08; // |ln(scale)| beyond this commits the gesture to zoom
const PAN_START = 12; // midpoint travel that commits to scroll/pan
const SCROLL_PX = 28; // finger px per wheel line
const SCALE_MIN = 1;
const SCALE_MAX = 8;

export interface TouchMouse {
  down(e: PointerEvent): void;
  move(e: PointerEvent): void;
  up(e: PointerEvent): void;
  cancel(e: PointerEvent): void;
  /** Lift anything held and forget everything — control off, source
   *  switch, session end. Safe to call twice. */
  reset(): void;
}

export function makeTouchMouse(host: TouchMouseHost): TouchMouse {
  // Live touches by pointer id, insertion-ordered (the first two drive a
  // gesture; extras are tracked so their ups don't confuse the count).
  const touches = new Map<number, { x: number; y: number }>();

  // ---- single-finger state ----
  let downAt = 0;
  let downX = 0;
  let downY = 0;
  let glided = false; // wandered past TAP_SLOP — can't be a tap anymore
  let held: number | null = null; // remote button this finger is holding
  let longTimer: ReturnType<typeof setTimeout> | null = null;
  let lastTap: { x: number; y: number; t: number } | null = null;

  // ---- two-finger gesture state ----
  type Mode = "idle" | "single" | "gesture";
  let mode: Mode = "idle";
  // A gesture starts undecided and commits to exactly one meaning: zoom
  // (pinch — also pan while zoomed) or scroll. Deciding once keeps a
  // wobbly pinch from also scrolling the remote.
  let gKind: "pending" | "zoom" | "scroll" = "pending";
  let gStartAt = 0;
  let gDist0 = 1;
  let gMidX0 = 0;
  let gMidY0 = 0;
  let gLastMidX = 0;
  let gLastMidY = 0;
  let gView0: ViewTransform = { scale: 1, x: 0, y: 0 };
  let gMoved = false;
  let scrollAccX = 0;
  let scrollAccY = 0;

  const clearLong = () => {
    if (longTimer != null) {
      clearTimeout(longTimer);
      longTimer = null;
    }
  };

  // Re-anchor a live gesture on the CURRENT pair — whenever the pair's
  // membership changes (one of the first two lifted while a third stays),
  // the old baselines describe fingers that no longer exist and the view
  // would jump by their difference.
  const rebaseline = () => {
    const [a, b] = firstTwo();
    if (!a || !b) return;
    gDist0 = Math.max(1, dist(a.x, a.y, b.x, b.y));
    gMidX0 = (a.x + b.x) / 2;
    gMidY0 = (a.y + b.y) / 2;
    gLastMidX = gMidX0;
    gLastMidY = gMidY0;
    gView0 = host.view();
  };

  const dist = (ax: number, ay: number, bx: number, by: number) =>
    Math.hypot(ax - bx, ay - by);

  const firstTwo = (): Array<{ x: number; y: number }> => {
    const out: Array<{ x: number; y: number }> = [];
    for (const t of touches.values()) {
      out.push(t);
      if (out.length === 2) break;
    }
    return out;
  };

  const liftHeld = () => {
    if (held != null) {
      host.button(held, false);
      held = null;
    }
  };

  function down(e: PointerEvent) {
    touches.set(e.pointerId, { x: e.clientX, y: e.clientY });

    if (touches.size === 1) {
      mode = "single";
      downAt = performance.now();
      downX = e.clientX;
      downY = e.clientY;
      glided = false;
      clearLong();
      if (!host.active()) return; // view-only: a lone finger pans (see move)
      // Tap-then-press within the window: this press IS the drag — the
      // button goes down at the cursor, and every move from here drags.
      // (The proximity check is between the two FINGER touches — the
      // gesture must feel like one double-tap, wherever the cursor is.)
      if (
        lastTap &&
        downAt - lastTap.t < DOUBLE_MS &&
        dist(e.clientX, e.clientY, lastTap.x, lastTap.y) < DOUBLE_SLOP
      ) {
        lastTap = null;
        held = 0;
        host.button(0, true);
        return;
      }
      // A fresh press touches nothing yet — the cursor stays where it is
      // (trackpad rule); an unmoved hold becomes the right button, at
      // the cursor.
      longTimer = setTimeout(() => {
        longTimer = null;
        if (mode !== "single" || glided || held != null) return;
        held = 2;
        host.button(2, true);
      }, LONG_MS);
      return;
    }

    if (touches.size === 2) {
      // Second finger: the remote keeps nothing from the single-finger
      // story — pending taps die, a held drag lifts (a pinch mid-drag
      // must not smear the drag around), and the pair drives the view.
      clearLong();
      lastTap = null;
      liftHeld();
      mode = "gesture";
      gKind = "pending";
      gStartAt = performance.now();
      const [a, b] = firstTwo();
      gDist0 = Math.max(1, dist(a.x, a.y, b.x, b.y));
      gMidX0 = (a.x + b.x) / 2;
      gMidY0 = (a.y + b.y) / 2;
      gLastMidX = gMidX0;
      gLastMidY = gMidY0;
      gView0 = host.view();
      gMoved = false;
      scrollAccX = 0;
      scrollAccY = 0;
      host.onGesture?.();
    }
  }

  function move(e: PointerEvent) {
    const t = touches.get(e.pointerId);
    if (!t) return;
    const prevX = t.x;
    const prevY = t.y;
    t.x = e.clientX;
    t.y = e.clientY;

    if (mode === "single") {
      if (!glided && dist(e.clientX, e.clientY, downX, downY) > TAP_SLOP) {
        glided = true;
        clearLong();
        lastTap = null; // a glide breaks the tap→drag chain
      }
      if (host.active()) {
        // Glide or drag — either way the finger's travel steers the
        // cursor by deltas, trackpad-style.
        host.moveBy(e.clientX - prevX, e.clientY - prevY);
      } else if (host.view().scale > 1.001) {
        // View-only and zoomed in: the lone finger pans the picture.
        const v = host.view();
        host.setView({ scale: v.scale, x: v.x + (e.clientX - prevX), y: v.y + (e.clientY - prevY) });
      }
      return;
    }

    if (mode !== "gesture" || touches.size < 2) return;
    const [a, b] = firstTwo();
    const d = Math.max(1, dist(a.x, a.y, b.x, b.y));
    const midX = (a.x + b.x) / 2;
    const midY = (a.y + b.y) / 2;
    const midTravel = dist(midX, midY, gMidX0, gMidY0);

    if (gKind === "pending") {
      // A pinch must stretch by ratio AND by real pixels: the two fingers
      // update alternately, so a fast two-finger scroll sees the pair
      // distance flutter by one event's travel — ratio alone would read
      // that as a pinch and hijack the scroll.
      const pinching = Math.abs(Math.log(d / gDist0)) > PINCH_LOG && Math.abs(d - gDist0) > 16;
      // Zoomed in, any real two-finger motion adjusts the view — but only
      // past a whisper of movement, so a two-finger TAP (fingers always
      // wobble a px or two on glass) still reads as the right click.
      const wiggle = midTravel > 4 || Math.abs(d - gDist0) > 4;
      if ((gView0.scale > 1.001 && wiggle) || pinching) {
        gKind = "zoom";
      } else if (midTravel > PAN_START) {
        // Flat two-finger drag at 1:1 — the scroll wheel. Without
        // control there's no wheel to turn, so it pans-by-zoom instead
        // (which at scale 1 clamps to no-op — harmless).
        gKind = host.active() ? "scroll" : "zoom";
      } else {
        return;
      }
      gMoved = true;
    }

    gMoved = true;
    if (gKind === "zoom") {
      // Keep the content point that was under the fingers' midpoint at
      // gesture start pinned under the midpoint now: solve the transform
      // (translate about the fixed stage center, then scale) for that.
      const c = host.viewCenter();
      const scale = Math.min(SCALE_MAX, Math.max(SCALE_MIN, (gView0.scale * d) / gDist0));
      const p0x = (gMidX0 - c.x - gView0.x) / gView0.scale;
      const p0y = (gMidY0 - c.y - gView0.y) / gView0.scale;
      host.setView({ scale, x: midX - c.x - p0x * scale, y: midY - c.y - p0y * scale });
    } else {
      // Natural scrolling: content follows the fingers, so fingers up =
      // wheel down. Accumulate midpoint travel and emit whole lines.
      scrollAccX += midX - gLastMidX;
      scrollAccY += midY - gLastMidY;
      gLastMidX = midX;
      gLastMidY = midY;
      const linesX = Math.trunc(scrollAccX / SCROLL_PX);
      const linesY = Math.trunc(scrollAccY / SCROLL_PX);
      if (linesX !== 0 || linesY !== 0) {
        scrollAccX -= linesX * SCROLL_PX;
        scrollAccY -= linesY * SCROLL_PX;
        host.wheel(-linesX, -linesY);
      }
    }
  }

  function up(e: PointerEvent) {
    const wasTracked = touches.delete(e.pointerId);
    if (!wasTracked) return;

    if (mode === "single") {
      clearLong();
      const now = performance.now();
      if (host.active()) {
        if (held != null) {
          // End of a drag (left) or a long-press (right) — the cursor is
          // already where the steering left it; just let go.
          host.button(held, false);
          held = null;
          lastTap = null;
        } else if (!glided && now - downAt < TAP_MS) {
          // A clean tap: click at the cursor. The tap is remembered (by
          // its FINGER position) so an immediate second press nearby can
          // become the drag.
          host.button(0, true);
          host.button(0, false);
          lastTap = { x: e.clientX, y: e.clientY, t: now };
        } else {
          lastTap = null;
        }
      }
      mode = "idle";
      return;
    }

    if (mode === "gesture") {
      if (touches.size >= 2) {
        // A finger lifted but two remain — the driving pair may have
        // changed members; re-anchor so the view doesn't jump.
        rebaseline();
        return;
      }
      const now = performance.now();
      if (
        gKind === "pending" &&
        !gMoved &&
        now - gStartAt < TWO_TAP_MS &&
        host.active()
      ) {
        // Two-finger tap — the trackpad right click, at the cursor.
        host.button(2, true);
        host.button(2, false);
      }
      if (gKind === "zoom") {
        // Near-1 zooms snap home so the picture never sits imperceptibly
        // off its natural fit.
        const v = host.view();
        if (v.scale < 1.02) host.setView({ scale: 1, x: 0, y: 0 });
      }
      if (touches.size === 1) {
        // One finger stayed down: it continues as a glide, never a tap —
        // lifting two fingers one at a time must not click.
        const rest = touches.values().next().value!;
        mode = "single";
        downAt = performance.now();
        downX = rest.x;
        downY = rest.y;
        glided = true;
        lastTap = null;
      } else {
        mode = "idle";
      }
      return;
    }
  }

  function cancel(e: PointerEvent) {
    // The OS reclaimed the touch (system gesture, notification pull) —
    // its up is never coming. Lift what's held and forget the finger.
    touches.delete(e.pointerId);
    clearLong();
    liftHeld();
    lastTap = null;
    mode = touches.size >= 2 ? "gesture" : touches.size === 1 ? "single" : "idle";
    if (mode === "single") glided = true; // survivors glide, never tap
    if (mode === "gesture") rebaseline();
    else gKind = "pending";
  }

  function reset() {
    clearLong();
    liftHeld();
    touches.clear();
    lastTap = null;
    mode = "idle";
    gKind = "pending";
  }

  return { down, move, up, cancel, reset };
}
