//! AllMyStuff GUI — Tauri shell.
//!
//! The window is a Svelte app; this Rust side:
//!
//!  1. **Scans the machine** (`scan_self`) so "this device" on the graph is
//!     real hardware.
//!  2. **Runs the live mesh** ([`mesh::Mesh`]) — a client of a `myownmesh
//!     serve` daemon over the control socket. Presence makes peers appear;
//!     the route handshake + `cpal` audio bridge make a connection actually
//!     stream. The front-end drives it via `connect_route` /
//!     `disconnect_route` and reads `allmystuff://session` snapshots.
//!  3. **Self-updates** via `allmystuff-updater` (its own release feed —
//!     not the daemon's).

#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

mod audio;
mod control_client;
mod daemon_spawn;
mod mesh;

use std::sync::Arc;

use control_client::{ControlClient, Request, Response};
use mesh::Mesh;
use parking_lot::Mutex;
use serde_json::{json, Value};
use tauri::{Manager, RunEvent, State};

struct AppState {
    client: Arc<ControlClient>,
    daemon_child: Mutex<Option<daemon_spawn::DaemonChild>>,
}

fn unwrap_response(resp: Response) -> Result<Value, String> {
    if !resp.ok {
        return Err(resp.error.unwrap_or_else(|| "(no error message)".into()));
    }
    Ok(resp.data.unwrap_or(Value::Null))
}

// ---- this machine -----------------------------------------------------

