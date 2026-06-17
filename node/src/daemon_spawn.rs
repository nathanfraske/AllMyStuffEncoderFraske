//! Daemon lifecycle. AllMyStuff drives a `myownmesh serve` daemon as its
//! mesh sidecar; if one isn't already listening, we spawn it and hold the
//! handle for the app's lifetime (killing the GUI kills the child via
//! `Drop`). This mirrors the MyOwnMesh GUI's daemon spawner, minus the
//! source-checkout dev path — AllMyStuff ships against an installed
//! `myownmesh` (pinned in `.myownmesh-rev`), found on `$PATH` or via the
//! `MYOWNMESH_BIN` override. A binary that's fallen behind the pin is
//! asked to update itself (`myownmesh update`, the same thing the
//! installer invokes) before we start it, so the mesh comes up with the
//! features this app was built against.

use std::path::{Path, PathBuf};
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

/// Tie the spawned daemon's lifetime to this process at the OS level —
/// the `Drop` kill above only covers a *clean* exit, and an app that
/// crashes or is force-killed (taskkill, a dev-loop Ctrl-C that doesn't
/// reach us, an OOM kill) orphans the daemon. An orphan is worse than a
/// leak: it keeps this machine's identity live on the mesh and silently
/// swallows control traffic addressed to the dead app, and on Windows it
/// also holds the sidecar exe locked so the *next build* fails copying it.
/// Only ever called for a daemon **we spawned** — an externally-started
/// daemon we merely reuse is never touched.
///
///  * **Windows**: assign the child to a job object with
///    `KILL_ON_JOB_CLOSE`. The job handle is deliberately leaked — the
///    kernel closes it when this process ends (any way at all), and that
///    closure kills the daemon.
///  * **Linux**: `PR_SET_PDEATHSIG(SIGKILL)` (set in `pre_exec` at spawn).
///  * **macOS**: no kernel-level parent-death signal exists; the `Drop`
///    kill remains the cover for clean exits.
#[cfg(windows)]
fn tie_daemon_lifetime(child: &Child) {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            tracing::warn!("couldn't create a job object for the daemon — a crash may orphan it");
            return;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) != 0
            && AssignProcessToJobObject(job, child.as_raw_handle() as _) != 0;
        if ok {
            tracing::info!("daemon tied to this process (job object, kill-on-close)");
            // The job handle must live exactly as long as this process:
            // leaking it hands the close — and so the kill — to the kernel.
        } else {
            tracing::warn!("couldn't tie the daemon to this process — a crash may orphan it");
            CloseHandle(job);
        }
    }
}

#[cfg(not(windows))]
fn tie_daemon_lifetime(_child: &Child) {
    // Linux is handled in `pre_exec` (PR_SET_PDEATHSIG); macOS has no
    // kernel-level equivalent.
}

/// True when a daemon is already answering on the control socket.
pub async fn probe(client: &ControlClient) -> bool {
    client.request(&Request::Status).await.is_ok()
}

/// `"v0.2.4"` / `"0.2.4"` / `"0.2.4-rc.1"` → `(0, 2, 4)`. Missing
/// minor/patch fields compare as 0 (the installer's `version_ge` does
/// the same). `None` when the major field isn't numeric — callers gate
/// sha pins out themselves (they don't start with `v`).
fn parse_semverish(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim();
    let s = s.strip_prefix('v').unwrap_or(s);
    let mut nums = [None::<u64>; 3];
    for (i, part) in s.splitn(3, '.').enumerate() {
        let end = part
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(part.len());
        nums[i] = part[..end].parse().ok();
    }
    Some((nums[0]?, nums[1].unwrap_or(0), nums[2].unwrap_or(0)))
}

/// First line of `myownmesh --version` ("myownmesh 0.2.4") → `(0, 2, 4)`.
fn parse_version_output(out: &str) -> Option<(u64, u64, u64)> {
    parse_semverish(out.lines().next()?.split_whitespace().last()?)
}

fn fmt_ver((a, b, c): (u64, u64, u64)) -> String {
    format!("{a}.{b}.{c}")
}

/// The app's daemon pin, when it's a comparable version tag (`vX.Y.Z`).
/// A sha pin can't be compared, so every version passes then — same
/// rule as the installer's `mesh_min_version`.
fn pinned_version() -> Option<(&'static str, (u64, u64, u64))> {
    let pin = option_env!("MYOWNMESH_PIN")?;
    let want = parse_semverish(pin.strip_prefix('v')?)?;
    Some((pin, want))
}

/// `bin --version`, parsed. `None` when the binary won't answer.
async fn binary_version(bin: &Path) -> Option<(u64, u64, u64)> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let out = tokio::time::timeout(Duration::from_secs(10), cmd.output())
        .await
        .ok()?
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_version_output(&String::from_utf8_lossy(&out.stdout))
}

/// `myownmesh update` downloads a release binary, so give it real time —
/// but never wedge mesh bring-up forever on a stalled network.
const DAEMON_UPDATE_TIMEOUT: Duration = Duration::from_secs(180);

