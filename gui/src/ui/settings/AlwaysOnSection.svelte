<script lang="ts">
  // Always On — keep this machine on the mesh without the app open (an OS
  // background service: systemd / launchd / the Windows SCM), and keep the app
  // itself a click away in the tray (close / minimize to the notification area
  // or menu bar). The service half delegates to `allmystuff service …`; the
  // window half persists a backend-owned preference the native close/minimize
  // handler reads.
  import { onMount } from "svelte";
  import { app } from "../../store.svelte";
  import { isTauri } from "../../tauri";

  const web = !isTauri();
  const svc = $derived(app.serviceInfo);
  const wb = $derived(app.windowBehavior);
  const busy = $derived(app.serviceBusy);

  const loaded = $derived(svc != null);
  // All three desktop OSes have a service layer, so `supported` reflects the
  // platform, not whether we could reach the CLI. A missing/erroring CLI sets
  // cli_missing / status_error instead — those mean "can't manage it right
  // now", never "this platform has no service".
  const supported = $derived(svc?.supported === true);
  const cliMissing = $derived(!!svc?.cli_missing);
  const statusError = $derived(svc?.status_error ?? null);
  const unsupported = $derived(loaded && svc?.supported !== true);
  const reachable = $derived(supported && !cliMissing && !statusError);
  const installed = $derived(svc?.installed === true);
  const running = $derived(svc?.running === true);
  const enabled = $derived(svc?.enabled === true);

  // Platform-aware wording so the copy reads right on each OS — derived from
  // the CLI's reported manager when we have it, else from the platform.
  const isMac = $derived(svc?.platform === "macos");
  const trayWord = $derived(isMac ? "menu bar" : "system tray");
  const startsWhen = $derived(svc?.scope === "system" ? "boot" : "login");
  const serviceKind = $derived(
    svc?.manager === "windows-service" || svc?.platform === "windows"
      ? "Windows"
      : svc?.manager === "launchd" || svc?.platform === "macos"
        ? "launchd"
        : svc?.manager === "systemd" || svc?.platform === "linux"
          ? "systemd"
          : "background",
  );
  const statusWord = $derived(
    !reachable ? "—" : !installed ? "Off" : running ? "Running" : "Stopped",
  );

  let armedUninstall = $state(false);
  function uninstall() {
    if (armedUninstall) {
      armedUninstall = false;
      void app.uninstallService();
    } else {
      armedUninstall = true;
      setTimeout(() => (armedUninstall = false), 3500);
    }
  }

  function platformLabel(p?: string): string {
    return p === "windows" ? "Windows" : p === "macos" ? "macOS" : p === "linux" ? "Linux" : "this system";
  }

  onMount(() => {
    void app.loadServiceStatus();
    void app.loadWindowBehavior();
  });
</script>

