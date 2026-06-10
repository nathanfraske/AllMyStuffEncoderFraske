<script lang="ts">
  // The tabbed terminal for one remote machine — a real shell on the far
  // side (a PTY the host spawns), xterm.js on this side, and the mesh in
  // between. Two skins, one component: the desktop renders it `windowed`
  // (filling a dedicated OS window per machine); the web preview shows the
  // same thing as an in-page popover, where it's honest that live shells
  // need the desktop app.
  //
  // Each tab is its own *session*: its own route (minted by
  // `terminalConnect`, unique per tab) and its own PTY on the far side.
  // A tab's lifecycle is driven by the route's live state from session
  // snapshots — connecting → live (bytes flow) → ended/rejected — plus
  // the host's exit report (`allmystuff://term-exit`) for the code.
  import { onMount } from "svelte";
  import { Terminal as XTerm } from "@xterm/xterm";
  import { FitAddon } from "@xterm/addon-fit";
  import { Unicode11Addon } from "@xterm/addon-unicode11";
  import { WebglAddon } from "@xterm/addon-webgl";
  import { WebLinksAddon } from "@xterm/addon-web-links";
  import "@xterm/xterm/css/xterm.css";
  import { app } from "../store.svelte";
  import {
    clipboardRead,
    clipboardWrite,
    closeThisWindow,
    onTermExit,
    onThisWindowClose,
    openExternal,
    termSend,
    watchTerminal,
  } from "../tauri";
  import { displayName } from "../types";

  let { host, windowed = false }: { host: string; windowed?: boolean } = $props();

  const node = $derived(app.node(host));

  type TabStatus = "connecting" | "live" | "rejected" | "ended" | "offline";
  interface TabMeta {
    id: number;
    routeId: string | null; // null = web mode, nothing can flow
    title: string;
    status: TabStatus;
    /** The line the overlay shows for rejected/ended tabs. */
    note: string;
  }

  /** The non-reactive half of a tab: the emulator and everything that
   *  must be torn down with it. Kept out of `$state` — class instances
   *  aren't proxied, and disposal order matters more than reactivity. */
  interface TabRuntime {
    term: XTerm;
    fit: FitAddon;
    started: boolean; // the route went live and the watcher is wired
    stopWatch: (() => void) | null;
    cleanup: Array<() => void>;
  }

  let tabs = $state<TabMeta[]>([]);
  let activeId = $state(0);
  let bell = $state(false);
  let nextTabId = 1;
  const runtimes = new Map<number, TabRuntime>();
  // Bumped when an emulator mounts (or disposes). The `runtimes` Map isn't
  // reactive, so the status effect reads this to re-run the instant a
  // tab's emulator is ready — otherwise a route that went live before its
  // pane mounted would wait for the next snapshot poll to get wired.
  let runtimesReady = $state(0);
  let unlistenExit: (() => void) | null = null;
  let unlistenClose: (() => void) | null = null;
  let bellTimer: ReturnType<typeof setTimeout> | null = null;

  // ---- byte plumbing ---------------------------------------------------

  /** Bytes → base64 without blowing the stack on a huge paste. */
  function b64encode(bytes: Uint8Array): string {
    let bin = "";
    const STEP = 0x8000;
    for (let i = 0; i < bytes.length; i += STEP) {
      bin += String.fromCharCode(...bytes.subarray(i, i + STEP));
    }
    return btoa(bin);
  }

  function sendBytes(routeId: string, bytes: Uint8Array) {
    void termSend(routeId, { kind: "data", bytes: b64encode(bytes) }).catch(() => {
      // A send into a just-torn route; the status overlay tells the story.
    });
  }

  // ---- tab lifecycle ---------------------------------------------------

  function newTab() {
    const id = nextTabId++;
    const routeId = app.terminalConnect(host);
    console.debug(`[terminal] tab ${id} opened — route ${routeId ?? "(none: web mode)"}`);
    tabs.push({
      id,
      routeId,
      title: `Shell ${id}`,
      status: routeId ? "connecting" : "offline",
      note: routeId ? "" : "Live terminals need the desktop app.",
    });
    activeId = id;
  }

  /** Svelte action: a tab's pane is in the DOM — build its emulator. A
   *  failure here must *show itself* (the tab would otherwise sit on
   *  "connecting" forever with a perfectly live route behind it). */
  function mountTerm(el: HTMLElement, tabId: number) {
    const meta = tabs.find((t) => t.id === tabId);
    if (!meta) return;
    try {
      return mountTermInner(el, tabId, meta);
    } catch (e) {
      console.error("[terminal] emulator failed to start:", e);
      meta.status = "rejected";
      meta.note = `the terminal emulator failed to start here: ${e}`;
      return;
    }
  }

  function mountTermInner(el: HTMLElement, tabId: number, meta: TabMeta) {
    const term = new XTerm({
      allowProposedApi: true,
      cursorBlink: true,
      scrollback: 10_000,
      fontSize: 13,
      fontFamily:
        "ui-monospace, SFMono-Regular, Menlo, Consolas, 'Liberation Mono', monospace",
      macOptionIsMeta: true,
      theme: {
        background: "#14121f",
        foreground: "#d7d2ec",
        cursor: "#a899f6",
        selectionBackground: "#3d3760",
      },
      // The remote's ConPTY reflows on resize; telling xterm gets the
      // heuristics (cursor handling, wrapped-line tracking) right.
      windowsPty:
        node?.summary?.os === "windows" ? { backend: "conpty" } : undefined,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.loadAddon(new Unicode11Addon());
    term.unicode.activeVersion = "11";
    term.loadAddon(
      new WebLinksAddon((e, uri) => {
        e.preventDefault();
        void openExternal(uri);
      }),
    );

    const rt: TabRuntime = { term, fit, started: false, stopWatch: null, cleanup: [] };
    runtimes.set(tabId, rt);
    runtimesReady++; // nudge the status effect — this tab can wire now

    term.open(el);
    // GPU rendering where the webview offers WebGL2; the DOM renderer is
    // the silent fallback (WebKitGTK without GL, lost contexts, …). The
    // addon needs an opened terminal — load it strictly after `open`.
    try {
      const gl = new WebglAddon();
      gl.onContextLoss(() => gl.dispose());
      term.loadAddon(gl);
    } catch (e) {
      console.info("[terminal] WebGL renderer unavailable, using DOM:", e);
    }
    fit.fit();
    term.focus();

    // Keystrokes → the far PTY. `onData` is everything typed (incl. escape
    // sequences); `onBinary` is the rare raw-byte path (some mouse modes).
    rt.cleanup.push(
      term.onData((s) => {
        const t = tabs.find((x) => x.id === tabId);
        if (t?.routeId && t.status === "live") sendBytes(t.routeId, new TextEncoder().encode(s));
      }).dispose,
    );
    rt.cleanup.push(
      term.onBinary((s) => {
        const t = tabs.find((x) => x.id === tabId);
        if (!t?.routeId || t.status !== "live") return;
        const bytes = new Uint8Array(s.length);
        for (let i = 0; i < s.length; i++) bytes[i] = s.charCodeAt(i) & 0xff;
        sendBytes(t.routeId, bytes);
      }).dispose,
    );

    // Emulator resized (fit ran) → the far PTY follows, debounced so a
    // window drag doesn't machine-gun resizes down the channel.
    let resizeTimer: ReturnType<typeof setTimeout> | null = null;
    rt.cleanup.push(
      term.onResize(({ cols, rows }) => {
        if (resizeTimer) clearTimeout(resizeTimer);
        resizeTimer = setTimeout(() => {
          const t = tabs.find((x) => x.id === tabId);
          if (t?.routeId && t.status === "live")
            void termSend(t.routeId, { kind: "resize", cols, rows }).catch(() => {});
        }, 50);
      }).dispose,
    );
    rt.cleanup.push(() => {
      if (resizeTimer) clearTimeout(resizeTimer);
    });

    // The shell names the tab (OSC 0/2 — what most prompts set).
    rt.cleanup.push(
      term.onTitleChange((title) => {
        const t = tabs.find((x) => x.id === tabId);
        if (t && title.trim()) t.title = title.trim();
      }).dispose,
    );

    rt.cleanup.push(
      term.onBell(() => {
        bell = true;
        if (bellTimer) clearTimeout(bellTimer);
        bellTimer = setTimeout(() => (bell = false), 150);
      }).dispose,
    );

    // Terminal-convention clipboard chords (Ctrl+C must stay SIGINT):
    // Ctrl+Shift+C copies the selection, Ctrl+Shift+V pastes, and
    // Ctrl+Shift+T opens a sibling shell. macOS Cmd+C/V already work —
    // xterm answers the webview's native copy/paste events.
    term.attachCustomKeyEventHandler((e) => {
      if (e.type !== "keydown" || !e.ctrlKey || !e.shiftKey) return true;
      const k = e.key.toLowerCase();
      if (k === "c" && term.hasSelection()) {
        void clipboardWrite(term.getSelection());
        term.clearSelection();
        return false;
      }
      if (k === "v") {
        void clipboardRead().then((text) => text && term.paste(text));
        return false;
      }
      if (k === "t") {
        newTab();
        return false;
      }
      return true;
    });

    // Keep the emulator fitted to its pane.
    const ro = new ResizeObserver(() => {
      if (el.clientWidth > 0 && el.clientHeight > 0) fit.fit();
    });
    ro.observe(el);
    rt.cleanup.push(() => ro.disconnect());

    if (meta.status === "offline") {
      term.write("\x1b[2m[demo mode — live terminals need the desktop app]\x1b[0m\r\n");
    }

    return {
      destroy() {
        disposeRuntime(tabId);
      },
    };
  }

  function disposeRuntime(tabId: number) {
    const rt = runtimes.get(tabId);
    if (!rt) return;
    runtimes.delete(tabId);
    runtimesReady++;
    rt.stopWatch?.();
    for (const fn of rt.cleanup) fn();
    rt.term.dispose();
  }

  /** A tab's route went live: wire the byte stream and size the far PTY
   *  to the emulator. Buffered output (the prompt that raced the window
   *  boot) arrives on the first poll. */
  function startSession(meta: TabMeta, rt: TabRuntime) {
    if (rt.started || !meta.routeId) return;
    rt.started = true;
    const routeId = meta.routeId;
    console.debug(`[terminal] wiring output for ${routeId}`);
    void watchTerminal(routeId, (bytes) => rt.term.write(bytes)).then((stop) => {
      // The tab may have died while the watch was being wired.
      if (runtimes.get(meta.id) !== rt) {
        stop();
        return;
      }
      rt.stopWatch = stop;
    });
    void termSend(routeId, { kind: "resize", cols: rt.term.cols, rows: rt.term.rows }).catch(
      () => {},
    );
    rt.term.focus();
  }

  // Drive each tab's status off its route's live state from the session
  // snapshots — the same source of truth the rest of the app reads. The
  // status flips on the route alone; wiring the byte stream additionally
  // needs the emulator runtime, and is retried each pass so a slow mount
  // can never strand a live route on "connecting".
  //
  // Both reactive sources are read *unconditionally up front*: an effect's
  // dependencies are whatever it read on its last run, so reading them
  // only inside the `tabs` loop meant an empty first run (before onMount's
  // first tab) subscribed to neither — and then only a `tabs` change, like
  // opening a *second* tab, would ever re-run it. That's why the first tab
  // sat on "connecting" until a sibling was added.
  $effect(() => {
    const states = app.routeStates;
    void runtimesReady;
    for (const t of tabs) {
      if (!t.routeId) continue;
      const st = states[t.routeId];
      const rt = runtimes.get(t.id);
      if (st?.state === "active") {
        if (t.status === "connecting") {
          console.debug(`[terminal] route live: ${t.routeId}`);
          t.status = "live";
        }
        if (t.status === "live" && rt && !rt.started) startSession(t, rt);
      } else if (st?.state === "rejected") {
        if (t.status !== "rejected") {
          console.warn(`[terminal] route rejected: ${t.routeId} (${st.reason ?? "no reason"})`);
          t.status = "rejected";
          t.note = st.reason || "the far side refused the session";
          rt?.term.write(`\r\n\x1b[31m[offer rejected: ${t.note}]\x1b[0m\r\n`);
        }
      } else if (st?.state === "torn_down" && (t.status === "live" || t.status === "connecting")) {
        // The route closed — under a live shell (host app quit, far side
        // ended it) or before it ever delivered one (the host's shell
        // failed to spawn). An exit report usually lands first and is the
        // better story; this is the fallback either way.
        console.debug(`[terminal] route torn down: ${t.routeId} (was ${t.status})`);
        const early = t.status === "connecting";
        t.status = "ended";
        if (!t.note)
          t.note = early
            ? "the far side closed the session before it started"
            : "session ended by the far side";
      }
    }
  });

  /** Give a finished tab a fresh shell (new route, same scrollback). */
  function restartTab(tabId: number) {
    const t = tabs.find((x) => x.id === tabId);
    const rt = runtimes.get(tabId);
    if (!t || !rt) return;
    rt.stopWatch?.();
    rt.stopWatch = null;
    rt.started = false;
    t.routeId = app.terminalConnect(host);
    t.status = t.routeId ? "connecting" : "offline";
    t.note = t.routeId ? "" : "Live terminals need the desktop app.";
    rt.term.write("\r\n\x1b[2m── new session ──\x1b[0m\r\n");
    rt.term.focus();
  }

  async function closeTab(tabId: number) {
    const t = tabs.find((x) => x.id === tabId);
    if (!t) return;
    const teardown = t.routeId ? app.terminalDisconnect(t.routeId) : Promise.resolve();
    tabs = tabs.filter((x) => x.id !== tabId); // unmounts the pane → disposeRuntime
    if (activeId === tabId && tabs.length) activeId = tabs[tabs.length - 1].id;
    if (tabs.length === 0) {
      await endAll(teardown);
      return;
    }
    void teardown;
    focusActiveSoon();
  }

  function selectTab(tabId: number) {
    activeId = tabId;
    focusActiveSoon();
  }

  /** After a tab becomes visible its pane has real dimensions again —
   *  refit and focus on the next frame. */
  function focusActiveSoon() {
    requestAnimationFrame(() => {
      const rt = runtimes.get(activeId);
      if (rt) {
        rt.fit.fit();
        rt.term.focus();
      }
    });
  }

  let closing = false;
  /** Tear every session down and close the surface. Bounded — a wedged
   *  backend must never hold a closing window hostage. */
  async function endAll(extra?: Promise<unknown>) {
    if (closing) return;
    closing = true;
    const teardowns = tabs
      .filter((t) => t.routeId)
      .map((t) => app.terminalDisconnect(t.routeId!));
    if (extra) teardowns.push(extra as Promise<never>);
    tabs = [];
    if (windowed) {
      await Promise.race([Promise.allSettled(teardowns), new Promise((r) => setTimeout(r, 600))]);
      void closeThisWindow();
    } else {
      app.closeTerminal();
    }
    closing = false;
  }

  function onPaneContextMenu(e: MouseEvent) {
    // Terminal convention: right-click copies the selection if there is
    // one, otherwise pastes — never the browser menu over a shell.
    e.preventDefault();
    const rt = runtimes.get(activeId);
    if (!rt) return;
    if (rt.term.hasSelection()) {
      void clipboardWrite(rt.term.getSelection());
      rt.term.clearSelection();
    } else {
      void clipboardRead().then((text) => text && rt.term.paste(text));
    }
  }

  onMount(() => {
    newTab();
    // Tab statuses hang off route negotiation states from session
    // snapshots. Snapshot *events* are the latency win but can be lost
    // (this codebase's video plane moved to pull for exactly that
    // reason) — so pull the snapshot on a short interval as the truth.
    const sessionPoll = setInterval(() => void app.refreshSession(), 1000);
    void onTermExit((ev) => {
      const t = tabs.find((x) => x.routeId === ev.route);
      if (!t) return;
      const rt = runtimes.get(t.id);
      const what = ev.code === null ? "ended" : `exited with code ${ev.code}`;
      t.status = "ended";
      t.note = `process ${what}`;
      rt?.term.write(`\r\n\x1b[2m[${t.note}]\x1b[0m\r\n`);
    }).then((u) => (unlistenExit = u));
    if (windowed) {
      // The OS chrome's ✕ must tear the sessions down too — the close is
      // held until the teardowns are on the wire (see onThisWindowClose).
      void onThisWindowClose(() => void endAll()).then((u) => (unlistenClose = u));
    }
    return () => {
      clearInterval(sessionPoll);
      unlistenExit?.();
      unlistenClose?.();
      if (bellTimer) clearTimeout(bellTimer);
    };
  });
</script>

{#if node}
  <div class="scrim" class:windowed>
    {#if !windowed}
      <button class="backdrop" aria-label="Close terminal" onclick={() => void endAll()}></button>
    {/if}
    <div
      class="terminal"
      class:bell
      role="dialog"
      aria-modal={!windowed}
      aria-label="Terminal on {displayName(node)}"
    >
      <header class="head">
        <div class="who">
          <span class="ico">📟</span>
          <div class="meta">
            <div class="name">{displayName(node)}</div>
            <div class="sub">
              <span class="dot" class:on={node.online}></span>
              {node.online ? "online" : "offline"} · terminal
            </div>
          </div>
        </div>
        <div class="tabs" role="tablist" aria-label="Shells">
          {#each tabs as t (t.id)}
            <div class="tab" class:active={t.id === activeId}>
              <button
                class="tab-pick"
                role="tab"
                aria-selected={t.id === activeId}
                title={t.title}
                onclick={() => selectTab(t.id)}
              >
                <span class="tab-state {t.status}"></span>
                <span class="tab-label">{t.title}</span>
              </button>
              <button class="tab-x" title="Close this shell" onclick={() => void closeTab(t.id)}>
                ✕
              </button>
            </div>
          {/each}
          <button class="tab-new" title="New shell (Ctrl+Shift+T)" onclick={() => newTab()}>＋</button>
        </div>
        <button class="x" onclick={() => void endAll()} aria-label="Close">✕</button>
      </header>

      <!-- role=application: a shell surface — keys belong to the far
           machine; right-click is copy/paste, never the browser menu. -->
      <div
        class="stage"
        role="application"
        aria-label="Shell on {displayName(node)}"
        oncontextmenu={onPaneContextMenu}
      >
        {#each tabs as t (t.id)}
          <div class="pane" class:hidden={t.id !== activeId}>
            <div class="xterm-host" use:mountTerm={t.id}></div>
            {#if t.status === "connecting"}
              <div class="veil">
                <p>Connecting to <b>{displayName(node)}</b>…</p>
                <!-- The raw negotiation state, so a stall names its stage:
                     "offered" = the far side never answered; "not
                     negotiated yet" with other routes known = this route
                     id is missing from the snapshot (a key bug); with 0
                     known = snapshots aren't reaching this window. -->
                <p class="diag">
                  route {app.routeStates[t.routeId ?? ""]?.state ?? "not negotiated yet"}
                  · {Object.keys(app.routeStates).length} known
                </p>
              </div>
            {:else if t.status === "offline"}
              <div class="veil">
                <p>{t.note}</p>
              </div>
            {:else if t.status === "rejected" || t.status === "ended"}
              <div class="veil ended">
                <p>{t.status === "rejected" ? "Refused: " : ""}{t.note}</p>
                <div class="veil-actions">
                  {#if t.status === "ended"}
                    <button class="btn-restart" onclick={() => restartTab(t.id)}>↻ New session</button>
                  {/if}
                  <button class="btn-close" onclick={() => void closeTab(t.id)}>Close tab</button>
                </div>
              </div>
            {/if}
          </div>
        {/each}
      </div>
    </div>
  </div>
{/if}

<style>
  .scrim {
    position: fixed;
    inset: 0;
    z-index: 40;
    display: grid;
    place-items: center;
  }
  .scrim.windowed {
    position: absolute;
    background: #14121f;
  }
  .backdrop {
    position: absolute;
    inset: 0;
    border: none;
    background: rgba(20, 18, 31, 0.55);
    cursor: default;
  }
  .terminal {
    position: relative;
    display: flex;
    flex-direction: column;
    width: min(960px, 94vw);
    height: min(620px, 88vh);
    background: #14121f;
    border-radius: 12px;
    box-shadow: 0 18px 60px rgba(10, 8, 20, 0.55);
    overflow: hidden;
    border: 1px solid #2c2745;
    transition: border-color 0.12s ease;
  }
  .terminal.bell {
    border-color: #a899f6;
  }
  .windowed .terminal {
    width: 100%;
    height: 100%;
    border-radius: 0;
    border: none;
    box-shadow: none;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0.45rem 0.6rem;
    background: #1c1930;
    border-bottom: 1px solid #2c2745;
    flex-shrink: 0;
    min-width: 0;
  }
  .who {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    min-width: 0;
  }
  .ico {
    font-size: 1.25rem;
  }
  .meta .name {
    color: #e8e4f8;
    font-size: 0.86rem;
    font-weight: 650;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 13rem;
  }
  .meta .sub {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    color: #8f88ad;
    font-size: 0.68rem;
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: #5c5680;
  }
  .dot.on {
    background: #41c98d;
  }
  .tabs {
    display: flex;
    align-items: center;
    gap: 0.25rem;
    flex: 1;
    min-width: 0;
    overflow-x: auto;
    scrollbar-width: thin;
  }
  .tab {
    display: flex;
    align-items: center;
    background: #242040;
    border: 1px solid transparent;
    border-radius: 7px;
    flex-shrink: 0;
  }
  .tab.active {
    background: #322c55;
    border-color: #494173;
  }
  .tab-pick {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    border: none;
    background: none;
    color: #b9b2d6;
    font-size: 0.74rem;
    padding: 0.28rem 0.15rem 0.28rem 0.55rem;
    cursor: pointer;
    max-width: 11rem;
  }
  .tab.active .tab-pick {
    color: #efecfb;
  }
  .tab-label {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .tab-state {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    flex-shrink: 0;
    background: #5c5680;
  }
  .tab-state.live {
    background: #41c98d;
  }
  .tab-state.connecting {
    background: #d8a544;
  }
  .tab-state.rejected,
  .tab-state.ended {
    background: #d4587a;
  }
  .tab-x {
    border: none;
    background: none;
    color: #7d76a0;
    font-size: 0.62rem;
    width: 1.3rem;
    height: 1.3rem;
    border-radius: 50%;
    cursor: pointer;
    margin-right: 0.15rem;
  }
  .tab-x:hover {
    background: #463e6e;
    color: #f1eefb;
  }
  .tab-new {
    border: 1px dashed #494173;
    background: none;
    color: #9a93b8;
    width: 1.65rem;
    height: 1.65rem;
    border-radius: 7px;
    font-size: 0.8rem;
    cursor: pointer;
    flex-shrink: 0;
  }
  .tab-new:hover {
    color: #efecfb;
    border-color: #6f64ab;
  }
  .x {
    border: none;
    background: #242040;
    color: #b9b2d6;
    width: 1.8rem;
    height: 1.8rem;
    border-radius: 50%;
    font-size: 0.75rem;
    cursor: pointer;
    flex-shrink: 0;
  }
  .x:hover {
    background: #463e6e;
    color: #fff;
  }
  .stage {
    position: relative;
    flex: 1;
    min-height: 0;
  }
  .pane {
    position: absolute;
    inset: 0;
    display: flex;
  }
  .pane.hidden {
    display: none;
  }
  .xterm-host {
    flex: 1;
    min-width: 0;
    min-height: 0;
    padding: 0.4rem 0 0 0.5rem;
  }
  .veil {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 0.7rem;
    background: rgba(20, 18, 31, 0.72);
    color: #b9b2d6;
    font-size: 0.86rem;
    text-align: center;
    padding: 1rem;
  }
  .veil.ended {
    background: rgba(20, 18, 31, 0.6);
  }
  .veil b {
    color: #e8e4f8;
  }
  .veil .diag {
    font-size: 0.68rem;
    color: #7d76a0;
  }
  .veil-actions {
    display: flex;
    gap: 0.5rem;
  }
  .btn-restart,
  .btn-close {
    border: 1px solid #494173;
    background: #242040;
    color: #d7d2ec;
    border-radius: 7px;
    padding: 0.34rem 0.7rem;
    font-size: 0.78rem;
    cursor: pointer;
  }
  .btn-restart:hover,
  .btn-close:hover {
    background: #322c55;
  }
</style>
