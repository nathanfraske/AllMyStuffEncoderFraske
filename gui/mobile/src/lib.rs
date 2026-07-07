//! AllMyStuff mobile (iOS / Android) — Tauri shell.
//!
//! The window is the **same** Svelte app the desktop ships (the graph/map,
//! Console, Files, Terminal, Rooms — see `../../gui/src`). The frontend is
//! built to run with or without a backend: every call goes through a
//! `tryInvoke` that falls back to demo data when a command is absent, so the
//! UI is fully navigable from the first build while the native planes land
//! one at a time.
//!
//! What differs from the desktop shell (`../src-tauri/src/main.rs`) is *how*
//! this Rust side answers. The desktop spawns a separate `allmystuff-serve`
//! node and drives it over a control socket. iOS forbids a sandboxed app
//! from spawning child processes, so the phone instead **embeds** its
//! viewer/controller brain in-process: this shell is built on
//! [`allmystuff_mobile_core`], and the live mesh engine drops in behind that
//! crate's `MeshClient` seam (the embedded `myownmesh-core` binding — the
//! next slice; see `docs/MOBILE.md`).
//!
//! Today this shell answers exactly the calls it can answer *honestly* from
//! `mobile-core` — chiefly [`scan_self`], which puts a real phone node on the
//! graph with the real viewer/controller capability set. Calls that need a
//! live mesh (presence, routing, the media planes) are intentionally not
//! registered yet, so the frontend keeps its demo behaviour for them until
//! the engine binding makes them real, rather than this side faking a mesh.

use allmystuff_mobile_core::prelude::*;
use serde_json::{json, Value};

// The real in-process mesh node (LAN discovery, presence → graph). Behind the
// default `mesh` feature so the NDK-free android CI check can build the shell
// with `--no-default-features` (the engine's C deps need the NDK on device).
#[cfg(feature = "mesh")]
mod logging;
#[cfg(feature = "mesh")]
mod mesh;

/// A friendly host-OS string for the node card — "Android", "iOS", or the
/// raw target name on a desktop smoke-test build.
pub(crate) fn os_label() -> &'static str {
    match std::env::consts::OS {
        "android" => "Android",
        "ios" => "iOS",
        other => other,
    }
}

/// Seconds since the Unix epoch — the phone's per-run boot id. A peer that
/// sees a new value knows the phone restarted, the same event-driven gossip
/// the desktop uses.
pub(crate) fn boot_id() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Scan "this machine" — the phone. Mirrors the desktop `scan_self`'s
/// `ScanResult` shape (`{ node_id, label, hostname, summary, capabilities }`)
/// so the store re-homes the local node with no mobile special case.
///
/// `node_id` is the engine's real ed25519 device id once the mesh is up, and
/// `"this"` before then — exactly the placeholder the desktop uses for its
/// offline/demo graph, which the store already understands (it re-homes the
/// local node when the real id arrives). The capabilities are real either way:
/// the viewer/controller set every phone advertises, built by
/// [`mobile_capabilities`] the same way a desktop's come from its hardware scan.
fn scan_self_impl(node_id: String) -> Result<Value, String> {
    let node = NodeId::from(node_id.as_str());
    let cfg = MobileNodeConfig {
        label: "My Phone".to_string(),
        os: os_label().to_string(),
        model: String::new(),
        ram_bytes: 0,
        scope: MobileScope::ViewerController,
    };
    let profile = mobile_profile(&node, &cfg, boot_id(), env!("CARGO_PKG_VERSION"));

    Ok(json!({
        "node_id": profile.node.as_str(),
        "label": profile.label,
        "hostname": "",
        "summary": profile.summary,
        "capabilities": profile.capabilities,
    }))
}

#[cfg(feature = "mesh")]
#[tauri::command]
fn scan_self(state: tauri::State<'_, mesh::MeshState>) -> Result<Value, String> {
    let node_id = state
        .0
        .lock()
        .unwrap()
        .as_ref()
        .map(|m| m.device_id().to_string())
        .unwrap_or_else(|| "this".to_string());
    scan_self_impl(node_id)
}

#[cfg(not(feature = "mesh"))]
#[tauri::command]
fn scan_self() -> Result<Value, String> {
    scan_self_impl("this".to_string())
}

/// Mirror one frontend diagnostic line into the app's log (the Xcode /
/// logcat console *and* the on-phone `allmystuff.log` — see [`logging`]), so
/// a call reads end to end — the mobile counterpart to the desktop's
/// `client_log`.
#[tauri::command]
fn client_log(line: String) {
    #[cfg(feature = "mesh")]
    tracing::info!("[ui] {line}");
    #[cfg(not(feature = "mesh"))]
    eprintln!("[ui] {line}");
}

/// The shared entry point. On iOS/Android the platform shell calls this
/// through the C ABI (`tauri::mobile_entry_point`); on a desktop smoke-test
/// build `main.rs` calls it directly.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default().plugin(tauri_plugin_shell::init());

    // With the mesh engine: manage its state, auto-join the LAN mesh on a
    // background thread (so the UI comes up immediately), and answer the
    // discovery/graph commands from the live node.
    #[cfg(feature = "mesh")]
    let builder = builder
        .manage(mesh::MeshState::default())
        .setup(|app| {
            // Route the embedded engine's `tracing` diagnostics (mDNS attach
            // failures, peer connects/drops) to the device console *and* an
            // on-phone log file (see [`logging`]). Without a subscriber they
            // are dropped — and "why is discovery dark" becomes undebuggable
            // on a phone.
            match logging::init(app.handle()) {
                Some(path) => tracing::info!("logging to {}", path.display()),
                None => tracing::warn!("file log unavailable; stderr only"),
            }
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                tracing::info!("[mesh] opening the embedded engine (LAN mDNS discovery)…");
                match mesh::join(&handle) {
                    Ok(id) => tracing::info!("[mesh] joined the LAN mesh as {id}"),
                    Err(e) => tracing::error!("[mesh] join failed: {e}"),
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            scan_self,
            client_log,
            mesh::session_snapshot,
            mesh::mesh_networks,
            mesh::mesh_peers,
            mesh::mesh_roster_list,
            mesh::mesh_identity,
            mesh::mesh_identity_set_label,
            mesh::mesh_network_add,
            mesh::mesh_network_remove,
            mesh::mesh_network_update,
            mesh::network_reconnect,
            mesh::network_set_enabled,
            mesh::disabled_networks,
            mesh::mesh_network_id_generate,
            mesh::mesh_status,
            mesh::mesh_config_show,
            mesh::refresh_node,
        ]);

    // Without it (the NDK-free CI check): just the demo-capable shell commands.
    #[cfg(not(feature = "mesh"))]
    let builder = builder.invoke_handler(tauri::generate_handler![scan_self, client_log]);

    builder
        .run(tauri::generate_context!())
        .expect("error while running the AllMyStuff mobile app");
}