/// Scan this machine: `{ node_id, label, summary, capabilities }`. `node_id`
/// is the mesh device id once the session is up (so capabilities match what
/// peers see), else `"this"` for the offline/demo graph; `label` is the
/// hostname shown on the local node.
#[tauri::command]
fn scan_self(mesh: State<'_, Arc<Mesh>>) -> Result<Value, String> {
    let me = mesh.local_node_id().unwrap_or_else(|| "this".to_string());
    let node = allmystuff_graph::NodeId::from(me.as_str());
    let inv = allmystuff_inventory::scan();
    serde_json::to_value(json!({
        "node_id": me,
        "label": inv.host.hostname,
        "summary": allmystuff_bridge::node_summary(&inv),
        "capabilities": allmystuff_bridge::capabilities_from_inventory(&inv, &node),
    }))
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn scan_full() -> Result<Value, String> {
    serde_json::to_value(allmystuff_inventory::scan()).map_err(|e| e.to_string())
}

// ---- live mesh (presence + routing + audio) ---------------------------

/// Offer a connection from one capability to another. Returns the route id.
#[tauri::command]
async fn connect_route(
    mesh: State<'_, Arc<Mesh>>,
    from: String,
    to: String,
    media: String,
) -> Result<String, String> {
    mesh.inner().connect(from, to, media).await
}

#[tauri::command]
async fn disconnect_route(mesh: State<'_, Arc<Mesh>>, route_id: String) -> Result<(), String> {
    mesh.inner().disconnect(route_id).await
}

/// Current peers + live route states.
#[tauri::command]
fn session_snapshot(mesh: State<'_, Arc<Mesh>>) -> Value {
    mesh.snapshot()
}

// ---- mesh control passthroughs ----------------------------------------

#[tauri::command]
async fn mesh_status(state: State<'_, AppState>) -> Result<Value, String> {
    unwrap_response(state.client.request(&Request::Status).await.map_err(|e| e.to_string())?)
}

#[tauri::command]
async fn mesh_identity(state: State<'_, AppState>) -> Result<Value, String> {
    unwrap_response(state.client.request(&Request::IdentityShow).await.map_err(|e| e.to_string())?)
}

#[tauri::command]
async fn mesh_networks(state: State<'_, AppState>) -> Result<Value, String> {
    unwrap_response(state.client.request(&Request::NetworksList).await.map_err(|e| e.to_string())?)
}

#[tauri::command]
async fn mesh_peers(state: State<'_, AppState>, network: String) -> Result<Value, String> {
    unwrap_response(
        state.client.request(&Request::PeersList { network }).await.map_err(|e| e.to_string())?,
    )
}

#[tauri::command]
async fn mesh_network_add(state: State<'_, AppState>, config: Value) -> Result<Value, String> {
    unwrap_response(
        state.client.request(&Request::NetworkAdd { config }).await.map_err(|e| e.to_string())?,
    )
}

#[tauri::command]
async fn mesh_roster_approve(
    state: State<'_, AppState>,
    network: String,
    device_id: String,
    label: Option<String>,
) -> Result<Value, String> {
    unwrap_response(
        state
            .client
            .request(&Request::RosterApprove { network, device_id, label })
            .await
            .map_err(|e| e.to_string())?,
    )
}

#[tauri::command]
async fn mesh_roster_remove(
    state: State<'_, AppState>,
    network: String,
    device_id: String,
) -> Result<Value, String> {
    unwrap_response(
        state
            .client
            .request(&Request::RosterRemove { network, device_id })
            .await
            .map_err(|e| e.to_string())?,
    )
}

// ---- self-update (AllMyStuff's own updater, not the daemon's) ----------

#[tauri::command]
async fn update_status() -> Result<Value, String> {
    serde_json::to_value(allmystuff_updater::status().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn update_check() -> Result<Value, String> {
    let outcome = allmystuff_updater::check_now(true).await.map_err(|e| e.to_string())?;
    serde_json::to_value(outcome).map_err(|e| e.to_string())
}

#[tauri::command]
async fn update_apply() -> Result<Value, String> {
    let applied = allmystuff_updater::apply_now().map_err(|e| e.to_string())?;
    Ok(json!({ "applied": applied }))
}

#[tauri::command]
async fn update_set_prefs(prefs: Value) -> Result<Value, String> {
    let prefs: allmystuff_updater::UpdatePrefs =
        serde_json::from_value(prefs).map_err(|e| e.to_string())?;
    serde_json::to_value(allmystuff_updater::set_prefs(prefs).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}

/// Pi / aarch64 Linux WebKitGTK rendering workaround — paint on the CPU so
/// the animated SVG graph doesn't corrupt or wedge the compositor. Kept in
/// sync with MyOwnMesh and MyOwnLLM.
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
fn workaround_pi_webkit_rendering() {
    for (key, value) in [
        ("WEBKIT_DISABLE_COMPOSITING_MODE", "1"),
        ("WEBKIT_DISABLE_DMABUF_RENDERER", "1"),
    ] {
        if std::env::var_os(key).is_none() {
            std::env::set_var(key, value);
        }
    }
}

fn main() {
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    workaround_pi_webkit_rendering();

    // Apply any update staged on the previous run before anything else.
    allmystuff_updater::apply_pending_if_any();

    let log_level = std::env::var("ALLMYSTUFF_GUI_LOG")
        .unwrap_or_else(|_| "info,allmystuff_gui=info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(log_level))
        .with_target(false)
        .init();

    let client = Arc::new(ControlClient::new().expect("resolve control socket path"));

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            client: client.clone(),
            daemon_child: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            scan_self,
            scan_full,
            connect_route,
            disconnect_route,
            session_snapshot,
            mesh_status,
            mesh_identity,
            mesh_networks,
            mesh_peers,
            mesh_network_add,
            mesh_roster_approve,
            mesh_roster_remove,
            update_status,
            update_check,
            update_apply,
            update_set_prefs,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            let mesh = Mesh::new(client.clone(), handle.clone());
            app.manage(mesh.clone());
            let client = client.clone();
            tauri::async_runtime::spawn(async move {
                match daemon_spawn::ensure_daemon_running(&client).await {
                    Ok(child) => {
                        if let Some(child) = child {
                            handle.state::<AppState>().daemon_child.lock().replace(child);
                        }
                    }
                    Err(e) => tracing::warn!("daemon auto-spawn skipped: {e:#}"),
                }
                // Bring the live session online (subscribes, advertises
                // presence, starts the event pump).
                mesh.start().await;
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building the AllMyStuff GUI")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                let state = app.state::<AppState>();
                let child = state.daemon_child.lock().take();
                if let Some(c) = child {
                    drop(c);
                }
            }
        });
}
