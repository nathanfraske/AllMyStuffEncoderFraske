//! `allmystuff serve` — run this machine as a headless mesh node.
//!
//! Like a bare `allmystuff` hands off to the desktop app, `serve` hands off
//! to the **`allmystuff-serve`** binary, which carries the whole node engine
//! (every media plane + the route handshake) and is too heavy to live in
//! this lightweight CLI. That binary supervises a `myownmesh serve` daemon
//! itself, so one `allmystuff serve` brings up the entire node on a headless
//! box — and one `allmystuff service` (see [`crate::service`]) keeps it
//! running across reboots.
//!
//! Discovery order mirrors the GUI launcher: `ALLMYSTUFF_SERVE_BIN` → next to
//! this binary (the release layout) → `$PATH` → dev artefacts under
//! `node/target/{debug,release}/`.

use std::path::PathBuf;
use std::process::{Command, ExitCode};

/// Run the node. Forwards any extra args through to `allmystuff-serve` and,
/// on Unix, *replaces* this process with it so a service manager tracks the
/// right PID and signals reach the node directly.
pub fn run(args: &[String]) -> ExitCode {
    let serve = match find_serve_binary() {
        Some(p) => p,
        None => {
            eprintln!("allmystuff: couldn't find the `allmystuff-serve` node binary.");
            eprintln!();
            eprintln!("Re-run the installer (it installs the node), point ALLMYSTUFF_SERVE_BIN");
            eprintln!("at the binary, or from a source checkout build it:");
            eprintln!("  cargo build --release --manifest-path node/Cargo.toml");
            return ExitCode::FAILURE;
        }
    };
    hand_off(&serve, args)
}

/// On Unix, exec replaces this image: the node keeps this PID, so systemd /
/// launchd supervise it directly and SIGTERM reaches it. `exec` only returns
/// on failure.
#[cfg(unix)]
fn hand_off(serve: &std::path::Path, args: &[String]) -> ExitCode {
    use std::os::unix::process::CommandExt as _;
    let err = Command::new(serve).args(args).exec();
    eprintln!("allmystuff: failed to exec {}: {err}", serve.display());
    ExitCode::FAILURE
}

/// Elsewhere, spawn the node and mirror its exit code.
#[cfg(not(unix))]
fn hand_off(serve: &std::path::Path, args: &[String]) -> ExitCode {
    match Command::new(serve).args(args).status() {
        Ok(status) => match status.code() {
            Some(0) | None => ExitCode::SUCCESS,
            Some(_) => ExitCode::FAILURE,
        },
        Err(e) => {
            eprintln!("allmystuff: failed to launch {}: {e}", serve.display());
            ExitCode::FAILURE
        }
    }
}

fn serve_exe_name() -> &'static str {
    if cfg!(windows) {
        "allmystuff-serve.exe"
    } else {
        "allmystuff-serve"
    }
}

/// Locate the `allmystuff-serve` binary. Shared by `serve` (which execs it)
/// and `service install` (which bakes its path into the unit's `ExecStart`).
pub fn find_serve_binary() -> Option<PathBuf> {
    let exe = serve_exe_name();

    if let Some(p) = std::env::var_os("ALLMYSTUFF_SERVE_BIN") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(current) = std::env::current_exe() {
        if let Some(candidate) = current.parent().map(|dir| dir.join(exe)) {
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(exe);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    for profile in ["release", "debug"] {
        if let Some(p) = workspace_serve_path(profile, exe) {
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

fn workspace_serve_path(profile: &str, exe: &str) -> Option<PathBuf> {
    // CARGO_MANIFEST_DIR = crates/allmystuff-cli; repo root is two up, and the
    // node engine's build output lives under `node/target/<profile>/`.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    Some(
        PathBuf::from(manifest_dir)
            .parent()? // crates/
            .parent()? // repo root
            .join("node")
            .join("target")
            .join(profile)
            .join(exe),
    )
}
