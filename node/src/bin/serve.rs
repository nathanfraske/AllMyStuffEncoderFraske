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
//! ```

use std::process::ExitCode;
use std::sync::Arc;

use allmystuff_node::control_client::ControlClient;
use allmystuff_node::daemon_spawn::{self, DaemonChild};
use allmystuff_node::mesh::Mesh;
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

fn main() -> ExitCode {
    // Apply any update staged on a previous run before binding anything —
    // same "stage now, apply on next launch" model as the GUI and the daemon.
    allmystuff_updater::apply_pending_if_any();

    // Default log filter: our crates at info, the rest quiet. Override with
    // `ALLMYSTUFF_LOG` (e.g. `debug`, or `info,allmystuff_node=debug`).
    let log_level = std::env::var("ALLMYSTUFF_LOG")
        .unwrap_or_else(|_| "info,allmystuff_node=info,allmystuff_serve=info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(log_level))
        .with_target(false)
        .init();

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

    runtime.block_on(run())
}

async fn run() -> ExitCode {
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "allmystuff node starting"
    );

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

    // Wire the engine to a headless sink and bring the session online
    // (subscribe, advertise presence + capabilities, start the event pump).
    let mesh = Mesh::new(client.clone(), Arc::new(LogSink));
    mesh.clone().start().await;

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
    wait_for_shutdown_signal().await;
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