/// Run `<bin> update` — the daemon's own self-updater, the same thing
/// the installer invokes — and report whether the binary on disk now
/// satisfies the pin. Its output is folded into our log; failure never
/// propagates (an old daemon still beats no daemon).
async fn run_daemon_update(bin: &Path) -> bool {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.arg("update")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    match tokio::time::timeout(DAEMON_UPDATE_TIMEOUT, cmd.output()).await {
        Err(_) => {
            tracing::warn!(
                "myownmesh update didn't finish within {}s — continuing with what's on disk",
                DAEMON_UPDATE_TIMEOUT.as_secs()
            );
        }
        Ok(Err(e)) => tracing::warn!("couldn't run myownmesh update: {e}"),
        Ok(Ok(out)) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let said = stdout.trim();
            if !said.is_empty() {
                tracing::info!("myownmesh update: {said}");
            }
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                tracing::warn!("myownmesh update failed: {}", stderr.trim());
            }
        }
    }
    // The re-check is what decides — `update` may have been refused
    // (package-manager install), failed, or landed exactly the pin.
    match (pinned_version(), binary_version(bin).await) {
        (Some((_, want)), Some(have)) => have >= want,
        _ => false,
    }
}

/// When the daemon binary we're about to start is older than the app's
/// pin, update it first. A stale daemon is the #1 way "the app updated
/// but a feature didn't appear" happens — it answers the socket fine,
/// so everything *looks* up, but the newer media lanes (the video track
/// lane screens ride, the Opus audio lane) simply don't exist in it.
async fn ensure_daemon_current(bin: &Path) {
    let Some((pin, want)) = pinned_version() else {
        return;
    };
    match binary_version(bin).await {
        None => tracing::warn!(
            "couldn't read {}'s version to compare against the {pin} pin",
            bin.display()
        ),
        Some(have) if have >= want => {}
        Some(have) => {
            tracing::info!(
                "myownmesh at {} is v{} but this app pins {pin} — asking it to update itself (myownmesh update)…",
                bin.display(),
                fmt_ver(have)
            );
            if run_daemon_update(bin).await {
                tracing::info!("myownmesh is current — starting the updated daemon");
            } else {
                tracing::warn!(
                    "couldn't bring myownmesh up to {pin}; starting the old daemon — the newer mesh features (e.g. the video track lane that screens ride) stay unavailable. Update it by hand: myownmesh update"
                );
            }
        }
    }
}

