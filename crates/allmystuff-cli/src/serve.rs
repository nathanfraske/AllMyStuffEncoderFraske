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
//! Discovery (in [`allmystuff_service::find_serve_binary`]) mirrors the GUI
//! launcher: `ALLMYSTUFF_SERVE_BIN` → next to this binary (the release layout)
//! → `$PATH` → the installer's dirs → dev artefacts under
//! `node/target/{debug,release}/`.

use std::process::{Command, ExitCode};

use allmystuff_service::find_serve_binary;

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
