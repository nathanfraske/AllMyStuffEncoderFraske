//! `allmystuff-serve` — run this machine as a mesh node with **no GUI**.
//!
//! This is the headless half of AllMyStuff. The desktop app's Tauri backend
//! and this binary link the very same engine ([`allmystuff_node`]); the only
//! difference is what's wired to [`UiSink`] — the app feeds events to its
//! Svelte front-end, and here they just go to the log, because there's no
//! front-end to feed. Everything a peer can do *to* this machine still works:
//! a console connecting to watch its screen (**monitor out**), a fleet member
//! opening a terminal, a room asking for its camera or system audio.
//!
//! It is also self-contained as a unit: AllMyStuff is a client of a
//! `myownmesh serve` daemon, so this binary **spawns and supervises that
//! daemon itself** (reusing the same logic the GUI uses). One process brings
//! up both — which is what lets a single `allmystuff service` install run the
//! whole node on a headless box. Users normally reach it as `allmystuff
//! serve`, which execs this binary.
//!
//! ```text
//! allmystuff serve                       # run this machine on the mesh, headless
//! ALLMYSTUFF_CLAIMABLE=1 allmystuff serve # …and let one of your machines adopt it
//! ALLMYSTUFF_LOG=debug allmystuff serve   # …with verbose logs
//! allmystuff serve --log debug            # …same, as a flag
//! ```
//!
//! **Windows service mode.** `allmystuff service install` registers this binary
//! with the Service Control Manager as `<exe> --service`. That flag flips it
//! into [`winsvc`] mode: it answers the SCM's control protocol (a plain console
//! binary would be killed for not doing so) and logs to a file (no console to
//! print to). systemd and launchd need no such mode — they run the binary as
//! an ordinary foreground process and signal it with SIGTERM.
//!
//! **Unattended self-update.** Headless and as a service the node is meant to
//! be "always on, always current": its background updater doesn't just stage a
//! release for the next launch (a service box might not restart for months) —
//! it applies the update and relaunches onto it, on all three OSes. See
//! [`allmystuff_updater::tick_forever_unattended`].

use std::future::Future;
use std::process::ExitCode;
use std::sync::Arc;

use allmystuff_node::control_client::ControlClient;
use allmystuff_node::daemon_spawn::{self, DaemonChild};
use allmystuff_node::mesh::Mesh;
use allmystuff_node::networks_store::DisabledNetworks;
use allmystuff_node::node_control::{self, SocketSink};
use allmystuff_node::UiSink;

/// Headless event sink. The engine's events (`allmystuff://session`,
/// `…/video-ready`, room/owned updates, …) are all front-end concerns, so a
/// node with no webview logs them at `trace` for debugging and otherwise
/// lets them fall on the floor.
struct LogSink;

impl UiSink for LogSink {
    fn emit(&self, event: &str, _payload: serde_json::Value) {
        tracing::trace!(event, "node event (headless: no UI listening)");
    }

    fn restart(&self) -> ! {
        // The fleet "upgrade this machine" path applied a new build and
        // wants us to run it. The GUI relaunches its window; headless, we
        // re-exec ourselves so the next process picks up the staged update
        // (`apply_pending_if_any` at startup). A service manager would also
        // restart us, but re-execing makes the upgrade land immediately even
        // when run straight from a shell.
        reexec_self()
    }
}

/// Replace this process with a fresh copy of itself, carrying the same args.
/// Returns only on failure (then exits), so the signature is `-> !`.
fn reexec_self() -> ! {
    match std::env::current_exe() {
        Ok(exe) => {
            let args: Vec<String> = std::env::args().skip(1).collect();
            tracing::info!("restarting onto the updated build: {}", exe.display());
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt as _;
                // `exec` only returns if it failed to replace the image.
                let err = std::process::Command::new(&exe).args(&args).exec();
                tracing::error!("re-exec failed: {err}");
            }
            #[cfg(not(unix))]
            {
                if let Err(e) = std::process::Command::new(&exe).args(&args).spawn() {
                    tracing::error!("couldn't relaunch: {e}");
                }
            }
        }
        Err(e) => tracing::error!("couldn't locate own executable to restart: {e}"),
    }
    std::process::exit(0);
}

/// Relaunch hook for the **Windows service** path. A service can't re-exec
/// itself the way a console process can — the SCM only tracks the original
/// process, so a spawned child would orphan and the SCM would think the
/// service died. Instead we exit non-zero: with the install's configured
/// restart-on-failure action, the SCM brings the service straight back up,
/// running the freshly-applied binary at the same ImagePath. The supervised
/// daemon dies with us (its kill-on-close job object) and the new process
/// respawns it.
#[cfg(windows)]
fn service_relaunch() -> ! {
    tracing::info!(
        "self-update applied; exiting so the Service Control Manager restarts the updated node"
    );
    std::process::exit(1);
}

/// Pick the relaunch the background updater uses when it applies a release.
fn pick_relaunch(as_service: bool) -> fn() -> ! {
    #[cfg(windows)]
    {
        if as_service {
            return service_relaunch;
        }
    }
    let _ = as_service; // unix services re-exec (execve keeps the PID)
    reexec_self
}

