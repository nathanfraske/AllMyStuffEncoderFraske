<script lang="ts">
  // One member's share inside a room panel — their screen or their
  // camera, told apart by the route's media — a lean cousin of the
  // console stage. It always asks the backend to decode (`decode: true`,
  // the console ladder's universal bottom rung), so the tile just blits
  // RGBA / JPEG frames and works in every webview; a member who wants the
  // full-quality path opens a console on the sharer instead.
  //
  // The picture letterboxes like every call app's: `object-fit: contain`
  // inside the tile, black bars top/bottom *or* left/right as the shapes
  // demand, never a stretch and never a crop.
  //
  // When the sharer also turned "share control" on (there's a live input
  // route from this machine's keyboard & mouse to theirs), the tile
  // captures clicks/keys over the picture and sends them down that route.
  import { makeKeyForwarder } from "../input-keys";
  import { app } from "../store.svelte";
  import { sendInput, watchVideo } from "../tauri";
  import { type InputAction, type MeshNode, type Route } from "../types";

  let { route, member }: { route: Route; member: MeshNode } = $props();

  let canvasEl = $state<HTMLCanvasElement | null>(null);
  let hasFrame = $state(false);
  // The streamed frame's pixel size — the content box the letterbox math
  // (and pointer normalization) works against.
  let frameW = $state(0);
  let frameH = $state(0);

  const who = $derived(app.roomWho(member.id));
  // A video route is a camera feed; a display route is a screen share —
  // the badge says which, since both tile identically.
  const isCamera = $derived(route.media === "video");

  // The live route this tile may drive the sharer with, if any.
  const controlRoute = $derived(app.roomControlRouteTo(member.id));

  $effect(() => {
    const routeId = route.id;
    let cancelled = false;
    let unwatch: (() => void) | null = null;
    hasFrame = false;

    const paint = (w: number, h: number, draw: (ctx: CanvasRenderingContext2D) => void) => {
      const c = canvasEl;
      if (cancelled || !c) return;
      if (c.width !== w) c.width = w;
      if (c.height !== h) c.height = h;
      const ctx = c.getContext("2d");
      if (!ctx) return;
      draw(ctx);
      frameW = w;
      frameH = h;
      hasFrame = true;
    };

    // JPEG bitmap decodes are async — chain them so frames paint in order.
    let drawChain = Promise.resolve();

    void watchVideo(
      routeId,
      (f) => {
        if (cancelled) return;
        if (f.kind === "raw") {
          if (f.data.byteLength !== f.width * f.height * 4) return;
          const pixels = new Uint8ClampedArray(f.data.buffer, f.data.byteOffset, f.data.byteLength);
          const img = new ImageData(pixels, f.width, f.height);
          paint(f.width, f.height, (ctx) => ctx.putImageData(img, 0, 0));
        } else if (f.kind === "jpeg") {
          const blob = new Blob([f.data], { type: "image/jpeg" });
          drawChain = drawChain.then(async () => {
            if (cancelled) return;
            try {
              const bmp = await createImageBitmap(blob);
              paint(bmp.width, bmp.height, (ctx) => ctx.drawImage(bmp, 0, 0));
              bmp.close();
            } catch {
              // A torn frame decodes as nothing; the next one stands alone.
            }
          });
        }
        // h264 never arrives here — decode: true means the backend hands
        // this tile ready-to-paint frames.
      },
      { decode: true },
    ).then((u) => {
      if (cancelled) u();
      else unwatch = u;
    });

    return () => {
      cancelled = true;
      unwatch?.();
    };
  });

  // ---- control capture (only when the sharer allowed it) ------------

  function send(action: InputAction) {
    if (controlRoute) void sendInput(controlRoute, action);
  }

  // Coordinates are normalized 0..1 over the streamed frame's *content
  // box* — the picture inside the letterbox, not the element — exactly
  // like the console stage, so clicks land where they look like they do.
  function norm(e: PointerEvent): { x: number; y: number } | null {
    const c = canvasEl;
    if (!c || !frameW || !frameH) return null;
    const r = c.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) return null;
    const scale = Math.min(r.width / frameW, r.height / frameH);
    const cw = frameW * scale;
    const ch = frameH * scale;
    const ox = r.left + (r.width - cw) / 2;
    const oy = r.top + (r.height - ch) / 2;
    const x = (e.clientX - ox) / cw;
    const y = (e.clientY - oy) / ch;
    if (x < 0 || x > 1 || y < 0 || y > 1) return null;
    return { x, y };
  }

  function onPointerMove(e: PointerEvent) {
    if (!controlRoute) return;
    const p = norm(e);
    if (p) send({ kind: "mouse_move", x: p.x, y: p.y });
  }
  function onPointerDown(e: PointerEvent) {
    if (!controlRoute) return;
    (e.currentTarget as HTMLElement).focus();
    const p = norm(e);
    if (!p) return;
    send({ kind: "mouse_move", x: p.x, y: p.y });
    send({ kind: "mouse_button", button: e.button, down: true });
  }
  function onPointerUp(e: PointerEvent) {
    if (!controlRoute) return;
    send({ kind: "mouse_button", button: e.button, down: false });
  }
  function onWheel(e: WheelEvent) {
    if (!controlRoute) return;
    e.preventDefault();
    send({ kind: "wheel", dx: e.deltaX, dy: e.deltaY });
  }
  // Key forwarding with the bookkeeping combinations need: the physical
  // `code` rides along, auto-repeat stays local, and keys still held when
  // the tile loses focus are lifted in a burst — otherwise the sharer's
  // machine keeps a stuck modifier.
  const keys = makeKeyForwarder(send);

  function onKey(e: KeyboardEvent, down: boolean) {
    if (!controlRoute) return;
    e.preventDefault();
    keys.onKey(e, down);
  }
