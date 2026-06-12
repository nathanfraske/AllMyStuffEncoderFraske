<script lang="ts">
  // The remote console — a pikvm-style session for another machine. A
  // video-inputs tab bar across the top picks which of the remote's
  // sources you're looking at (its screen, its cameras); the bar
  // underneath is the handle for audio passthrough and keyboard/mouse
  // control. It owns the real routes the session runs on, so toggles here
  // actually wire (and unwire) the mesh.
  //
  // Two skins, one component: the desktop renders it `windowed` — filling
  // a dedicated per-machine OS window (see ConsoleHost) so several
  // consoles can be open at once — while the web preview keeps the in-page
  // popover.
  //
  // The stage is a live MJPEG sink: the backend pushes each inbound frame
  // for the watched route over a per-route IPC channel (raw JPEG bytes —
  // see `watchVideo`), and this component shows the latest one. When
  // "Keyboard & mouse" is on, the stage captures pointer/key events,
  // normalizes coordinates onto the streamed frame, and forwards them down
  // the control route.
  import { onMount } from "svelte";
  import { app } from "../store.svelte";
  import {
    closeThisWindow,
    onThisWindowClose,
    refreshRoute,
    watchVideo,
    watchVideoStatus,
    type VideoHostStatus,
  } from "../tauri";
  import {
    displayName,
    originIcon,
    mediaColor,
    MEDIA,
    type Capability,
    type MediaKind,
  } from "../types";

  let { windowed = false }: { windowed?: boolean } = $props();

  const node = $derived(app.consoleNode);
  const inputs = $derived(node ? app.consoleVideoInputs(node.id) : []);
  const selectedId = $derived(app.consoleInput);
  const selected = $derived<Capability | null>(
    (selectedId ? app.capability(selectedId) : null) ?? null,
  );
  // Which remote monitor the stage is showing (`<node>:screen:<id>`),
  // undefined for the primary `screen` (and for cameras) — rides every
  // mouse move so the remote lands the cursor on the screen being viewed.
  const controlScreen = $derived.by(() => {
    const m = selectedId?.match(/:screen:(\d+)$/);
    return m ? Number(m[1]) : undefined;
  });

  // ---- live video ---------------------------------------------------
  //
  // The stage is a canvas with three decode paths: H.264 access units
  // through WebCodecs (hardware where the webview has it), JPEG frames
  // through createImageBitmap, and — when this webview has no WebCodecs
  // (Linux WebKitGTK) or its decoder stalled out — the backend's native
  // openh264 decoder, which delivers ready-to-paint RGBA frames this
  // component just blits. The WebCodecs decoder is created lazily on the
  // first key unit and rebuilt on the next one after any error.
  let canvasEl = $state<HTMLCanvasElement | null>(null);
  let hasFrame = $state(false);
  // The host's word on why pixels aren't flowing (`vstat`): shown on the
  // placeholder before the first frame, and as a banner if the stream
  // stalls after one. Null = no condition reported (or it cleared).
  let hostStatus = $state<VideoHostStatus | null>(null);
  let frameW = $state(0);
  let frameH = $state(0);
  let fps = $state(0);
  let transport = $state("");
  let decodeFails = $state(0);
  /// Pipeline anomaly readout — empty while healthy, e.g.
  /// "in 14/s · out 0/s · q 38 · sw" when packets arrive but frames
  /// don't, so a stall names its stage on the chip itself.
  let pipeDiag = $state("");
  let frameCount = 0;
  let inCount = 0;
  let queuePeek = () => 0;
  let stallKick = () => {};
  let decodeModeNote = "";
  // Whether the backend decodes for us (raw RGBA in, no webview codec).
  // Starts true where WebCodecs doesn't exist at all; the decode ladder
  // also lands here after WebCodecs stalls twice. Sticky for the session —
  // a webview whose decoder wedged once isn't owed a third chance.
  let nativeDecode = $state(typeof VideoDecoder === "undefined");

  // ---- the quality pills ---------------------------------------------
  //
  // Resolution, frame rate, and bitrate ride a `Tune` ask to the machine
  // being viewed (its capture restarts with the picks); the codec pill
  // re-offers the route on the chosen transport and picks where H.264 is
  // decoded. Auto everywhere = exactly the automatic pipeline.
  type PillChoice = { label: string; value: number | null };
  const RES_CHOICES: PillChoice[] = [
    { label: "Auto", value: null },
    { label: "4K", value: 3840 },
    { label: "1440p", value: 2560 },
    { label: "1080p", value: 1920 },
    { label: "720p", value: 1280 },
  ];
  const FPS_CHOICES: PillChoice[] = [
    { label: "Auto", value: null },
    { label: "60", value: 60 },
    { label: "30", value: 30 },
    { label: "24", value: 24 },
    { label: "15", value: 15 },
  ];
  const RATE_CHOICES: PillChoice[] = [
    { label: "Auto", value: null },
    { label: "40 Mbps", value: 40_000_000 },
    { label: "25 Mbps", value: 25_000_000 },
    { label: "15 Mbps", value: 15_000_000 },
    { label: "8 Mbps", value: 8_000_000 },
    { label: "4 Mbps", value: 4_000_000 },
  ];
  type CodecChoice = "auto" | "h264" | "native" | "mjpeg";
  const CODEC_CHOICES: Array<{ label: string; value: CodecChoice }> = [
    { label: "Auto", value: "auto" },
    { label: "H.264", value: "h264" },
    { label: "H.264 · native decode", value: "native" },
    { label: "MJPEG", value: "mjpeg" },
  ];
  let codecChoice = $state<CodecChoice>("auto");
  let openPill = $state<"res" | "fps" | "rate" | "codec" | null>(null);

  const pillLabel = (choices: PillChoice[], v: number | null | undefined) =>
    choices.find((c) => c.value === (v ?? null))?.label ?? "Auto";

  function pickRes(v: number | null) {
    app.setConsoleTune({ maxEdge: v ?? undefined });
    openPill = null;
  }
  function pickFps(v: number | null) {
    app.setConsoleTune({ fps: v ?? undefined });
    openPill = null;
  }
  function pickRate(v: number | null) {
    app.setConsoleTune({ bitrate: v ?? undefined });
    openPill = null;
  }
  function pickCodec(v: CodecChoice) {
    codecChoice = v;
    openPill = null;
    // Where to decode is this window's choice; which transport to offer
    // is the store's (it re-offers the route when that part changes).
    nativeDecode = v === "native" || (v === "auto" && typeof VideoDecoder === "undefined");
    app.setConsoleCodec(v === "mjpeg" ? "mjpeg" : v === "auto" ? "auto" : "h264");
  }

  // Decode errors ask the sender for a clean entry (rate-limited again
  // backend-side) instead of sitting out the periodic IDR interval.
  let lastRefreshAsk = 0;
  function askRefresh() {
    const route = app.consoleVideoLive;
    if (!route) return;
    const now = performance.now();
    if (now - lastRefreshAsk < 700) return;
    lastRefreshAsk = now;
    void refreshRoute(route);
  }

  onMount(() => {
    let unlistenClose: (() => void) | undefined;
    const fpsTimer = setInterval(() => {
      fps = frameCount;
      const inRate = inCount;
      frameCount = 0;
      inCount = 0;
      // Healthy: most of what arrives gets painted. Anything else is an
      // anomaly worth wearing on the chip.
      pipeDiag =
        inRate > 2 && fps < inRate / 2
          ? `in ${inRate}/s · out ${fps}/s · q ${queuePeek()}${decodeModeNote ? ` · ${decodeModeNote}` : ""}`
          : "";
      // Chunks flowing in, paints collapsed, queue deep: the decoder
      // stopped consuming (the hardware-pool stall). Rebuild it — the
      // ladder steps to software decode on the way.
      if (inRate > 5 && fps < inRate / 4 && queuePeek() > 8) stallKick();
    }, 1000);
    if (windowed) {
      // The OS chrome's ✕ must tear the session's routes down too — the
      // close is held until they're on the wire (see onThisWindowClose).
      void onThisWindowClose(() => void endSession()).then((u) => (unlistenClose = u));
    }
    return () => {
      unlistenClose?.();
      clearInterval(fpsTimer);
    };
  });

  // The exact codec string for the incoming stream, read off its SPS
  // (profile/constraints/level are the three bytes after the NAL header)
  // — a guessed string risks the decoder sizing reorder buffers for a
  // stream we're not sending.
  function spsCodecString(au: Uint8Array): string | null {
    for (let i = 0; i + 4 < au.length; i++) {
      if (au[i] !== 0 || au[i + 1] !== 0) continue;
      const off = au[i + 2] === 1 ? i + 3 : au[i + 2] === 0 && au[i + 3] === 1 ? i + 4 : 0;
      if (!off) continue;
      if ((au[off] & 0x1f) === 7 && off + 3 < au.length) {
        const hex = (n: number) => n.toString(16).padStart(2, "0").toUpperCase();
        return `avc1.${hex(au[off + 1])}${hex(au[off + 2])}${hex(au[off + 3])}`;
      }
      i = off;
    }
    return null;
  }

  // Follow the live video route: watch its packet channel while it's up,
  // and show the placeholder rather than a stale frame whenever it
  // changes (input switch, session end).
  $effect(() => {
    const route = app.consoleVideoLive;
    // Reading this here makes the ladder's last rung re-run the effect:
    // flipping it tears the watch down and re-watches in native mode.
    const native = nativeDecode;
    hasFrame = false;
    hostStatus = null;
    fps = 0;
    transport = "";
    decodeFails = 0;
    pipeDiag = "";
    frameCount = 0;
    inCount = 0;
    if (!route) return;
    let cancelled = false;
    let unwatch: (() => void) | undefined;
    let unwatchStatus: (() => void) | undefined;
    // The host's capture-state reports for this route — they explain the
    // placeholder (and any mid-stream stall) in the host's own words.
    void watchVideoStatus(route, (s) => {
      if (cancelled) return;
      hostStatus = s.state === "ok" ? null : s;
    }).then((u) => {
      if (cancelled) u();
      else unwatchStatus = u;
    });
    let decoder: VideoDecoder | null = null;
    let codecString: string | null = null;
    // The decode ladder: hardware-preference first; any stall (born dead
    // *or* mid-stream — the hardware-pool failure shape is bursts, then a
    // growing queue with no output and no error) rebuilds the decoder one
    // rung down on software decode, and a stall *there* hands the stream
    // to the backend's native decoder. The chip's diag line records the
    // step.
    let decodeMode: HardwareAcceleration = "no-preference";
    let decodeCalls = 0;
    let decodeOutputs = 0;
    // Decoded frames don't paint inside the output callback: the freshest
    // one is parked here (superseded frames close immediately — freshness
    // over completeness) and a rAF paints it. The decoder's frame pool can
    // never be starved by the paint path, throttled window or not.
    let pendingFrame: VideoFrame | null = null;
    let paintScheduled = false;
    queuePeek = () => decoder?.decodeQueueSize ?? 0;
    decodeModeNote = native ? "native" : "";

    const rebuildDecoder = () => {
      if (decodeMode === "no-preference") {
        decodeMode = "prefer-software";
        decodeModeNote = "sw";
      } else {
        // Software decode stalled too — hand the stream to the backend's
        // openh264 decoder. Setting the flag re-runs this effect, which
        // re-watches the route in native mode (and tears this rung down).
        console.warn(`video decoder (${codecString}) stalled twice — switching to native decode`);
        nativeDecode = true;
        askRefresh();
        return;
      }
      console.warn(
        `video decoder (${codecString}) stalled — rebuilding with ${decodeMode}`,
      );
      try {
        if (decoder && decoder.state !== "closed") decoder.close();
      } catch {
        // already closed
      }
      decoder = null; // re-created on the next key unit (≤2s away)
    };
    stallKick = () => {
      if (!cancelled && decoder) rebuildDecoder();
    };

    const paintPending = () => {
      paintScheduled = false;
      const frame = pendingFrame;
      pendingFrame = null;
      if (!frame) return;
      try {
        if (!cancelled) {
          paint(frame.displayWidth, frame.displayHeight, (ctx) =>
            ctx.drawImage(frame, 0, 0),
          );
        }
      } finally {
        frame.close();
      }
    };

    // JPEG bitmap decodes are async — chain them so frames paint in
    // arrival order.
    let drawChain = Promise.resolve();

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
      frameCount += 1;
    };

    const dropDecoder = (why: unknown) => {
      // Surfaced, not swallowed: the chip counts these and the console
      // log names them — a decoder that quietly dies reads as a freeze.
      decodeFails += 1;
      askRefresh();
      console.warn("video decode error:", why);
      try {
        if (decoder && decoder.state !== "closed") decoder.close();
      } catch {
        // already closed
      }
      decoder = null;
    };

    void watchVideo(
      route,
      (f) => {
      if (cancelled) return;
      transport = f.kind === "jpeg" ? "MJPEG" : "H.264";
      if (f.kind === "jpeg") {
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
        return;
      }
      if (f.kind === "raw") {
        // The backend already decoded — RGBA in, one blit out.
        inCount += 1;
        if (f.data.byteLength !== f.width * f.height * 4) return;
        const pixels = new Uint8ClampedArray(f.data.buffer, f.data.byteOffset, f.data.byteLength);
        const img = new ImageData(pixels, f.width, f.height);
        paint(f.width, f.height, (ctx) => ctx.putImageData(img, 0, 0));
        return;
      }
      // H.264 — decode entry is a key unit; deltas before one wait.
      inCount += 1;
      if (!decoder || decoder.state === "closed") {
        if (!f.key) return;
        codecString = spsCodecString(f.data) ?? codecString ?? "avc1.42E01F";
        decoder = new VideoDecoder({
          output: (frame) => {
            decodeOutputs += 1;
            if (pendingFrame) pendingFrame.close();
            pendingFrame = frame;
            if (!paintScheduled) {
              paintScheduled = true;
              requestAnimationFrame(paintPending);
            }
          },
          // Recovery is the sender's periodic IDR: drop the decoder,
          // re-init on the next key unit.
          error: dropDecoder,
        });
        try {
          decoder.configure({
            codec: codecString,
            optimizeForLatency: true,
            hardwareAcceleration: decodeMode,
          });
          decodeCalls = 0;
          decodeOutputs = 0;
        } catch (e) {
          dropDecoder(e);
          return;
        }
      }
      try {
        decoder.decode(
          new EncodedVideoChunk({
            type: f.key ? "key" : "delta",
            timestamp: f.seq,
            data: f.data,
          }),
        );
        decodeCalls += 1;
      } catch (e) {
        dropDecoder(e);
        return;
      }
      // Born-dead decoders (key accepted, nothing ever out) get the same
      // rebuild as mid-stream stalls — without waiting for the 1s sweep.
      if (decodeOutputs === 0 && decodeCalls >= 20) rebuildDecoder();
      },
      { decode: native },
    ).then((u) => {
      // The route may have changed while the subscribe was in flight.
      if (cancelled) u();
      else unwatch = u;
    });
    return () => {
      cancelled = true;
      unwatch?.();
      unwatchStatus?.();
      stallKick = () => {};
      if (pendingFrame) {
        pendingFrame.close();
        pendingFrame = null;
      }
      try {
        if (decoder && decoder.state !== "closed") decoder.close();
      } catch {
        // already closed
      }
      decoder = null;
    };
  });

  /** The host's capture condition as a human sentence — what the stage
   *  shows instead of (or over) silent black. */
  function hostStatusText(s: VideoHostStatus): string {
    switch (s.state) {
      case "waiting_consent":
        return "Waiting for someone at the remote machine to approve screen sharing (a one-time consent dialog is open there).";
      case "display_asleep":
        return "The remote display is asleep or blank — nudging it awake…";
      case "no_monitor":
        return "No monitor to capture on the remote machine — its displays are detached or in deep sleep.";
      case "grab_failed":
        return `Screen capture is failing on the remote machine${s.detail ? `: ${s.detail}` : "."}`;
      default:
        return "";
    }
  }

  let closing = false;
  async function endSession() {
    if (closing) return;
    closing = true;
    // UI resets synchronously; the await is only so a console window's
    // teardown reaches the backend before the webview dies. Bounded — a
    // wedged daemon must never hold a closing window hostage.
    const teardown = app.closeConsole();
    if (windowed) {
      await Promise.race([teardown, new Promise((r) => setTimeout(r, 600))]);
      void closeThisWindow();
    }
    closing = false;
  }

  // ---- keyboard & mouse forwarding -----------------------------------
  //
  // Coordinates are normalized 0..1 over the *streamed frame's* content
  // box (the canvas is letterboxed with object-fit: contain), which the
  // remote denormalizes onto its own screen — neither side needs the
  // other's resolution.
  function normPoint(e: PointerEvent | WheelEvent): { x: number; y: number } | null {
    const img = canvasEl;
    if (!img || !frameW || !frameH) return null;
    const r = img.getBoundingClientRect();
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

  // Pointer moves stream constantly; cap at ~60/s — the events are tiny
  // and the finer cadence keeps remote cursor motion feeling direct.
  let lastMoveAt = 0;
  function onPointerMove(e: PointerEvent) {
    if (!app.consoleControl) return;
    const now = performance.now();
    if (now - lastMoveAt < 16) return;
    const p = normPoint(e);
    if (!p) return;
    lastMoveAt = now;
    app.sendConsoleInput({ kind: "mouse_move", ...p, screen: controlScreen });
  }

  function onPointerButton(e: PointerEvent, down: boolean) {
    if (!app.consoleControl) return;
    const p = normPoint(e);
    if (!p) return;
    e.preventDefault();
    // Land the cursor exactly where the click happened, then click.
    app.sendConsoleInput({ kind: "mouse_move", ...p, screen: controlScreen });
    app.sendConsoleInput({ kind: "mouse_button", button: e.button, down });
  }

  function onWheel(e: WheelEvent) {
    if (!app.consoleControl || !normPoint(e)) return;
    e.preventDefault();
    // Normalize the browser's delta modes to wheel lines.
    const lines = e.deltaMode === 1 ? 1 : 1 / 40;
    app.sendConsoleInput({ kind: "wheel", dx: e.deltaX * lines, dy: e.deltaY * lines });
  }

  function onKey(e: KeyboardEvent, down: boolean) {
    if (!node) return;
    if (!app.consoleControl) {
      // Without control, Escape closes the session (popover habit; in a
      // window it closes the window too).
      if (down && e.key === "Escape") endSession();
      return;
    }
    // With control on, *every* key belongs to the remote — including
    // Escape, exactly like sitting at the machine.
    e.preventDefault();
    if (e.repeat) return; // the remote OS does its own key repeat
    app.sendConsoleInput({ kind: "key", key: e.key, down });
  }

  function inputIcon(c: Capability): string {
    return originIcon(c.origin, c.media);
  }
</script>

<svelte:window
  onkeydown={(e) => onKey(e, true)}
  onkeyup={(e) => onKey(e, false)}
  onclick={() => (openPill = null)}
/>

{#if node}
  <div class="scrim" class:windowed>
    {#if !windowed}
      <button class="backdrop" aria-label="Close console" onclick={endSession}></button>
    {/if}
    <div
      class="console"
      role="dialog"
      aria-modal={!windowed}
      aria-label="Console for {displayName(node)}"
    >
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
        <button class="x" onclick={endSession} aria-label="Close">✕</button>
      </header>

      <!-- Video stage -->
      <!-- role=application: a remote-desktop surface — every pointer/key
           event belongs to the far machine while control is on. -->
      <div
        class="stage"
        class:grabbing={app.consoleControl}
        role="application"
        aria-label="Remote screen — input is forwarded while keyboard & mouse control is on"
        onpointermove={onPointerMove}
        onpointerdown={(e) => onPointerButton(e, true)}
        onpointerup={(e) => onPointerButton(e, false)}
        onwheel={onWheel}
        oncontextmenu={(e) => app.consoleControl && e.preventDefault()}
      >
        {#if app.consoleVideoLive}
          <canvas
            bind:this={canvasEl}
            class="live"
            class:waiting={!hasFrame}
            aria-label="Live screen of {displayName(node)}"
          ></canvas>
        {/if}
        {#if hasFrame}
          <!-- the canvas above is the stage; a host-reported stall (the
               remote display sleeping mid-session) banners over it. -->
          {#if hostStatus}
            <div class="host-status">{hostStatusText(hostStatus)}</div>
          {/if}
        {:else if selected}
          <div class="screen" style="--mc: {mediaColor(selected.media)}">
            <div class="screen-glyph">{inputIcon(selected)}</div>
            <div class="screen-title">{selected.label}</div>
            {#if selected.media === "display"}
              <div class="screen-note">
                {hostStatus ? hostStatusText(hostStatus) : "Connecting this machine's display…"}
              </div>
            {:else}
              <div class="screen-note">
                Camera input selected — camera streaming is the next transport to land.
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
      {#snippet pillMenu(
        key: "res" | "fps" | "rate",
        name: string,
        choices: PillChoice[],
        current: number | null | undefined,
        pick: (v: number | null) => void,
      )}
        <span class="pill-wrap">
          <button
            class="pill"
            class:tuned={(current ?? null) !== null}
            onclick={(e) => {
              e.stopPropagation();
              openPill = openPill === key ? null : key;
            }}
          >
            {name} · {pillLabel(choices, current)} ▾
          </button>
          {#if openPill === key}
            <div class="pill-menu" role="menu">
              {#each choices as c (c.label)}
                <button
                  class="pill-item"
                  class:sel={(current ?? null) === c.value}
                  onclick={(e) => {
                    e.stopPropagation();
                    pick(c.value);
                  }}
                >
                  {c.label}
                </button>
              {/each}
            </div>
          {/if}
        </span>
      {/snippet}
      <footer class="controls">
        <div class="toggles">
          <button
            class="toggle"
            class:on={app.consoleAudio}
            onclick={() => app.toggleConsoleAudio()}
            title="Play that machine's audio on this machine (listen-only — nothing is sent back)"
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

        {#if app.consoleVideoLive}
          <div class="pills" role="group" aria-label="Stream quality">
            {@render pillMenu("res", "Res", RES_CHOICES, app.consoleTune.maxEdge, pickRes)}
            {@render pillMenu("fps", "FPS", FPS_CHOICES, app.consoleTune.fps, pickFps)}
            {@render pillMenu("rate", "Rate", RATE_CHOICES, app.consoleTune.bitrate, pickRate)}
            <span class="pill-wrap">
              <button
                class="pill"
                class:tuned={codecChoice !== "auto"}
                onclick={(e) => {
                  e.stopPropagation();
                  openPill = openPill === "codec" ? null : "codec";
                }}
              >
                Codec · {CODEC_CHOICES.find((c) => c.value === codecChoice)?.label ?? "Auto"} ▾
              </button>
              {#if openPill === "codec"}
                <div class="pill-menu" role="menu">
                  {#each CODEC_CHOICES as c (c.value)}
                    <button
                      class="pill-item"
                      class:sel={codecChoice === c.value}
                      onclick={(e) => {
                        e.stopPropagation();
                        pickCodec(c.value);
                      }}
                    >
                      {c.label}
                    </button>
                  {/each}
                </div>
              {/if}
            </span>
          </div>
        {/if}

        <div class="status">
          {#if hasFrame}
            <span class="chip stream" title="Live stream — frame size · rate">
              <span class="chip-dot live-dot"></span>{frameW}×{frameH} · {fps} fps · {transport}{decodeFails
                ? ` · ${decodeFails} decode-err`
                : ""}{pipeDiag ? ` · ⚠ ${pipeDiag}` : ""}
            </span>
          {/if}
          {#each app.consoleSessionRoutes as r (r.id)}
            <span class="chip" style="--mc: {mediaColor(r.media as MediaKind)}">
              <span class="chip-dot"></span>{MEDIA[r.media as MediaKind].label}
            </span>
          {/each}
          {#if app.consoleSessionRoutes.length === 0}
            <span class="muted">No active links yet</span>
          {/if}
        </div>

        <button class="btn end" onclick={endSession}>End session</button>
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
  /* A dedicated console window: no overlay, the console *is* the window. */
  .scrim.windowed {
    background: #14121f;
    backdrop-filter: none;
    padding: 0;
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
  .windowed .console {
    width: 100%;
    height: 100%;
    border: none;
    border-radius: 0;
    box-shadow: none;
    animation: none;
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
    /* Anchors the .host-status banner. */
    position: relative;
    display: grid;
    /* The single track must be the stage's size, never the content's:
       an auto track grows to the canvas's intrinsic width (1920), which
       overflowed narrower windows and clipped the sides — the letterbox
       must come from object-fit inside the element, both axes. */
    grid-template-rows: minmax(0, 1fr);
    grid-template-columns: minmax(0, 1fr);
    place-items: center;
    padding: 1rem;
    background:
      radial-gradient(1200px 400px at 50% -10%, rgba(108, 92, 231, 0.12), transparent),
      repeating-linear-gradient(0deg, #100e1a, #100e1a 2px, #12101c 2px, #12101c 4px);
  }
  .stage.grabbing {
    cursor: crosshair;
  }
  .live {
    max-width: 100%;
    max-height: 100%;
    width: 100%;
    height: 100%;
    object-fit: contain;
    user-select: none;
    -webkit-user-drag: none;
    border-radius: 4px;
    box-shadow: 0 6px 30px rgba(0, 0, 0, 0.5);
  }
  /* Mounted (so the first frame has somewhere to land) but invisible
     until it does — the placeholder shows through. */
  .live.waiting {
    visibility: hidden;
    position: absolute;
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
  /* The host's capture condition, bannered over a live stage when the
     stream stalls mid-session (display fell asleep, grabs failing). */
  .host-status {
    position: absolute;
    left: 50%;
    bottom: 1.4rem;
    transform: translateX(-50%);
    max-width: min(34rem, 85%);
    padding: 0.45rem 0.85rem;
    border-radius: 0.55rem;
    background: rgba(26, 23, 48, 0.92);
    border: 1px solid #2c2740;
    color: #c7c0e2;
    font-size: 0.8rem;
    line-height: 1.4;
    text-align: center;
    pointer-events: none;
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
  .pills {
    display: flex;
    gap: 0.3rem;
    flex-shrink: 0;
  }
  .pill-wrap {
    position: relative;
    display: inline-flex;
  }
  .pill {
    border: 1px solid #322c47;
    background: #14121f;
    color: #c8c2e0;
    border-radius: var(--r-pill);
    padding: 0.32rem 0.55rem;
    font-size: 0.72rem;
    font-weight: 650;
    cursor: pointer;
    white-space: nowrap;
    transition: border-color 0.12s ease;
  }
  .pill:hover {
    border-color: var(--accent);
  }
  /* A dial off Auto reads as deliberately set. */
  .pill.tuned {
    border-color: var(--accent);
    color: #e6e1ff;
  }
  .pill-menu {
    position: absolute;
    bottom: calc(100% + 6px);
    left: 0;
    min-width: 100%;
    display: flex;
    flex-direction: column;
    background: #1a1730;
    border: 1px solid #322c47;
    border-radius: var(--r-md);
    box-shadow: var(--shadow-lg);
    padding: 0.25rem;
    z-index: 20;
  }
  .pill-item {
    border: none;
    background: transparent;
    color: #c8c2e0;
    text-align: left;
    font-size: 0.76rem;
    font-weight: 600;
    padding: 0.32rem 0.6rem;
    border-radius: var(--r-sm, 6px);
    cursor: pointer;
    white-space: nowrap;
  }
  .pill-item:hover {
    background: #241f38;
    color: #fff;
  }
  .pill-item.sel {
    color: var(--accent);
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
  .chip.stream {
    color: #c7efdb;
    border-color: rgba(26, 160, 109, 0.5);
  }
  .live-dot {
    background: var(--ok);
    animation: blink 1.6s ease-in-out infinite;
  }
  @keyframes blink {
    50% {
      opacity: 0.35;
    }
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