fn main() -> ExitCode {
    // Windows registers this binary as `<exe> --service`; that flag is what
    // tells us to speak the SCM control protocol instead of running in the
    // foreground. Off Windows there's no such mode.
    #[cfg(windows)]
    let as_service = std::env::args().skip(1).any(|a| a == "--service");
    #[cfg(not(windows))]
    let as_service = false;

    // Apply any update staged on a previous run before binding anything —
    // same "stage now, apply on next launch" model as the GUI and the daemon.
    allmystuff_updater::apply_pending_if_any();

    init_logging(as_service);

    // As a Windows service, hand off to the SCM dispatcher: it runs the node on
    // its own thread (see `winsvc::run_service`) and blocks until stopped.
    #[cfg(windows)]
    if as_service {
        return winsvc::dispatch();
    }

    // Foreground (a console, or a systemd/launchd-supervised process): run
    // until a stop signal arrives.
    run_blocking(as_service, wait_for_shutdown_signal())
}

/// Build the async runtime and run the node to completion, stopping when
/// `shutdown` resolves. Shared by the foreground path (a signal future) and
/// the Windows service path (an SCM-stop future).
fn run_blocking<F: Future<Output = ()>>(as_service: bool, shutdown: F) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("allmystuff serve: failed to build async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(run(as_service, shutdown))
}