<div class="section">
  <h3>Always On</h3>
  <p class="lead">
    Keep this machine on the mesh without the app open — and keep AllMyStuff itself a click away
    in the {trayWord}.
  </p>

  {#if web}
    <section class="block">
      <p class="notice">
        These controls live in the desktop app — this is the in-browser preview.
      </p>
    </section>
  {:else}
    <!-- Background service -->
    <section class="block">
      <div class="row">
        <div class="grow">
          <div class="title">Run in the background</div>
          <div class="hint">
            Install AllMyStuff as a {serviceKind} service so it serves this machine — its screen,
            files, terminal and more — across logout and reboot, no window needed. The service keeps
            itself (and the mesh daemon) up to date on its own.
          </div>
        </div>
        <span class="pill" class:on={running} class:idle={installed && !running}>{statusWord}</span>
      </div>

      {#if !loaded}
        <p class="notice">Reading service status…</p>
      {:else if unsupported}
        <p class="notice">
          A background service isn't available on {platformLabel(svc?.platform)} — you can still run
          <code>allmystuff serve</code> by hand.
        </p>
      {:else if cliMissing}
        <p class="notice">
          A {serviceKind} background service is available here, but AllMyStuff couldn't find the
          <code>allmystuff</code> command-line tool that manages it. Reinstall AllMyStuff (the CLI
          ships alongside the app), or point <code>ALLMYSTUFF_CLI_BIN</code> at it.
        </p>
        <div class="actions">
          <button class="btn" disabled={busy} onclick={() => app.loadServiceStatus()}>Retry</button>
        </div>
      {:else if statusError}
        <p class="notice">Couldn't read the service status: {statusError}</p>
        <div class="actions">
          <button class="btn" disabled={busy} onclick={() => app.loadServiceStatus()}>Retry</button>
        </div>
      {:else if !installed}
        <div class="actions">
          <button class="btn primary" disabled={busy} onclick={() => app.installService()}>
            {busy ? "Installing…" : "Install as a service"}
          </button>
        </div>
        {#if svc?.needs_privilege}
          <p class="fineprint">Windows will ask for administrator approval to install it.</p>
        {/if}
      {:else}
        <div class="actions">
          {#if running}
            <button class="btn" disabled={busy} onclick={() => app.stopService()}>Stop</button>
            <button class="btn" disabled={busy} onclick={() => app.restartService()}>Restart</button>
          {:else}
            <button class="btn primary" disabled={busy} onclick={() => app.startService()}>Start</button>
          {/if}
          <button class="btn danger" class:armed={armedUninstall} disabled={busy} onclick={uninstall}>
            {armedUninstall ? "Click to confirm" : "Uninstall"}
          </button>
        </div>
        <div class="meta">
          <span>Starts at {startsWhen}: <b>{enabled ? "yes" : "no"}</b></span>
          <span>Status: <b>{running ? "running" : "stopped"}</b></span>
        </div>
      {/if}
    </section>

    <!-- Window behaviour -->
    <section class="block">
      <div class="title">Window</div>
      <label class="toggle">
        <input
          type="checkbox"
          checked={wb?.close_to_tray ?? true}
          onchange={(e) => app.setWindowBehavior({ close_to_tray: e.currentTarget.checked })}
        />
        <span>
          <b>Closing keeps it running</b>
          <span class="hint">
            The window's close button hides AllMyStuff to the {trayWord}; its icon there brings it
            back. Quit from the {trayWord} icon's menu to exit for real.
          </span>
        </span>
      </label>
      <label class="toggle">
        <input
          type="checkbox"
          checked={wb?.minimize_to_tray ?? false}
          onchange={(e) => app.setWindowBehavior({ minimize_to_tray: e.currentTarget.checked })}
        />
        <span>
          <b>Minimize to the {trayWord}</b>
          <span class="hint">Minimizing hides the window to the {trayWord} instead of the taskbar.</span>
        </span>
      </label>
    </section>
  {/if}
</div>

<style>
  .section {
    display: flex;
    flex-direction: column;
  }
  h3 {
    margin: 0 0 0.4rem;
    font-size: 1.2rem;
  }
  .lead {
    color: var(--ink-soft);
    font-size: 0.84rem;
    line-height: 1.5;
    margin: 0 0 1.1rem;
  }
  .block {
    border-top: 1px solid var(--line);
    padding: 0.9rem 0;
  }
  .block:first-of-type {
    border-top: none;
    padding-top: 0.2rem;
  }
  .row {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 1rem;
  }
  .grow {
    min-width: 0;
  }
  .title {
    font-size: 0.95rem;
    font-weight: 600;
  }
  .hint {
    font-size: 0.78rem;
    color: var(--ink-soft);
    margin: 0.2rem 0 0;
    line-height: 1.45;
    display: block;
  }
  .pill {
    flex-shrink: 0;
    font-size: 0.72rem;
    font-weight: 700;
    padding: 0.2rem 0.55rem;
    border-radius: var(--r-pill);
    background: var(--surface-2);
    color: var(--ink-faint);
    white-space: nowrap;
  }
  .pill.on {
    background: var(--ok-soft, var(--surface-2));
    color: var(--ok);
  }
  .pill.idle {
    background: var(--warn-soft, var(--surface-2));
    color: var(--warn);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
    margin-top: 0.9rem;
  }
  .danger {
    color: var(--danger);
    border-color: var(--danger);
  }
  .danger.armed {
    background: var(--danger);
    color: #fff;
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    gap: 1.2rem;
    margin-top: 0.8rem;
    font-size: 0.8rem;
    color: var(--ink-soft);
  }
  .fineprint {
    font-size: 0.76rem;
    color: var(--ink-faint);
    margin: 0.5rem 0 0;
  }
  .notice {
    font-size: 0.82rem;
    color: var(--ink-soft);
    background: var(--surface-2);
    border-radius: var(--r-sm);
    padding: 0.6rem 0.7rem;
    margin: 0.2rem 0 0;
    line-height: 1.45;
  }
  code {
    font-family: var(--mono);
    font-size: 0.78rem;
    background: var(--surface-2);
    padding: 0.05rem 0.3rem;
    border-radius: var(--r-sm);
  }
  .toggle {
    display: flex;
    align-items: flex-start;
    gap: 0.6rem;
    cursor: pointer;
    margin-top: 0.7rem;
  }
  .toggle:first-of-type {
    margin-top: 0.4rem;
  }
  .toggle input {
    margin-top: 0.2rem;
  }
</style>