</script>

<!-- role=application: like the console stage, a screen-share tile with
     control on is a remote-desktop surface — every pointer/key goes to
     the far machine, not this document. The tile is focusable only while
     control is granted, so keys land here and nowhere else. -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div
  class="tile"
  class:driving={!!controlRoute}
  role="application"
  aria-label="{who.who}'s {isCamera ? 'camera' : 'screen'}{who.machine ? ` (${who.machine})` : ''}"
  tabindex={controlRoute ? 0 : -1}
  onpointermove={onPointerMove}
  onpointerdown={onPointerDown}
  onpointerup={onPointerUp}
  onwheel={onWheel}
  onkeydown={(e) => onKey(e, true)}
  onkeyup={(e) => onKey(e, false)}
  onblur={() => keys.releaseAll()}
>
  <canvas bind:this={canvasEl} class:waiting={!hasFrame}></canvas>
  {#if !hasFrame}
    <div class="waiting-note">
      <span class="who">{who.who}{#if who.machine}&nbsp;<span class="machine">· {who.machine}</span>{/if}</span>
      <span class="note">waiting for pixels…</span>
    </div>
  {:else}
    <div class="badge">
      {isCamera ? "📷" : "🖥"} <b>{who.who}</b>{#if who.machine}<span class="machine">· {who.machine}</span>{/if}
      {#if controlRoute}<span class="ctl" title="They turned control sharing on — click and type here to drive their machine">🕹 you can drive</span>{/if}
    </div>
  {/if}
</div>

<style>
  .tile {
    position: relative;
    background: #000;
    border-radius: var(--r-md);
    overflow: hidden;
    border: 1px solid var(--line-strong);
    min-height: 8rem;
    min-width: 0;
    /* The single track is the tile's size, never the canvas's intrinsic
       one — the letterbox comes from object-fit inside the element. */
    display: grid;
    grid-template-rows: minmax(0, 1fr);
    grid-template-columns: minmax(0, 1fr);
    place-items: center;
  }
  .tile.driving {
    cursor: crosshair;
  }
  .tile.driving:focus {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  canvas {
    width: 100%;
    height: 100%;
    object-fit: contain;
    display: block;
  }
  /* Mounted (so the first frame has somewhere to land) but invisible
     until one does — the waiting note shows through. */
  canvas.waiting {
    visibility: hidden;
  }
  .waiting-note {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 0.25rem;
  }
  .who {
    font-size: 0.85rem;
    font-weight: 650;
  }
  .note {
    font-size: 0.74rem;
    color: var(--ink-faint);
  }
  .badge {
    position: absolute;
    left: 0.5rem;
    bottom: 0.5rem;
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    background: rgba(0, 0, 0, 0.55);
    backdrop-filter: blur(4px);
    border-radius: var(--r-pill);
    padding: 0.18rem 0.55rem;
    font-size: 0.72rem;
    font-weight: 600;
    max-width: calc(100% - 1rem);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .machine {
    color: oklch(0.75 0.02 285);
    font-weight: 500;
  }
  .ctl {
    color: var(--ok);
  }
</style>
