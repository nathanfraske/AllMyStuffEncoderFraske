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
  import { makeKeyForwarder } from "../input-keys";
  import { app } from "../store.svelte";
  import {
    closeThisWindow,
    focusThisWindow,
    isTauri,
    onThisWindowClose,
    refreshRoute,
    sendVideoFeedback,
    toggleWindowFullscreen,
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
  // What this machine actually shared with us — the console activates with
  // whatever subset is available and hides the toggles for the rest (a
  // screen-only share shows the screen, no inert Audio/Control buttons).
  const access = $derived(
    node
      ? app.consoleAccess(node)
      : { remote: false, files: false, terminal: false, sites: false, audio: false, control: false, clipboard: false },
  );
  const inputs = $derived(node ? app.consoleVideoInputs(node.id) : []);
  const selectedId = $derived(app.consoleInput);
  const selected = $derived<Capability | null>(
    (selectedId ? app.capability(selectedId) : null) ?? null,
  );
  // Auto-select the first input (the screen leads the list) once the remote's
  // video sources land. The console opens before the caps arrive, so the
  // open-time pick can come up empty; pick reactively when they show up. Guarded
  // on nothing being selected, so it never overrides a manual pick.
  $effect(() => {
    if (!selectedId && inputs.length > 0) app.setConsoleInput(inputs[0].id);
  });
  // Whether the machine's build streams its cameras at all — an older one
  // advertises the tabs but has no transport behind them, and the stage
  // says so instead of waiting on pixels that never come.
  const cameraSupported = $derived(node ? app.cameraStreamSupported(node) : false);
  // The selected input is off in its own popout window — the stage shows
  // the big "Return video here" instead of competing for the stream.
  const selectedPopped = $derived(!!selectedId && app.isVideoPopped(`cap:${selectedId}`));
  // Pointer forwarding is live only over a *desktop* picture: coordinates
  // normalize onto the streamed frame, and only a screen's frame maps
  // back onto the remote desktop — a camera tab is a viewing surface.
  // (Keys keep the session rule: with control on they always belong to
  // the remote, whichever tab is showing.)
  const stagePointerActive = $derived(app.consoleControl && selected?.media === "display");
  // Fullscreen ("theater"): the stage takes the whole window over (bars
  // and tabs hidden), and — windowed — the OS window goes fullscreen too,
  // so exactly this video fills the screen. Esc exits while control is
  // off; with control on every key belongs to the remote, so the hover
  // ⛶ is the way out.
  let theater = $state(false);

  async function flipTheater() {
    theater = !theater;
    if (windowed) await toggleWindowFullscreen();
    // The OS fullscreen transition blurs the stage (firing keys.releaseAll via
    // onblur), and claimFocus won't re-pin once the window itself holds focus —
    // so with control on, re-pin here or keyboard forwarding silently stops
    // until the user clicks back into the picture ("controls stopped mapping").
    if (app.consoleControl) stageEl?.focus({ preventScroll: true });
  }
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
  // The role=application stage. Control key forwarding is bound here (not to
  // `<svelte:window>`) so it fires whenever the stage holds focus — and
  // hover/click/toggle pin that focus, the reliable way a dedicated console
  // window's keyboard reaches the remote (window-level setFocus alone doesn't
  // push document focus into the webview on hover).
  let stageEl = $state<HTMLElement | null>(null);
  let hasFrame = $state(false);
  // The host's word on why pixels aren't flowing (`vstat`): shown on the
  // placeholder before the first frame, and as a banner if the stream
  // stalls after one. Null = no condition reported (or it cleared).
  let hostStatus = $state<VideoHostStatus | null>(null);
  // The video route was refused, or its offer expired unanswered (the far
  // side's app isn't running, or it NACKed a route it no longer holds) —
  // the reason replaces "Connecting…" so a dead stage explains itself.
  const videoRefused = $derived.by(() => {
    const live = app.consoleVideoLive;
    if (!live) return null;
    const st = app.routeStates[live];
    return st?.state === "rejected" ? st.reason || "the far side refused the stream" : null;
  });
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
  // Starts true where WebCodecs doesn't exist at all, and flips true at
  // mount when it exists but can't actually decode H.264 (the
  // isConfigSupported probe — WebKitGTK ships the API shape with the
  // codecs delegated to GStreamer plugins that usually aren't there).
  // The decode ladder also lands here after WebCodecs stalls or dies
  // repeatedly. Sticky for the session — a webview whose decoder wedged
  // once isn't owed a third chance.
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
  let openPill = $state<"res" | "fps" | "rate" | "codec" | "aspect" | null>(null);

  // ---- source aspect (mouse letterbox correction) --------------------
  //
  // A machine whose native resolution isn't 16:9 gets letterboxed into the
  // capture; the mouse must map over the desktop inside the bars. "Auto" reads
  // the bars off the picture (detectActiveRegion); an explicit aspect computes
  // the exact symmetric bars with no sampling — the confident manual path. Like
  // the codec pill, this is a pills-only control (not driven by the slider).
  const ASPECTS: Array<{ label: string; value: string; ratio: number | null }> = [
    { label: "Auto", value: "auto", ratio: null },
    { label: "16:9", value: "16:9", ratio: 16 / 9 },
    { label: "16:10", value: "16:10", ratio: 16 / 10 },
    { label: "3:2", value: "3:2", ratio: 3 / 2 },
    { label: "4:3", value: "4:3", ratio: 4 / 3 },
    { label: "21:9", value: "21:9", ratio: 21 / 9 },
  ];
  let aspectChoice = $state<string>(
    (typeof localStorage !== "undefined" && localStorage.getItem("ams.consoleAspect")) || "auto",
  );
  function pickAspect(v: string) {
    aspectChoice = v;
    openPill = null;
    try {
      localStorage.setItem("ams.consoleAspect", v);
    } catch {
      // storage disabled — the pick still applies for this session
    }
  }
  const aspectLabel = $derived(ASPECTS.find((a) => a.value === aspectChoice)?.label ?? "Auto");
  // The codec pill reflects the *selected source's* transport (per-source in
  // the store) plus this window's decode choice — so switching sources shows
  // that source's codec, and "native" stays a window-local decode preference.
  const codecChoice = $derived<CodecChoice>(
    app.consoleCodec === "mjpeg"
      ? "mjpeg"
      : app.consoleCodec === "auto"
        ? "auto"
        : nativeDecode
          ? "native"
          : "h264",
  );

  // The Speed↔Quality slider: one knob that snaps to a preset curve of
  // res/fps/rate. Codec stays Auto here — forcing a codec is a pills-only
  // choice. Each stop reuses the same values the pills offer.
  const QUALITY_STOPS = [
    { label: "Speed", maxEdge: 1280, fps: 24, bitrate: 4_000_000 },
    { label: "Smooth", maxEdge: 1920, fps: 30, bitrate: 8_000_000 },
    { label: "Balanced", maxEdge: 1920, fps: 60, bitrate: 15_000_000 },
    { label: "Crisp", maxEdge: 2560, fps: 60, bitrate: 25_000_000 },
    { label: "Quality", maxEdge: 3840, fps: 60, bitrate: 40_000_000 },
  ];
  // Where the slider sits for the live tune: the nearest stop by resolution,
  // defaulting to Balanced when everything is Auto — so it reflects reality
  // on open and on a source switch.
  const sliderPos = $derived.by(() => {
    const t = app.consoleTune;
    if (t.maxEdge == null && t.fps == null && t.bitrate == null) return 2;
    let best = 2;
    let bestD = Infinity;
    QUALITY_STOPS.forEach((s, i) => {
      const d = Math.abs((t.maxEdge ?? s.maxEdge) - s.maxEdge);
      if (d < bestD) {
        bestD = d;
        best = i;
      }
    });
    return best;
  });
  function pickQuality(i: number) {
    const s = QUALITY_STOPS[i];
    app.setConsoleTune({ maxEdge: s.maxEdge, fps: s.fps, bitrate: s.bitrate });
  }

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
    // 300 ms floor (was 700): a re-key recovers visible corruption, so ask
    // fast. The backend throttles again (300 ms) so a storm can't form.
    if (now - lastRefreshAsk < 300) return;
    lastRefreshAsk = now;
    void refreshRoute(route);
  }

  onMount(() => {
    let unlistenClose: (() => void) | undefined;
    // "VideoDecoder exists" stopped meaning "H.264 decode works":
    // WebKitGTK 2.4x ships the WebCodecs shape with codec support
    // delegated to GStreamer plugins that usually aren't installed. Ask
    // the API itself; anything short of a clean "supported" starts the
    // session on native decode instead of feeding a decoder that can
    // only die. (A probe that *lies* is still caught by the born-dead
    // ladder below — this just skips the few seconds it costs.)
    if (!nativeDecode && typeof VideoDecoder !== "undefined") {
      try {
        void VideoDecoder.isConfigSupported({ codec: "avc1.42E01F" })
          .then((s) => {
            if (!s.supported) nativeDecode = true;
          })
          .catch(() => (nativeDecode = true));
      } catch {
        nativeDecode = true;
      }
    }
    let fbTick = 0;
    let fbFailsSent = 0;
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
      // Auto aspect: measure the frame's active region (letterbox bars) until
      // it locks, so the mouse maps over the desktop of a non-16:9 source. A
      // manual Aspect pick owns the region instead (see the $effect).
      if (stagePointerActive && aspectChoice === "auto") detectActiveRegion();
      // Every other tick, report our decode health back to the streamer so
      // it can adapt (receiver → sender). decode_fails is the delta since the
      // last report; recv_fps is what we actually painted.
      const fbRoute = app.consoleVideoLive;
      if (fbRoute && ++fbTick % 2 === 0) {
        void sendVideoFeedback(fbRoute, fps, decodeFails - fbFailsSent, queuePeek());
        fbFailsSent = decodeFails;
      }
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
    // Clear the previous stream's frame dimensions too: normPoint letterboxes
    // against frameW/frameH, so leaving them live would map pointer events onto
    // the OLD source's aspect (and a hidden, repositioned canvas) during a
    // re-wire — the "mouse doesn't map cleanly after a transition" report. The
    // hasFrame gate below then drops events until the first fresh frame repaints.
    frameW = 0;
    frameH = 0;
    // A new source may have a different (or no) letterbox — start from the full
    // frame, unlock, and let the one-shot detector re-measure and re-lock.
    activeRegion = { x0: 0, y0: 0, x1: 1, y1: 1 };
    detectLocked = false;
    detectPrev = null;
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
    // Start on GPU decode (VideoToolbox/D3D11/VA-API behind WebCodecs) — it's
    // the lowest-latency, lowest-CPU rung. The ladder steps down to software
    // then native on a stall, so a box without HW decode still recovers.
    let decodeMode: HardwareAcceleration = "prefer-hardware";
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
      if (decodeMode !== "prefer-software") {
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

    // Decoder instances lost on this rung before they ever produced a
    // frame. The queue-stall detectors above can't see this failure
    // shape: a decoder whose configure() throws — or whose very first
    // decode errors — dies and resets the counters each time, so the
    // ladder never stepped and the stage sat black forever. WebKitGTK
    // 2.4x is the live case: it exposes the VideoDecoder *shape* with no
    // working H.264 behind it (GStreamer-dependent), so the old "does
    // VideoDecoder exist" test chose a decoder that can only die.
    let bornDeadDrops = 0;

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
      // Nothing ever decoded on this rung and the decoder keeps dying:
      // that's a rung failure, not a glitch — walk the same ladder the
      // stall detectors use, which ends at the backend's native decoder.
      if (decodeOutputs === 0) {
        bornDeadDrops += 1;
        if (bornDeadDrops >= 3) {
          bornDeadDrops = 0;
          rebuildDecoder();
        }
      }
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
        return "The remote display is asleep or blank — forcing it awake (clicking here helps too)…";
      case "no_monitor":
        return "No monitor to capture on the remote machine — its displays are detached or in deep sleep.";
      case "grab_failed":
        return `Screen capture is failing on the remote machine${s.detail ? `: ${s.detail}` : "."}`;
      case "no_camera":
        return "No camera to capture on the remote machine — it may have been unplugged since the scan.";
      case "camera_failed":
        return `The remote camera won't stream — another app may be holding it, or its camera permission is off${s.detail ? ` (${s.detail})` : ""}.`;
      default:
        return "";
    }
  }

  let closing = false;
  async function endSession() {
    if (closing) return;
    closing = true;
    // Keys still held ride out *before* the control route goes — closing
    // a console mid-chord (⌘W closes this very window) must not leave
    // the remote holding the modifier.
    keys.releaseAll();
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
    // Gate on hasFrame, not just truthy frameW/frameH: during a re-wire the
    // canvas is `.waiting` (visibility:hidden; position:absolute), so its rect
    // has moved out of the centered grid cell — normalizing against it (or the
    // stale prior dims) lands the remote cursor in the wrong place. Wait for the
    // first fresh frame, then the live-rect letterbox math below maps cleanly.
    if (!img || !hasFrame || !frameW || !frameH) return null;
    const r = img.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) return null;
    const scale = Math.min(r.width / frameW, r.height / frameH);
    const cw = frameW * scale;
    const ch = frameH * scale;
    const ox = r.left + (r.width - cw) / 2;
    const oy = r.top + (r.height - ch) / 2;
    // Fraction over the streamed FRAME.
    const fx = (e.clientX - ox) / cw;
    const fy = (e.clientY - oy) / ch;
    if (fx < 0 || fx > 1 || fy < 0 || fy > 1) return null;
    // Remap over the ACTIVE region — inside any baked-in letterbox bars. A
    // source whose native aspect ≠ the 16:9 capture (e.g. a 16:10 laptop output
    // as 1080p) bakes black bars into the frame, and the remote's absolute
    // pointer maps over its DESKTOP, not the whole frame — so mapping over the
    // full frame lands the cursor off by the bar width. activeRegion is the
    // detected desktop box (0..1), or the full frame when there are no clear,
    // symmetric bars. Bar-hits clamp to the desktop edge.
    const ar = activeRegion;
    const x = Math.min(1, Math.max(0, (fx - ar.x0) / (ar.x1 - ar.x0)));
    const y = Math.min(1, Math.max(0, (fy - ar.y0) / (ar.y1 - ar.y0)));
    return { x, y };
  }

  // ---- letterbox / active-area detection -----------------------------
  //
  // The active region of the streamed frame in 0..1 fractions (the desktop
  // inside any baked-in black bars); the full frame until detection runs.
  let activeRegion = $state({ x0: 0, y0: 0, x1: 1, y1: 1 });
  // The bars are baked pixels with no sidechannel (HDMI reports only the signal
  // size, the SPS only the coded size), so they must be measured off the frame —
  // but they're STATIC for a source mode, so this is a one-shot: it measures on
  // the health tick only until two content-bearing frames agree, then LOCKS and
  // stops sampling. Reset (unlock) on a stream re-wire.
  let detectLocked = false;
  let detectPrev: { x0: number; x1: number; y0: number; y1: number } | null = null;

  // Measure the frame's active region: sample the decoded canvas for symmetric
  // black letterbox/pillarbox bars. Cheap (a dozen 1px strips), and it stops
  // once locked. Conservative: only crops when clear bars sit on BOTH opposite
  // edges and are near-symmetric (a real letterbox), so ordinary dark content is
  // never mistaken for a bar; otherwise it maps over the whole frame.
  function detectActiveRegion() {
    if (detectLocked) return;
    const c = canvasEl;
    if (!c || !c.width || !c.height || !hasFrame) return;
    const ctx = c.getContext("2d", { willReadFrequently: true });
    if (!ctx) return;
    const w = c.width;
    const h = c.height;
    const DARK = 24;
    const bright = (d: Uint8ClampedArray, px: number) =>
      d[px * 4] > DARK || d[px * 4 + 1] > DARK || d[px * 4 + 2] > DARK;
    const median = (a: number[]) => a.slice().sort((p, q) => p - q)[a.length >> 1];

    let x0 = 0;
    let x1 = w;
    let y0 = 0;
    let y1 = h;
    let found = false; // did the frame carry real (non-black) content this pass?
    try {
      const L: number[] = [];
      const R: number[] = [];
      for (let k = 1; k <= 6; k++) {
        const y = Math.floor((h * k) / 7);
        const row = ctx.getImageData(0, y, w, 1).data;
        let l = 0;
        while (l < w && !bright(row, l)) l++;
        let rr = w - 1;
        while (rr > l && !bright(row, rr)) rr--;
        if (l < rr) {
          L.push(l);
          R.push(rr);
        }
      }
      const T: number[] = [];
      const B: number[] = [];
      for (let k = 1; k <= 6; k++) {
        const x = Math.floor((w * k) / 7);
        const col = ctx.getImageData(x, 0, 1, h).data;
        let t = 0;
        while (t < h && !bright(col, t)) t++;
        let b = h - 1;
        while (b > t && !bright(col, b)) b--;
        if (t < b) {
          T.push(t);
          B.push(b);
        }
      }
      found = L.length >= 4;
      if (L.length >= 4) {
        x0 = median(L);
        x1 = median(R) + 1;
      }
      if (T.length >= 4) {
        y0 = median(T);
        y1 = median(B) + 1;
      }
    } catch {
      return; // canvas not readable this tick — keep the last region
    }

    // A near-all-black frame (screensaver, dark boot screen) tells us nothing —
    // don't measure or lock on it, just try again on the next tick.
    if (!found) {
      detectPrev = null;
      return;
    }

    const barL = x0;
    const barR = w - x1;
    const barT = y0;
    const barB = h - y1;
    const okX = Math.min(barL, barR) > w * 0.015 && Math.abs(barL - barR) < w * 0.02;
    const okY = Math.min(barT, barB) > h * 0.015 && Math.abs(barT - barB) < h * 0.02;
    const next = {
      x0: okX ? x0 / w : 0,
      x1: okX ? x1 / w : 1,
      y0: okY ? y0 / h : 0,
      y1: okY ? y1 / h : 1,
    };
    activeRegion = next;
    // Lock once two consecutive content-bearing frames agree — then stop
    // sampling entirely until the next re-wire resets it.
    const near = (a: number, b: number) => Math.abs(a - b) < 0.005;
    if (
      detectPrev &&
      near(detectPrev.x0, next.x0) &&
      near(detectPrev.x1, next.x1) &&
      near(detectPrev.y0, next.y0) &&
      near(detectPrev.y1, next.y1)
    ) {
      detectLocked = true;
    }
    detectPrev = next;
  }

  // The exact symmetric active region for a known source aspect within the
  // current frame: a source narrower than the frame pillarboxes (L/R bars),
  // a wider one letterboxes (T/B bars).
  function activeRegionForAspect(ratio: number, fw: number, fh: number) {
    const frameRatio = fw / fh;
    if (ratio < frameRatio - 1e-4) {
      const x0 = (1 - ratio / frameRatio) / 2;
      return { x0, x1: 1 - x0, y0: 0, y1: 1 };
    }
    if (ratio > frameRatio + 1e-4) {
      const y0 = (1 - frameRatio / ratio) / 2;
      return { x0: 0, x1: 1, y0, y1: 1 - y0 };
    }
    return { x0: 0, y0: 0, x1: 1, y1: 1 };
  }

  // Apply the Aspect pick: "Auto" hands the active region back to the one-shot
  // pixel detector; an explicit aspect computes the exact bars and disables
  // detection. Re-runs when the pick or the frame geometry changes.
  $effect(() => {
    const choice = aspectChoice;
    const fw = frameW;
    const fh = frameH;
    const ratio = ASPECTS.find((a) => a.value === choice)?.ratio ?? null;
    if (ratio == null) {
      // Auto — unlock and let detectActiveRegion re-measure.
      detectLocked = false;
      detectPrev = null;
      activeRegion = { x0: 0, y0: 0, x1: 1, y1: 1 };
      return;
    }
    detectLocked = true; // stop the detector; the aspect is authoritative
    activeRegion = fw && fh ? activeRegionForAspect(ratio, fw, fh) : { x0: 0, y0: 0, x1: 1, y1: 1 };
  });

  // The KVM rule: with control live, the window under the mouse is the one
  // your keyboard should reach — claim focus on hover, no click in between (a
  // click would go to the *remote*). Raise the OS window AND pin keyboard
  // focus on the stage element: setFocus() alone doesn't reliably push
  // document focus into the webview on hover-without-click, so without the
  // element focus the key handlers (now on the stage) never fire in a
  // dedicated console window. Gated on the document not already holding focus
  // so it never steals focus from an open pill menu once the window is active.
  function claimFocus() {
    if (document.hasFocus()) return;
    void focusThisWindow();
    stageEl?.focus({ preventScroll: true });
  }

  // Pointer moves stream constantly; cap at ~60/s — the events are tiny
  // and the finer cadence keeps remote cursor motion feeling direct.
  let lastMoveAt = 0;
  function onPointerMove(e: PointerEvent) {
    // Keep keyboard focus on the stage whenever control is on (even over a
    // camera input, where pointer forwarding is off but typing still flows).
    if (app.consoleControl) claimFocus();
    if (!stagePointerActive) return;
    const now = performance.now();
    if (now - lastMoveAt < 16) return;
    const p = normPoint(e);
    if (!p) return;
    lastMoveAt = now;
    app.sendConsoleInput({ kind: "mouse_move", ...p, screen: controlScreen });
  }

  function onPointerButton(e: PointerEvent, down: boolean) {
    // A click is the most reliable focus pin (whatever was last focused).
    if (down && app.consoleControl) stageEl?.focus({ preventScroll: true });
    if (!stagePointerActive) return;
    if (down) {
      // Hold the pointer for the whole press: a touch drag (or a mouse drag
      // that wanders off the element) keeps streaming its moves here, and
      // the matching up always lands — capture auto-releases on pointerup.
      try {
        (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
      } catch {
        // a stale/synthetic pointer id — capture is best-effort
      }
    }
    const p = normPoint(e);
    if (!p) {
      // A release outside the streamed frame (a captured drag that wandered
      // onto the letterbox bars or past the edge before lifting): still lift
      // the button we pressed, or the remote is stranded mid-drag. Presses
      // outside the frame stay ignored, as ever.
      if (!down && heldButtons.delete(e.button)) {
        e.preventDefault();
        app.sendConsoleInput({ kind: "mouse_button", button: e.button, down: false });
      }
      return;
    }
    e.preventDefault();
    // Land the cursor exactly where the click happened, then click.
    app.sendConsoleInput({ kind: "mouse_move", ...p, screen: controlScreen });
    app.sendConsoleInput({ kind: "mouse_button", button: e.button, down });
    if (down) heldButtons.add(e.button);
    else heldButtons.delete(e.button);
  }

  // Buttons currently pressed on the remote, so a pointer that *cancels*
  // (iOS reclaiming the touch for a system gesture, the OS eating the
  // pointer mid-drag) can lift what it pressed — its matching pointerup is
  // never coming, and without this the remote is stranded mid-drag with a
  // button held.
  const heldButtons = new Set<number>();
  function onPointerCancel() {
    for (const b of heldButtons) {
      app.sendConsoleInput({ kind: "mouse_button", button: b, down: false });
    }
    heldButtons.clear();
  }

  // iPhone/iPad WebKit: touches already arrive as Pointer Events (that's
  // what drives the remote mouse above), but WebKit *also* synthesizes
  // compatibility mouse events and gesture defaults (double-tap zoom, the
  // long-press callout) off the raw touches. Cancelling touchstart's
  // default while pointer forwarding is live keeps a tap exactly one click
  // at its coordinates. Bound via an action, not `ontouchstart` — Svelte
  // registers touch listeners passive, where preventDefault is a no-op.
  // (Scroll/pan is already opted out with `touch-action: none` in CSS, so
  // a drag streams pointermoves instead of being claimed as a pan.)
  function touchGuard(el: HTMLElement) {
    const onTouchStart = (e: TouchEvent) => {
      if (stagePointerActive) e.preventDefault();
    };
    el.addEventListener("touchstart", onTouchStart, { passive: false });
    return {
      destroy() {
        el.removeEventListener("touchstart", onTouchStart);
      },
    };
  }

  function onWheel(e: WheelEvent) {
    if (!stagePointerActive || !normPoint(e)) return;
    e.preventDefault();
    // Normalize the browser's delta modes to wheel lines.
    const lines = e.deltaMode === 1 ? 1 : 1 / 40;
    app.sendConsoleInput({ kind: "wheel", dx: e.deltaX * lines, dy: e.deltaY * lines });
  }

  // Key forwarding with the bookkeeping combinations need: the physical
  // `code` rides along, and held keys are lifted in a burst whenever
  // their keyups can no longer arrive (blur, control off, session end).
  const keys = makeKeyForwarder((a) => app.sendConsoleInput(a));

  // The physical paste chord — Cmd+V (mac) / Ctrl+V (win·linux). `code` is
  // layout-independent, so this fires on the V key whatever it composes.
  function isPasteChord(e: KeyboardEvent): boolean {
    const isV = e.code === "KeyV" || (!e.code && e.key.toLowerCase() === "v");
    return isV && (e.metaKey || e.ctrlKey) && !e.altKey;
  }

  // The physical copy/cut chords — Cmd+C·X (mac) / Ctrl+C·X (win·linux). The
  // mirror of paste: with clipboard passthrough on, these copy/cut *from* the
  // remote. `code` is layout-independent, so it fires on the C/X key whatever
  // it composes.
  function isCopyCutChord(e: KeyboardEvent): boolean {
    const isCorX =
      e.code === "KeyC" ||
      e.code === "KeyX" ||
      (!e.code && (e.key.toLowerCase() === "c" || e.key.toLowerCase() === "x"));
    return isCorX && (e.metaKey || e.ctrlKey) && !e.altKey;
  }

  // Copy/cut/paste keyups already accounted for: the chord is replayed as a
  // synthesized press (a paste *after* the clipboard frame, a copy/cut *before*
  // pulling the remote clipboard back — see onKey), so the matching natural
  // keyup is a straggler to swallow — no double event, no stuck key.
  const chordHandled = new Set<string>();

  // No-control keys ride the window, so Escape works without the stage being
  // focused: it steps out of fullscreen first, then (the popover habit)
  // closes the session; in a window it closes the window too.
  function onWindowKey(e: KeyboardEvent) {
    if (!node || app.consoleControl) return;
    if (e.key === "Escape") {
      if (theater) void flipTheater();
      else endSession();
    }
  }

  // Control forwarding — bound to the focusable stage, so it fires only while
  // the stage holds keyboard focus. With control on, *every* key belongs to
  // the remote — including Escape and chords like Ctrl+W, exactly like sitting
  // at the machine.
  function onKey(e: KeyboardEvent, down: boolean) {
    if (!node || !app.consoleControl) return;
    e.preventDefault();
    // Send-on-paste: with clipboard passthrough on, a paste pushes this
    // machine's clipboard to the remote first, then replays the paste
    // keystroke — so the remote writes our clipboard before it pastes.
    // Both ride the same ordered channel to the same peer, so sending the
    // frame and only then the keystroke keeps the order the remote needs.
    if (down && app.consoleClipboard && isPasteChord(e)) {
      // Paste once per press: a held paste chord must not repeat-paste, and
      // its forwarded repeat-downs would never get a matching keyup (the
      // straggler line below swallows it), stranding the key down on the
      // remote.
      if (e.repeat) return;
      chordHandled.add(e.code || e.key);
      void app.pasteConsoleClipboard(e.key, e.code || undefined, e.metaKey);
      return;
    }
    // Copy/cut-from-remote: forward the chord so the remote copies its
    // selection into its own clipboard, then pull that clipboard back here.
    // Same once-per-press guard as paste — the forwarded keystroke is what
    // does the copying; its straggler keyup is swallowed below.
    if (down && app.consoleClipboard && isCopyCutChord(e)) {
      if (e.repeat) return;
      chordHandled.add(e.code || e.key);
      void app.copyConsoleClipboard(e.key, e.code || undefined, e.metaKey);
      return;
    }
    if (!down && chordHandled.delete(e.code || e.key)) return; // straggler keyup
    keys.onKey(e, down);
  }

  function toggleControl() {
    // Turning control off mid-chord: lift what's held while the route
    // can still carry the keyups.
    if (app.consoleControl) keys.releaseAll();
    app.toggleConsoleControl();
    // Turning it on: focus the stage so keys forward immediately, without
    // needing a click into the picture first (a `tabindex=-1` element is
    // still focusable programmatically, so this works before the reactive
    // tabindex flips to 0).
    if (app.consoleControl) stageEl?.focus({ preventScroll: true });
  }

  function inputIcon(c: Capability): string {
    return originIcon(c.origin, c.media);
  }
</script>

<svelte:window onkeydown={onWindowKey} onclick={() => (openPill = null)} />

{#if node}
  <div class="scrim" class:windowed>
    {#if !windowed}
      <button class="backdrop" aria-label="Close console" onclick={endSession}></button>
    {/if}
    <div
      class="console"
      class:theater
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
            {@const inpPopped = app.isVideoPopped(`cap:${inp.id}`)}
            <span class="tab-wrap" class:active={inp.id === selectedId}>
              <button
                class="tab"
                class:active={inp.id === selectedId}
                role="tab"
                aria-selected={inp.id === selectedId}
                title={inpPopped ? `${inp.label} — in its own window` : inp.label}
                onclick={() => app.setConsoleInput(inp.id)}
              >
                <span class="tab-icon">{inputIcon(inp)}</span>
                <span class="tab-label">{inp.label}</span>
                {#if inp.default}<span class="tab-def" title="Default input">★</span>{/if}
                {#if inpPopped}<span class="tab-out" title="In its own window">↗</span>{/if}
              </button>
              {#if isTauri() && !inpPopped}
                <button
                  class="tab-pop"
                  title="Pop this video out into its own window"
                  aria-label="Pop {inp.label} out into its own window"
                  onclick={(e) => {
                    e.stopPropagation();
                    void app.popOutConsoleInput(inp.id);
                  }}>⧉</button
                >
              {/if}
            </span>
          {/each}
          {#if inputs.length === 0}
            <span class="no-inputs">No video inputs advertised</span>
          {/if}
        </div>
        <button class="x" onclick={endSession} aria-label="Close">✕</button>
      </header>

      <!-- Video stage -->
      <!-- role=application: a remote-desktop surface — every pointer/key
           event belongs to the far machine while control is on. Focusable
           only while control is on, so keys forward from here and nowhere
           else. -->
      <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
      <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
      <div
        bind:this={stageEl}
        class="stage"
        class:grabbing={stagePointerActive}
        role="application"
        aria-label="Remote screen — input is forwarded while keyboard & mouse control is on"
        tabindex={app.consoleControl ? 0 : -1}
        use:touchGuard
        onpointermove={onPointerMove}
        onpointerdown={(e) => onPointerButton(e, true)}
        onpointerup={(e) => onPointerButton(e, false)}
        onpointercancel={onPointerCancel}
        onwheel={onWheel}
        onkeydown={(e) => onKey(e, true)}
        onkeyup={(e) => onKey(e, false)}
        onblur={() => keys.releaseAll()}
        oncontextmenu={(e) => app.consoleControl && e.preventDefault()}
      >
        {#if selectedPopped}
          <!-- This input lives in its own window right now; here's its
               way home — findable even when that window is fullscreen on
               another monitor. -->
          <div class="screen">
            <div class="screen-glyph">{selected ? inputIcon(selected) : "🪟"}</div>
            <div class="screen-title">{selected?.label ?? ""} is in its own window</div>
            <button class="return-btn" onclick={() => app.askReturnVideo(`cap:${selectedId}`)}>
              ⤓ Return video here
            </button>
          </div>
        {:else}
          {#if app.consoleVideoLive}
            <canvas
              bind:this={canvasEl}
              class="live"
              class:waiting={!hasFrame}
              aria-label="Live {selected?.media === 'video' ? 'camera' : 'screen'} view of {displayName(
                node,
              )}"
            ></canvas>
          {/if}
          {#if hasFrame}
            <!-- the canvas above is the stage; a host-reported stall (the
                 remote display sleeping mid-session) banners over it. -->
            {#if videoRefused}
              <div class="host-status">{videoRefused}</div>
            {:else if hostStatus}
              <div class="host-status">{hostStatusText(hostStatus)}</div>
            {/if}
          {:else if selected}
            <div class="screen" style="--mc: {mediaColor(selected.media)}">
              <div class="screen-glyph">{inputIcon(selected)}</div>
              <div class="screen-title">{selected.label}</div>
              {#if selected.media === "display"}
                <div class="screen-note">
                  {videoRefused ??
                    (hostStatus ? hostStatusText(hostStatus) : "Connecting this machine's display…")}
                </div>
              {:else if cameraSupported}
                <div class="screen-note">
                  {videoRefused ??
                    (hostStatus ? hostStatusText(hostStatus) : "Connecting this camera…")}
                </div>
              {:else}
                <div class="screen-note">
                  This machine runs an older AllMyStuff — update it there and its cameras will
                  stream here.
                </div>
              {/if}
            </div>
          {:else}
            <div class="screen empty">
              <div class="screen-glyph">🪟</div>
              <div class="screen-note">Pick a video input above to view this machine.</div>
            </div>
          {/if}
          <!-- The video player's corner: fullscreen where everyone looks
               for it. Hover-revealed; clicks stop here so control
               forwarding never sees them. -->
          {#if app.consoleVideoLive}
            <div class="corner">
              <button
                class="corner-btn"
                title={theater
                  ? `Exit fullscreen${app.consoleControl ? "" : " (Esc)"}`
                  : "Fullscreen"}
                aria-label={theater ? "Exit fullscreen" : "Fullscreen"}
                onpointerdown={(e) => e.stopPropagation()}
                onpointerup={(e) => e.stopPropagation()}
                onclick={(e) => {
                  e.stopPropagation();
                  void flipTheater();
                }}>{theater ? "⤡" : "⛶"}</button
              >
            </div>
          {/if}
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
          {#if access.audio}
            <button
              class="toggle"
              class:on={app.consoleAudio}
              onclick={() => app.toggleConsoleAudio()}
              title="Play that machine's audio on this machine (listen-only — nothing is sent back)"
            >
              <span class="t-icon">🔊</span>
              Audio
              <span class="pip" class:lit={app.consoleAudio}></span>
            </button>
          {/if}
          {#if access.control}
            <button
              class="toggle"
              class:on={app.consoleControl}
              onclick={toggleControl}
              title="Send this machine's keyboard & mouse to the remote"
            >
              <span class="t-icon">⌨️</span>
              Control
              <span class="pip" class:lit={app.consoleControl}></span>
            </button>
          {/if}
          {#if access.clipboard}
            <button
              class="toggle"
              class:on={app.consoleClipboard}
              onclick={() => app.toggleConsoleClipboard()}
              title="Share clipboard on paste — pasting here sends this machine's clipboard so it lands on the remote. Each machine keeps its own clipboard otherwise."
            >
              <span class="t-icon">📋</span>
              Clipboard
              <span class="pip" class:lit={app.consoleClipboard}></span>
            </button>
          {/if}
        </div>

        <!-- Quick handles to the rest of this machine — its file manager
             and a shell — so a console session doesn't send you back to
             the graph drawer for them. Owner/fleet gated exactly like the
             drawer's buttons (the far side enforces the rule again); on
             the desktop each opens (or focuses) the machine's dedicated
             window beside this one. -->
        {#if app.filesAllowed(node) || app.terminalAllowed(node)}
          <div class="quick">
            {#if app.filesAllowed(node)}
              <button
                class="launch"
                onclick={() => app.openFiles(node.id)}
                title="Browse this machine's files over the mesh"
              >
                <span class="t-icon">🗂</span>
                Files
              </button>
            {/if}
            {#if app.terminalAllowed(node)}
              <button
                class="launch"
                onclick={() => app.openTerminal(node.id)}
                title="Open a shell on this machine over the mesh"
              >
                <span class="t-icon">📟</span>
                Terminal
              </button>
            {/if}
          </div>
        {/if}

        {#if app.consoleVideoLive}
          <div class="quality">
            {#if app.consoleControlMode === "slider"}
              <!-- One Speed↔Quality knob (codec stays Auto) -->
              <div class="slider-wrap" role="group" aria-label="Stream quality">
                <span class="slider-end">Speed</span>
                <input
                  class="quality-slider"
                  type="range"
                  min="0"
                  max={QUALITY_STOPS.length - 1}
                  step="1"
                  value={sliderPos}
                  oninput={(e) => pickQuality(+e.currentTarget.value)}
                  aria-label="Quality"
                  title="Drag toward Speed (lighter, faster) or Quality (sharper, heavier)"
                />
                <span class="slider-end">Quality</span>
                <span class="slider-now">{QUALITY_STOPS[sliderPos].label}</span>
              </div>
            {:else}
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
                {#if stagePointerActive}
                  <span class="pill-wrap">
                    <button
                      class="pill"
                      class:tuned={aspectChoice !== "auto"}
                      title="Source aspect — corrects the mouse when a machine whose native resolution isn't 16:9 is letterboxed into the capture. Auto detects the bars from the picture."
                      onclick={(e) => {
                        e.stopPropagation();
                        openPill = openPill === "aspect" ? null : "aspect";
                      }}
                    >
                      Aspect · {aspectLabel} ▾
                    </button>
                    {#if openPill === "aspect"}
                      <div class="pill-menu" role="menu">
                        {#each ASPECTS as a (a.value)}
                          <button
                            class="pill-item"
                            class:sel={aspectChoice === a.value}
                            onclick={(e) => {
                              e.stopPropagation();
                              pickAspect(a.value);
                            }}
                          >
                            {a.label}
                          </button>
                        {/each}
                      </div>
                    {/if}
                  </span>
                {/if}
              </div>
            {/if}
            <button
              class="more"
              title="Switch between the quality slider and the detailed controls"
              aria-label="Toggle quality controls"
              onclick={() => app.toggleConsoleControlMode()}>…</button
            >
          </div>
        {/if}

        <div class="status">
          {#if hasFrame}
            <span class="chip stream" title="Live stream — frame size · rate">
              <span class="chip-dot live-dot"></span>{frameW}×{frameH} · {fps} fps · {transport}{pipeDiag
                ? ` · ⚠ ${pipeDiag}`
                : ""}
            </span>
          {/if}
          {#each app.consoleSessionRoutes as r (r.id)}
            <span class="chip" style="--mc: {mediaColor(r.media as MediaKind)}">
              <span class="chip-dot"></span>{MEDIA[r.media as MediaKind].label}
            </span>
          {/each}
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
  /* Phone-width: the in-page console takes the whole screen — closing it is
     the way back to the graph. The scrim already sits above the header and
     the portrait pill dock (z 60 > 30); safe-area padding keeps the title
     bar (and its ✕) clear of the status bar, and the control bar clear of
     the home indicator. Zero effect on desktop: env() is 0 there, and a
     narrow *windowed* console is already 100%×100%. */
  @media (max-width: 700px) {
    .scrim {
      padding: 0;
    }
    .console {
      width: 100%;
      height: 100%;
      border: none;
      border-radius: 0;
    }
    .bar {
      padding-top: calc(0.5rem + max(3.4rem, env(safe-area-inset-top, 0px)));
    }
    .controls {
      padding-bottom: calc(0.6rem + env(safe-area-inset-bottom, 0px));
    }
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
    box-shadow: 0 0 0 3px oklch(0.8 0.17 150 / 0.25);
  }
  .inputs {
    display: flex;
    gap: 0.3rem;
    flex: 1;
    min-width: 0;
    overflow-x: auto;
    /* Bottom room inside the scroller for the tabs' pop-out button,
       which hangs off the bottom corner (the scroll clip would eat an
       overhang); the negative margin gives the space back so the bar
       doesn't grow. */
    padding: 0.1rem 0.1rem 0.55rem;
    margin-bottom: -0.45rem;
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
    color: var(--warn);
    font-size: 0.7rem;
  }
  .tab-out {
    color: var(--accent-2, #9be3ff);
    font-size: 0.7rem;
  }
  .tab.active .tab-out {
    color: #fff;
  }
  /* The hover popout: lives on a wrapper (a button can't nest one),
     revealed when the tab is hovered or it has keyboard focus. */
  .tab-wrap {
    position: relative;
    display: inline-flex;
    flex-shrink: 0;
  }
  .tab-pop {
    position: absolute;
    bottom: -0.45rem;
    right: -0.3rem;
    width: 1.25rem;
    height: 1.25rem;
    border-radius: 50%;
    border: 1px solid #443d63;
    background: #241f38;
    color: #c8c2e0;
    font-size: 0.66rem;
    line-height: 1;
    cursor: pointer;
    opacity: 0;
    transition: opacity 120ms ease;
  }
  .tab-wrap:hover .tab-pop,
  .tab-pop:focus-visible {
    opacity: 1;
  }
  .tab-pop:hover {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
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
    /* Touch drives the remote pointer: opt out of the browser's own
       gestures (scroll/pan, double-tap zoom) so a finger drag streams
       pointermoves instead of being claimed — and pointercancelled — as a
       pan. The stage never scrolls, and mouse input is unaffected. */
    touch-action: none;
    /* And no long-press text-selection callout / magnifier over the
       picture — a held finger is a held mouse button, nothing more. */
    -webkit-user-select: none;
    user-select: none;
    -webkit-touch-callout: none;
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
      radial-gradient(1200px 400px at 50% -10%, oklch(0.62 0.2 292 / 0.12), transparent),
      repeating-linear-gradient(0deg, #100e1a, #100e1a 2px, #12101c 2px, #12101c 4px);
  }
  .stage.grabbing {
    cursor: crosshair;
  }
  /* The stage is focusable (so keys forward) but fills its pane — a focus
     ring around it would just be noise. */
  .stage:focus,
  .stage:focus-visible {
    outline: none;
  }
  /* Fullscreen ("theater"): chrome away, the stage is the window. */
  .console.theater > .bar,
  .console.theater > .controls {
    display: none;
  }
  .console.theater {
    border: none;
    border-radius: 0;
  }
  .console.theater > .stage {
    padding: 0;
    background: #000;
  }
  .console.theater .live {
    border-radius: 0;
    box-shadow: none;
  }
  /* The hover corner — the video player's bottom-right. */
  .corner {
    position: absolute;
    right: 1.4rem;
    bottom: 1.4rem;
    display: inline-flex;
    gap: 0.3rem;
    opacity: 0;
    transition: opacity 120ms ease;
  }
  .stage:hover .corner,
  .corner:focus-within {
    opacity: 1;
  }
  .corner-btn {
    border: 1px solid rgba(255, 255, 255, 0.22);
    background: rgba(0, 0, 0, 0.55);
    backdrop-filter: blur(4px);
    color: #fff;
    border-radius: var(--r-sm);
    width: 2.1rem;
    height: 2.1rem;
    font-size: 1.05rem;
    line-height: 1;
    cursor: pointer;
  }
  .corner-btn:hover {
    background: rgba(0, 0, 0, 0.8);
  }
  /* The way home for a popped-out input. */
  .return-btn {
    margin-top: 0.4rem;
    border: 1px solid var(--line-strong);
    background: var(--accent);
    color: #fff;
    border-radius: var(--r-md);
    padding: 0.75rem 1.3rem;
    font-size: 1rem;
    font-weight: 700;
    cursor: pointer;
  }
  .return-btn:hover {
    filter: brightness(1.12);
  }
  .live {
    /* Size the element to the video's OWN box (its intrinsic backing store
       scaled down to fit the stage), centered by the stage's place-items — the
       standard responsive-replaced-element pattern (display:block + auto dims +
       max caps). NOT width/height:100% + object-fit, which makes the element the
       full cell with the video letterboxed INSIDE it: normPoint then has to
       recompute that inset and it drifts by ~a bar width (the "offset = letterbox
       size" skew). With the element == the content box, normPoint's inset is 0
       and it normalizes over the element directly — like the KVM's accurate web
       UI. */
    display: block;
    width: auto;
    height: auto;
    max-width: 100%;
    max-height: 100%;
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
    background: radial-gradient(900px 360px at 50% 30%, oklch(0.62 0.2 292 / 0.1), #0c0b14);
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
    filter: drop-shadow(0 4px 12px var(--mc, oklch(0.62 0.2 292 / 0.4)));
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
  /* The bar wraps instead of overflowing: on a narrow console the groups
     fall onto extra rows rather than squashing the stage or clipping their
     controls. */
  .controls {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.5rem 0.7rem;
    padding: 0.6rem 0.7rem;
    background: #1a1730;
    border-top: 1px solid #2c2740;
    flex-shrink: 0;
  }
  .toggles,
  .quick {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
  }
  .toggle,
  .launch {
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
  .toggle:hover,
  .launch:hover {
    border-color: var(--accent);
  }
  .toggle.on {
    background: oklch(0.8 0.17 150 / 0.15);
    border-color: var(--ok);
    color: oklch(0.88 0.09 150);
  }
  .t-icon {
    font-size: 0.95rem;
  }
  .quality {
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }
  .pills {
    display: flex;
    flex-wrap: wrap;
    gap: 0.3rem;
  }
  .slider-wrap {
    display: flex;
    align-items: center;
    gap: 0.45rem;
  }
  .slider-end {
    font-size: 0.72rem;
    color: #8a83a6;
  }
  .quality-slider {
    width: 8.5rem;
    accent-color: #7c6cff;
    cursor: pointer;
  }
  .slider-now {
    min-width: 4.2rem;
    font-size: 0.74rem;
    color: #c8c2e0;
  }
  .more {
    border: 1px solid #322c47;
    background: #14121f;
    color: #c8c2e0;
    border-radius: var(--r-pill);
    padding: 0 0.5rem;
    line-height: 1.55rem;
    cursor: pointer;
    font-weight: 700;
  }
  .more:hover {
    border-color: #4a4170;
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
    box-shadow: 0 0 0 3px oklch(0.8 0.17 150 / 0.25);
  }
  .status {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    flex: 1 1 10rem;
    min-width: 0;
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
    color: oklch(0.88 0.09 150);
    border-color: oklch(0.8 0.17 150 / 0.5);
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
  .end {
    flex-shrink: 0;
    margin-left: auto;
    background: oklch(0.2 0.05 14);
    border: 1px solid oklch(0.38 0.11 14);
    color: oklch(0.82 0.1 14);
  }
  .end:hover {
    background: oklch(0.25 0.07 14);
  }
</style>
