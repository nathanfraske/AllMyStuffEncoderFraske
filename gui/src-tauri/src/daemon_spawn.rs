//! Daemon lifecycle. AllMyStuff drives a `myownmesh serve` daemon as its
//! mesh sidecar; if one isn't already listening, we spawn it and hold the
//! handle for the app's lifetime (killing the GUI kills the child via
//! `Drop`). This mirrors the MyOwnMesh GUI's daemon spawner, minus the
//! source-checkout dev path — AllMyStuff ships against an installed
//! `myownmesh` (pinned in `.myownmesh-rev`), found on `$PATH` or via the
//! `MYOWNMESH_BIN` override.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

use crate::control_client::{ControlClient, Request};

/// Owned wrapper around a spawned `myownmesh serve` child. Dropping it
/// kills the child.
pub struct DaemonChild {
    child: Option<Child>,
}

impl DaemonChild {
    fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }
}

impl Drop for DaemonChild {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
            tracing::info!("myownmesh daemon child terminated");
        }
    }
}

/// True when a daemon is already answering on the control socket.
pub async fn probe(client: &ControlClient) -> bool {
    client.request(&Request::Status).await.is_ok()
}

/// Locate the `myownmesh` binary. Discovery order:
///
/// 1. `MYOWNMESH_BIN` environment variable (manual override).
/// 2. A side-by-side `MyOwnMesh` source checkout's build artefacts
///    (`../MyOwnMesh/target/{debug,release}/myownmesh`) — so a developer
///    working on both repos who's run `cargo build` over there gets live
///    mesh from `just dev` without installing anything.
/// 3. `myownmesh` on `$PATH` (the production / `just mesh-install` path).
pub fn find_daemon_binary() -> Result<PathBuf> {
    let exe = if cfg!(windows) { "myownmesh.exe" } else { "myownmesh" };

    if let Ok(p) = std::env::var("MYOWNMESH_BIN") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Ok(p);
        }
    }
    // Side-by-side dev checkout (debug first — `just dev` over there builds
    // debug; then release).
    for profile in ["debug", "release"] {
        if let Some(p) = sibling_myownmesh_path(profile, exe) {
            if p.exists() {
                return Ok(p);
            }
        }
    }
    // Manual PATH walk (rather than Command's implicit search) so we skip
    // stale, non-existent entries and can report the resolved location.
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(exe);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }
    Err(anyhow!(
        "couldn't find `{exe}` — run `just mesh-install` (installs the version \
         pinned in .myownmesh-rev), put it on PATH, or set MYOWNMESH_BIN"
    ))
}

/// `../MyOwnMesh/target/<profile>/myownmesh` relative to the AllMyStuff repo
/// root. CARGO_MANIFEST_DIR here is `gui/src-tauri`, so the repo root is two
/// parents up and the sibling checkout one more.
fn sibling_myownmesh_path(profile: &str, exe: &str) -> Option<PathBuf> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    Some(
        PathBuf::from(manifest_dir)
            .parent()? // gui/
            .parent()? // AllMyStuff/
            .parent()? // workspace dir (AllMyStuff + MyOwnMesh side by side)
            .join("MyOwnMesh")
            .join("target")
            .join(profile)
            .join(exe),
    )
}

/// Spawn `myownmesh serve` and wait briefly for its socket. Returns
/// `Ok(None)` when a daemon is already running (we reuse it).
pub async fn ensure_daemon_running(client: &ControlClient) -> Result<Option<DaemonChild>> {
    if probe(client).await {
        tracing::info!("existing myownmesh daemon found on the control socket");
        return Ok(None);
    }

    let bin = find_daemon_binary().context("locate myownmesh binary")?;
    tracing::info!(?bin, "spawning myownmesh daemon");

    let child = Command::new(&bin)
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("spawn {}", bin.display()))?;
    let handle = DaemonChild::new(child);

    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    while std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(150)).await;
        if probe(client).await {
            tracing::info!("myownmesh daemon up");
            return Ok(Some(handle));
        }
    }
    tracing::warn!("daemon did not answer within 8s; leaving it running — the event pump will retry");
    Ok(Some(handle))
}
