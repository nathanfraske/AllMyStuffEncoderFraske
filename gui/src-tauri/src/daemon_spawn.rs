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

/// Compare the answering daemon's version against the app's pin and log
/// the verdict — loudly on mismatch. A stale daemon is the #1 way "the
/// app updated but a feature didn't appear" happens: the sidecar
/// resolution prefers a sibling dev build, and a long-lived daemon
/// process survives app upgrades entirely. Only meaningful when the pin
/// is a version tag (`vX.Y.Z`); a sha pin can't be compared.
pub async fn log_daemon_version(client: &ControlClient) {
    let Some(pin) = option_env!("MYOWNMESH_PIN") else {
        return;
    };
    let Some(pinned) = pin.strip_prefix('v').filter(|p| !p.is_empty()) else {
        return;
    };
    let running = client
        .request(&Request::Status)
        .await
        .ok()
        .and_then(|r| r.data)
        .and_then(|d| d.get("version").and_then(|v| v.as_str()).map(String::from));
    match running {
        Some(v) if v == pinned => tracing::info!("myownmesh daemon v{v} (matches the {pin} pin)"),
        Some(v) => tracing::warn!(
            "myownmesh daemon is v{v} but this app pins {pin} — features the newer daemon carries (e.g. the video track lane) will be unavailable. If this is a dev setup, rebuild the sibling MyOwnMesh checkout (or remove its stale binary so build.rs fetches the pinned release) and restart the app."
        ),
        None => tracing::warn!("couldn't read the daemon version to compare against the {pin} pin"),
    }
}

/// The target triple `build.rs` bundled the sidecar for. In a dev build
/// Tauri keeps the `-<triple>` suffix on the staged sidecar; a production
/// bundle strips it to plain `myownmesh{.exe}`.
const DAEMON_SIDECAR_TRIPLE: &str = env!("DAEMON_SIDECAR_TRIPLE");

/// True when a path is a real, non-empty binary (not a zero-byte sidecar
/// stub `build.rs` wrote when it couldn't fetch the daemon).
fn usable(p: &std::path::Path) -> bool {
    p.metadata()
        .map(|m| m.is_file() && m.len() > 0)
        .unwrap_or(false)
}

/// Locate the `myownmesh` daemon. It normally ships *with the app* — bundled
/// as a Tauri sidecar by `build.rs` (see that file) — so this resolves the
/// bundled binary first and only falls back for unusual setups:
///
/// 1. `MYOWNMESH_BIN` override.
/// 2. **Bundled sidecar** next to the app binary — `myownmesh{.exe}`
///    (production) or `myownmesh-<triple>{.exe}` (dev).
/// 3. **Dev source slot** — `gui/src-tauri/binaries/myownmesh-<triple>`
///    (the path `build.rs` writes; reachable in a dev run via the
///    build-time manifest dir).
/// 4. Side-by-side `../MyOwnMesh` source build.
/// 5. `myownmesh` on `$PATH`.
pub fn find_daemon_binary() -> Result<PathBuf> {
    let exe = if cfg!(windows) {
        "myownmesh.exe"
    } else {
        "myownmesh"
    };
    let exe_triple = if cfg!(windows) {
        format!("myownmesh-{DAEMON_SIDECAR_TRIPLE}.exe")
    } else {
        format!("myownmesh-{DAEMON_SIDECAR_TRIPLE}")
    };

    // 1. Override.
    if let Ok(p) = std::env::var("MYOWNMESH_BIN") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Ok(p);
        }
    }

    // 2. Bundled sidecar next to the running app binary.
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(dir) = exe_path.parent() {
            for name in [exe, exe_triple.as_str()] {
                let p = dir.join(name);
                if usable(&p) {
                    return Ok(p);
                }
            }
        }
    }

    // 3. Dev source slot written by build.rs (build-time manifest dir).
    let dev_slot = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("binaries")
        .join(&exe_triple);
    if usable(&dev_slot) {
        return Ok(dev_slot);
    }

    // 4. Side-by-side MyOwnMesh checkout (release first, then debug).
    for profile in ["release", "debug"] {
        if let Some(p) = sibling_myownmesh_path(profile, exe) {
            if p.exists() {
                return Ok(p);
            }
        }
    }

    // 5. PATH walk (skip stale, non-existent entries).
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(exe);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }
    Err(anyhow!(
        "couldn't find the `myownmesh` daemon — it normally ships bundled with \
         the app; build from source (so build.rs bundles it), put `myownmesh` \
         on PATH, or set MYOWNMESH_BIN"
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
        log_daemon_version(client).await;
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
            log_daemon_version(client).await;
            return Ok(Some(handle));
        }
    }
    tracing::warn!(
        "daemon did not answer within 8s; leaving it running — the event pump will retry"
    );
    Ok(Some(handle))
}
