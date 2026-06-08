//! AllMyStuff GUI — Tauri shell.
//!
//! The window is a Svelte app; this Rust side does two jobs:
//!
//!  1. **Scans the machine** it runs on (`scan_self`) via
//!     `allmystuff-inventory` + the `allmystuff-bridge`, handing the
//!     front-end the same `{ summary, capabilities }` shape its demo data
//!     uses — so "this device" on the graph becomes real hardware.
//!
//!  2. **Bridges the mesh.** Like the MyOwnMesh GUI and MyOwnLLM, it's a
//!     *client* of a `myownmesh serve` daemon over the local control
//!     socket — it never embeds the engine. One-shot Tauri commands wrap
//!     control requests; a background task pumps the daemon's event stream
//!     out to the renderer as `allmystuff://event`.

#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

mod control_client;
mod daemon_spawn;

use std::sync::Arc;

use control_client::{ControlClient, Request, Response};
use parking_lot::Mutex;
use tauri::{AppHandle, Emitter, Manager, RunEvent, State};
use tokio::sync::mpsc;

struct AppState {
    client: Arc<ControlClient>,
    daemon_child: Mutex<Option<daemon_spawn::DaemonChild>>,
    last_subscription_status: Mutex<serde_json::Value>,
}

fn update_subscription_status(handle: &AppHandle, value: serde_json::Value) {
    let state = handle.state::<AppState>();
    *state.last_subscription_status.lock() = value.clone();
    let _ = handle.emit("allmystuff://subscription", value);
}

fn unwrap_response(resp: Response) -> Result<serde_json::Value, String> {
    if !resp.ok {
        return Err(resp.error.unwrap_or_else(|| "(no error message)".into()));
    }
    Ok(resp.data.unwrap_or(serde_json::Value::Null))
}

// ---- this machine -----------------------------------------------------

/// Scan this machine and return `{ summary, capabilities }` — the shape the
/// Svelte store hydrates the local node from. Pure local work; needs no
/// daemon.
#[tauri::command]
fn scan_self() -> Result<serde_json::Value, String> {
    let inv = allmystuff_inventory::scan();
    let me = allmystuff_graph::NodeId::this();
    let capabilities = allmystuff_bridge::capabilities_from_inventory(&inv, &me);
    let summary = allmystuff_bridge::node_summary(&inv);
    serde_json::to_value(serde_json::json!({
        "summary": summary,
        "capabilities": capabilities,
    }))
    .map_err(|e| e.to_string())
}

/// The full raw inventory, for a "details" view / bug report.
#[tauri::command]
fn scan_full() -> Result<serde_json::Value, String> {
    serde_json::to_value(allmystuff_inventory::scan()).map_err(|e| e.to_string())
}

// ---- mesh: one-shot control commands ----------------------------------

#[tauri::command]
async fn mesh_status(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    unwrap_response(state.client.request(&Request::Status).await.map_err(|e| e.to_string())?)
}

#[tauri::command]
async fn mesh_identity(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    unwrap_response(state.client.request(&Request::IdentityShow).await.map_err(|e| e.to_string())?)
}

#[tauri::command]
async fn mesh_networks(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    unwrap_response(state.client.request(&Request::NetworksList).await.map_err(|e| e.to_string())?)
}

#[tauri::command]
async fn mesh_peers(state: State<'_, AppState>, network: String) -> Result<serde_json::Value, String> {
    unwrap_response(
        state.client.request(&Request::PeersList { network }).await.map_err(|e| e.to_string())?,
    )
}

#[tauri::command]
async fn mesh_network_add(
    state: State<'_, AppState>,
    config: serde_json::Value,
) -> Result<serde_json::Value, String> {
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
) -> Result<serde_json::Value, String> {
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
) -> Result<serde_json::Value, String> {
    unwrap_response(
        state
            .client
            .request(&Request::RosterRemove { network, device_id })
            .await
            .map_err(|e| e.to_string())?,
    )
}

/// Advertise this node's capability matrix on a network. AllMyStuff packs
/// its presence advert into the daemon's capability slot so peers discover
/// what's on offer.
#[tauri::command]
async fn mesh_capabilities_set(
    state: State<'_, AppState>,
    network: String,
    capabilities: serde_json::Value,
) -> Result<serde_json::Value, String> {
    unwrap_response(
        state
            .client
            .request(&Request::CapabilitiesSet { network, capabilities })
            .await
            .map_err(|e| e.to_string())?,
    )
}

