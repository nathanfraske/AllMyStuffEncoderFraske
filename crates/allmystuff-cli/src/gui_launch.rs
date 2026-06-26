//! Bare `allmystuff` with no subcommand → open the desktop app.
//!
//! AllMyStuff ships two binaries — the `allmystuff` CLI (this one) and the
//! `allmystuff-gui` Tauri app — so a bare invocation locates the GUI and
//! hands off to it, exactly like a bare `myownmesh` opens `myownmesh-gui`.
//! The GUI in turn auto-spawns the `myownmesh serve` daemon it needs.
//!
//! Discovery is [`allmystuff_service::find_gui_binary`] — the same lookup
//! `amst` uses to open the app when no node is running, so both find it the
//! same way (`ALLMYSTUFF_GUI_BIN` → next to this binary → `$PATH` → the
//! installer's dirs → dev artefacts under `gui/src-tauri/target/`).

use std::process::{Command, ExitCode};

use allmystuff_service::find_gui_binary;

pub fn launch() -> ExitCode {
    // A webview can't attach to a headless box; bail with a pointer at the
    // headless-friendly entry points instead of silently doing nothing.
    #[cfg(target_os = "linux")]
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("allmystuff: no DISPLAY or WAYLAND_DISPLAY — can't open the desktop app.");
        eprintln!();
        eprintln!("On a headless box, use the CLI directly:");
        eprintln!("  allmystuff serve           # run this machine on the mesh (no GUI)");
        eprintln!("  allmystuff service install # …and keep it running across reboots");
        eprintln!("  allmystuff scan            # inventory this machine");
        eprintln!("  allmystuff capabilities    # what it would expose on the mesh");
        eprintln!("  allmystuff update          # update to the latest release");
        return ExitCode::FAILURE;
    }

    let gui = match find_gui_binary() {
        Some(p) => p,
        None => {
            eprintln!("allmystuff: couldn't find the `allmystuff-gui` desktop app.");
            eprintln!();
            eprintln!("Re-run the installer (it fetches the app by default), install an OS");
            eprintln!("bundle from Releases, point ALLMYSTUFF_GUI_BIN at the binary, or");
            eprintln!("run the CLI directly (`allmystuff scan`). From a source checkout,");
            eprintln!("`just dev` runs the app.");
            return ExitCode::FAILURE;
        }
    };

    match Command::new(&gui).status() {
        Ok(status) => match status.code() {
            Some(0) | None => ExitCode::SUCCESS,
            Some(_) => ExitCode::FAILURE,
        },
        Err(e) => {
            eprintln!("allmystuff: failed to launch {}: {e}", gui.display());
            ExitCode::FAILURE
        }
    }
}