async fn run<F: Future<Output = ()>>(as_service: bool, shutdown: F) -> ExitCode {
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "allmystuff node starting"
    );

    // One node per machine. The node control socket is the guard: bind it
    // before bringing up any mesh. A *live* node already holding it means this
    // machine is already served (a running Always-On service, or the desktop
    // app's spawned node) — starting a second mesh would put two nodes under
    // one identity and then nothing connects, so step aside cleanly. Binding
    // before the mesh starts is also what makes two simultaneously-starting
    // nodes safe (see `bind_control_socket`).
    let shutdown = std::pin::pin!(shutdown);
    let control_listener = match node_control::bind_control_socket().await {
        Ok(listener) => listener,
        Err(e) => {
            tracing::info!("not starting a second node ({e:#})");
            return ExitCode::SUCCESS;
        }
    };

    let client = match ControlClient::new() {
        Ok(c) => Arc::new(c),
        Err(e) => {
            tracing::error!("couldn't resolve the control socket path: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Bring up the mesh daemon this node rides on. We supervise it: when this
    // binary spawned it, dropping the handle on shutdown kills it too, so one
    // service really does run both. An already-running daemon (someone else's,
    // or the GUI's) is reused and left untouched.
    let _daemon: Option<DaemonChild> = match daemon_spawn::ensure_daemon_running(&client).await {
        Ok(child) => child,
        Err(e) => {
            tracing::warn!(
                "couldn't start the myownmesh daemon ({e:#}); will try to use one if it appears. \
                 Install it (so it's on PATH), set MYOWNMESH_BIN, or run `myownmesh serve` yourself."
            );
            None
        }
    };

    // Wire the engine to a sink that both logs (the headless `LogSink`) and
    // broadcasts every event to clients of the node control socket — the seam
    // a thin GUI drives the node over (Phase A). The broadcaster is shared
    // with the control server spawned below, and `DisabledNetworks` is the
    // park store the server's `network_set_enabled` command needs.
    let broadcaster = node_control::new_broadcaster();
    let (event_tx, event_rx) = node_control::event_channel();
    let disabled = Arc::new(DisabledNetworks::load());
    let sink = SocketSink::new(Arc::new(LogSink), event_tx);
    let mesh = Mesh::new(client.clone(), Arc::new(sink));
    mesh.clone().start().await;

    // Serve the node control + event socket (on the listener bound up front) so
    // the desktop app drives this node over it instead of running its own mesh.
    tokio::spawn({
        let mesh = mesh.clone();
        let client = client.clone();
        let disabled = disabled.clone();
        async move {
            if let Err(e) = node_control::serve(
                control_listener,
                mesh,
                client,
                disabled,
                broadcaster,
                event_rx,
            )
            .await
            {
                tracing::warn!("node control socket stopped: {e:#}");
            }
        }
    });

    // Self-update ticker: a headless node checks the release feed on its own
    // and, unlike the desktop app's "stage + offer relaunch", *applies* what
    // its policy permits and relaunches onto it — so an always-on box that
    // never gets manually restarted still keeps every half (CLI/GUI/node)
    // current. No-ops when auto-update is off or this is a package-managed
    // install. Under a Windows service the relaunch hands back to the SCM.
    tokio::spawn(allmystuff_updater::tick_forever_unattended(pick_relaunch(
        as_service,
    )));

    match mesh.resolve_local_id().await {
        Some(id) => tracing::info!(device_id = %id, "serving this machine on the mesh"),
        None => tracing::warn!(
            "serving, but couldn't read this device's mesh identity yet — \
             is the daemon up? (the event pump will keep retrying)"
        ),
    }
    if allmystuff_node::ownership::Ownership::load().claimable() {
        tracing::info!(
            "claim mode is on (ALLMYSTUFF_CLAIMABLE) — one of your machines can adopt this one"
        );
    }
    tracing::info!("node is up — press Ctrl-C to stop");

    // Run until asked to stop. Holding `mesh` and `_daemon` in scope keeps the
    // pump alive and the supervised daemon running for the node's whole life.
    shutdown.await;
    tracing::info!("shutdown requested — stopping");

    // `_daemon` drops here, killing the daemon we spawned (if any).
    drop(mesh);
    ExitCode::SUCCESS
}

/// Wait for SIGINT (Ctrl-C) or SIGTERM (service stop), mirroring the daemon.
async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigint = match signal(SignalKind::interrupt()) {
            Ok(s) => s,
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => {
                let _ = sigint.recv().await;
                return;
            }
        };
        tokio::select! {
            _ = sigint.recv() => {}
            _ = sigterm.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

/// Initialise tracing. A console build logs to stderr; a Windows service has no
/// console, so it logs to a file under `%ProgramData%\AllMyStuff\logs\`.
fn init_logging(as_service: bool) {
    let filter = tracing_subscriber::EnvFilter::new(resolve_log_filter());

    #[cfg(windows)]
    if as_service {
        if let Some(make) = winsvc::log_writer() {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_target(false)
                .with_ansi(false)
                .with_writer(make)
                .init();
            return;
        }
    }

    #[cfg(not(windows))]
    let _ = as_service;

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

/// The log filter: `--log <filter>` wins, then `ALLMYSTUFF_LOG`, then a quiet
/// default (our crates at info, everything else off).
fn resolve_log_filter() -> String {
    if let Some(f) = arg_value("--log") {
        return f;
    }
    std::env::var("ALLMYSTUFF_LOG")
        .unwrap_or_else(|_| "info,allmystuff_node=info,allmystuff_serve=info".to_string())
}

/// The value following `flag` in this process's argv, if present.
fn arg_value(flag: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .filter(|v| !v.starts_with('-'))
        .cloned()
}

// ---------------------------------------------------------------------------
// Windows Service Control Manager glue
// ---------------------------------------------------------------------------

/// Speaks the SCM control protocol so `allmystuff-serve --service` runs as a
/// real Windows service. `allmystuff service install` registers the binary
/// with `sc.exe`; the SCM then launches it, and [`dispatch`] connects it to
/// the service control dispatcher. The node itself is unchanged — it just runs
/// under an SCM-stop future instead of a signal one.
#[cfg(windows)]
mod winsvc {
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::process::ExitCode;
    use std::time::Duration;

    use windows_service::define_windows_service;
    use windows_service::service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::service_dispatcher;

    /// Must match the SCM service name `allmystuff service install` creates
    /// (`WINDOWS_SERVICE_NAME` in the CLI's `service.rs`); the control handler
    /// can't bind otherwise.
    const SERVICE_NAME: &str = "AllMyStuff";
    const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

    define_windows_service!(ffi_service_main, service_main);

    /// Hand this process to the SCM. The dispatcher runs the service on its own
    /// thread (calling [`service_main`]) and blocks until the service stops.
    pub fn dispatch() -> ExitCode {
        match service_dispatcher::start(SERVICE_NAME, ffi_service_main) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                tracing::error!("service dispatcher failed to start: {e}");
                ExitCode::FAILURE
            }
        }
    }

    fn service_main(_args: Vec<OsString>) {
        if let Err(e) = run_service() {
            tracing::error!("windows service stopped with error: {e}");
        }
    }

    fn run_service() -> windows_service::Result<()> {
        // The SCM control handler runs on its own thread. Bridge a Stop into
        // the node's async shutdown with a tokio channel: an unbounded sender
        // can fire from sync code with no runtime in scope, so the handler is
        // free to call it directly.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let handler = move |control| -> ServiceControlHandlerResult {
            match control {
                ServiceControl::Stop | ServiceControl::Shutdown => {
                    let _ = tx.send(());
                    ServiceControlHandlerResult::NoError
                }
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                _ => ServiceControlHandlerResult::NotImplemented,
            }
        };
        let status_handle = service_control_handler::register(SERVICE_NAME, handler)?;

        // Report running straight away (the node starts asynchronously), then
        // run it until the SCM asks us to stop.
        status_handle.set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: ServiceState::Running,
            controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })?;

        // `as_service = true`: the self-updater relaunches by exiting for the
        // SCM to restart, not by re-execing.
        super::run_blocking(true, async move {
            let _ = rx.recv().await;
        });

        status_handle.set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: ServiceState::Stopped,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })?;
        Ok(())
    }

    /// A `MakeWriter` over `%ProgramData%\AllMyStuff\logs\service.log` (append),
    /// so a service with no console still leaves a log. `None` if the file
    /// can't be opened, in which case logging falls back to stderr.
    pub fn log_writer() -> Option<impl Fn() -> std::fs::File + Send + Sync + 'static> {
        let dir = std::env::var_os("ProgramData")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("C:\\ProgramData"))
            .join("AllMyStuff")
            .join("logs");
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join("service.log");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()?;
        Some(move || file.try_clone().expect("clone service log file handle"))
    }
}