/// Broadcast an AllMyStuff app message (presence / route / share) on a
/// typed channel to every peer.
#[tauri::command]
async fn mesh_channel_send_all(
    state: State<'_, AppState>,
    network: String,
    channel: String,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    unwrap_response(
        state
            .client
            .request(&Request::ChannelSendAll { network, channel, payload })
            .await
            .map_err(|e| e.to_string())?,
    )
}

/// Send an app message point-to-point to one peer.
#[tauri::command]
async fn mesh_channel_send_to(
    state: State<'_, AppState>,
    network: String,
    channel: String,
    peer: String,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    unwrap_response(
        state
            .client
            .request(&Request::ChannelSendTo { network, channel, peer, payload })
            .await
            .map_err(|e| e.to_string())?,
    )
}

#[tauri::command]
fn mesh_subscription_state(state: State<'_, AppState>) -> serde_json::Value {
    state.last_subscription_status.lock().clone()
}

// ---- self-update (pass-through to the daemon's updater) ---------------

#[tauri::command]
async fn update_status(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    unwrap_response(state.client.request(&Request::UpdateStatus).await.map_err(|e| e.to_string())?)
}

#[tauri::command]
async fn update_check(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    unwrap_response(state.client.request(&Request::UpdateCheck).await.map_err(|e| e.to_string())?)
}

#[tauri::command]
async fn update_apply(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    unwrap_response(state.client.request(&Request::UpdateApply).await.map_err(|e| e.to_string())?)
}

#[tauri::command]
async fn update_set_prefs(
    state: State<'_, AppState>,
    prefs: serde_json::Value,
) -> Result<serde_json::Value, String> {
    unwrap_response(
        state.client.request(&Request::UpdateSetPrefs { prefs }).await.map_err(|e| e.to_string())?,
    )
}

/// Background task owning the daemon's event subscription; each line
/// becomes an `allmystuff://event` Tauri event. Re-subscribes on
/// disconnect.
async fn run_event_pump(app: AppHandle, client: Arc<ControlClient>) {
    loop {
        let (tx, mut rx) = mpsc::channel::<serde_json::Value>(256);
        match client.subscribe_events(tx).await {
            Ok(()) => {
                update_subscription_status(&app, serde_json::json!({ "status": "live" }));
                while let Some(value) = rx.recv().await {
                    let _ = app.emit("allmystuff://event", value);
                }
                update_subscription_status(&app, serde_json::json!({ "status": "disconnected" }));
            }
            Err(e) => {
                tracing::warn!("event subscribe failed: {e} — will retry");
                update_subscription_status(
                    &app,
                    serde_json::json!({ "status": "disconnected", "error": e.to_string() }),
                );
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

/// Pi / aarch64 Linux WebKitGTK rendering workaround — paint on the CPU so
/// the animated SVG graph doesn't corrupt or wedge the compositor. Kept in
/// sync with MyOwnMesh and MyOwnLLM, which hit the same V3D breakage.
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

    let log_level =
        std::env::var("ALLMYSTUFF_GUI_LOG").unwrap_or_else(|_| "info,allmystuff_gui=info".to_string());
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
            last_subscription_status: Mutex::new(serde_json::json!({ "status": "connecting" })),
        })
        .invoke_handler(tauri::generate_handler![
            scan_self,
            scan_full,
            mesh_status,
            mesh_identity,
            mesh_networks,
            mesh_peers,
            mesh_network_add,
            mesh_roster_approve,
            mesh_roster_remove,
            mesh_capabilities_set,
            mesh_channel_send_all,
            mesh_channel_send_to,
            mesh_subscription_state,
            update_status,
            update_check,
            update_apply,
            update_set_prefs,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            let client = client.clone();
            tauri::async_runtime::spawn(async move {
                match daemon_spawn::ensure_daemon_running(&client).await {
                    Ok(child) => {
                        if let Some(child) = child {
                            let state = handle.state::<AppState>();
                            *state.daemon_child.lock() = Some(child);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("daemon auto-spawn skipped: {e:#}");
                        update_subscription_status(
                            &handle,
                            serde_json::json!({
                                "status": "disconnected",
                                "error": format!("no mesh daemon: {e}"),
                            }),
                        );
                    }
                }
                run_event_pump(handle, client).await;
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