/// Compare the answering daemon's version against the app's pin and log
/// the verdict — loudly on mismatch. Returns `true` when the daemon is
/// confirmed older than the pin. Only meaningful when the pin is a
/// version tag (`vX.Y.Z`); a sha pin can't be compared.
pub async fn log_daemon_version(client: &ControlClient) -> bool {
    let Some((pin, want)) = pinned_version() else {
        return false;
    };
    let running = client
        .request(&Request::Status)
        .await
        .ok()
        .and_then(|r| r.data)
        .and_then(|d| d.get("version").and_then(|v| v.as_str()).map(String::from));
    match running {
        Some(v) => match parse_semverish(&v) {
            Some(have) if have >= want => {
                tracing::info!("myownmesh daemon v{v} (satisfies the {pin} pin)");
                false
            }
            Some(_) => {
                tracing::warn!(
                    "myownmesh daemon is v{v} but this app pins {pin} — features the newer daemon carries (e.g. the video track lane) will be unavailable. If this is a dev setup, rebuild the sibling MyOwnMesh checkout (or remove its stale binary so build.rs fetches the pinned release) and restart the app."
                );
                true
            }
            None => {
                tracing::warn!(
                    "myownmesh daemon reported an unreadable version ({v}) against the {pin} pin"
                );
                false
            }
        },
        None => {
            tracing::warn!("couldn't read the daemon version to compare against the {pin} pin");
            false
        }
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

/// Where `find_daemon_binary` found the daemon — decides whether the
/// binary is ours to keep current.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DaemonSource {
    /// Explicit `MYOWNMESH_BIN` override — deliberately pinned, never
    /// touched.
    Override,
    /// Installed for the user: the production bundle's sidecar or a
    /// `myownmesh` on `$PATH` (what the installer drops). Kept current
    /// against the pin by asking it to update itself.
    Installed,
    /// A dev artifact (the dev-staged sidecar, the `build.rs` source
    /// slot, a sibling checkout's target dir) — never touched;
    /// self-updating one would clobber build output with a release
    /// download.
    DevBuild,
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
pub fn find_daemon_binary() -> Result<(PathBuf, DaemonSource)> {
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
            return Ok((p, DaemonSource::Override));
        }
    }

    // 2. Bundled sidecar next to the running app binary. The plain name
    // is the production bundle; the triple-suffixed one is Tauri's dev
    // staging of the source slot.
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(dir) = exe_path.parent() {
            for (name, source) in [
                (exe, DaemonSource::Installed),
                (exe_triple.as_str(), DaemonSource::DevBuild),
            ] {
                let p = dir.join(name);
                if usable(&p) {
                    return Ok((p, source));
                }
            }
        }
    }

    // 3. Dev source slot written by the GUI's build.rs. That stages the
    // sidecar under `gui/src-tauri/binaries`; this engine crate sits beside
    // `gui/` at the repo root (`node/`), so reach across to it from the
    // build-time manifest dir.
    if let Some(dev_slot) = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent() // repo root (AllMyStuff/)
        .map(|root| {
            root.join("gui")
                .join("src-tauri")
                .join("binaries")
                .join(&exe_triple)
        })
    {
        if usable(&dev_slot) {
            return Ok((dev_slot, DaemonSource::DevBuild));
        }
    }

    // 4. Side-by-side MyOwnMesh checkout (release first, then debug).
    for profile in ["release", "debug"] {
        if let Some(p) = sibling_myownmesh_path(profile, exe) {
            if p.exists() {
                return Ok((p, DaemonSource::DevBuild));
            }
        }
    }

    // 5. PATH walk (skip stale, non-existent entries).
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(exe);
            if candidate.exists() {
                return Ok((candidate, DaemonSource::Installed));
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
/// root. CARGO_MANIFEST_DIR here is `node/`, so the repo root is one parent
/// up and the side-by-side checkout one more.
fn sibling_myownmesh_path(profile: &str, exe: &str) -> Option<PathBuf> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    Some(
        PathBuf::from(manifest_dir)
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
        if log_daemon_version(client).await {
            // The running daemon is stale, but it isn't ours to restart
            // (an externally-started daemon, or a macOS orphan from a
            // crashed run). Refresh the binary on disk so the *next*
            // daemon start runs the pinned features.
            if let Ok((bin, DaemonSource::Installed)) = find_daemon_binary() {
                if run_daemon_update(&bin).await {
                    tracing::warn!(
                        "updated myownmesh on disk, but the running daemon keeps the old version until it restarts — quit whatever started it (or reboot) and relaunch the app"
                    );
                }
            }
        }
        return Ok(None);
    }

    let (bin, source) = find_daemon_binary().context("locate myownmesh binary")?;
    if source == DaemonSource::Installed {
        ensure_daemon_current(&bin).await;
    }
    tracing::info!(?bin, "spawning myownmesh daemon");

    let mut cmd = Command::new(&bin);
    cmd.arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    // The daemon is a console-subsystem binary and this GUI is windowless,
    // so without CREATE_NO_WINDOW Windows would give the child its own
    // console window, parked on screen for the app's whole lifetime. The
    // inherited stdio handles are unaffected by the flag.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    // Linux half of the lifetime tie (see `tie_daemon_lifetime`): SIGKILL
    // the daemon the moment this process dies, however it dies.
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt as _;
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
    }
    let child = cmd
        .spawn()
        .with_context(|| format!("spawn {}", bin.display()))?;
    tie_daemon_lifetime(&child);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semverish_parses_tags_and_bare_versions() {
        assert_eq!(parse_semverish("v0.2.4"), Some((0, 2, 4)));
        assert_eq!(parse_semverish("0.2.4"), Some((0, 2, 4)));
        assert_eq!(parse_semverish("0.10.3"), Some((0, 10, 3)));
        assert_eq!(parse_semverish(" v1.0.0 "), Some((1, 0, 0)));
    }

    #[test]
    fn semverish_missing_fields_compare_as_zero() {
        assert_eq!(parse_semverish("0.2"), Some((0, 2, 0)));
        assert_eq!(parse_semverish("1"), Some((1, 0, 0)));
    }

    #[test]
    fn semverish_ignores_prerelease_suffixes() {
        assert_eq!(parse_semverish("0.2.4-rc.1"), Some((0, 2, 4)));
    }

    #[test]
    fn semverish_rejects_non_versions() {
        assert_eq!(parse_semverish(""), None);
        assert_eq!(parse_semverish("main"), None);
        assert_eq!(parse_semverish("x.2.4"), None);
    }

    #[test]
    fn version_output_takes_the_last_token_of_the_first_line() {
        assert_eq!(parse_version_output("myownmesh 0.2.4\n"), Some((0, 2, 4)));
        assert_eq!(
            parse_version_output("myownmesh 0.2.1\nextra noise\n"),
            Some((0, 2, 1))
        );
        assert_eq!(parse_version_output("garbage"), None);
        assert_eq!(parse_version_output(""), None);
    }

    #[test]
    fn tuple_ordering_matches_semver() {
        // The whole fix rides on this comparison: numeric per-field,
        // not lexicographic on the string ("0.10.0" > "0.2.4").
        assert!((0, 2, 1) < (0, 2, 4));
        assert!((0, 10, 0) > (0, 2, 4));
        assert!((1, 0, 0) > (0, 10, 0));
        assert!((0, 2, 4) >= (0, 2, 4));
    }
}
