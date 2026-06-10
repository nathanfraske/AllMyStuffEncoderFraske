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

#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

mod audio;
mod control_client;
mod daemon_spawn;
mod input_inject;
mod mesh;
mod ownership;
mod video;

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
async fn scan_self(mesh: State<'_, Arc<Mesh>>) -> Result<Value, String> {
    let me = mesh
        .resolve_local_id()
        .await
        .unwrap_or_else(|| "this".to_string());
    let node = allmystuff_graph::NodeId::from(me.as_str());
    let inv = allmystuff_inventory::scan();
    serde_json::to_value(json!({
        "node_id": me,
        "label": inv.host.hostname,
        "hostname": inv.host.hostname,
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

/// Claim a device as one of yours. Only takes if the target is in claim
/// mode; the target's next presence advert (owner = us) confirms it.
#[tauri::command]
async fn claim_node(mesh: State<'_, Arc<Mesh>>, node: String) -> Result<(), String> {
    mesh.inner().claim(node).await
}

/// Put this device into / out of claim mode so another of your machines can
/// adopt it. Returns whether it's now claimable.
#[tauri::command]
async fn set_claimable(mesh: State<'_, Arc<Mesh>>, claimable: bool) -> Result<bool, String> {
    mesh.inner().set_claimable(claimable).await
}

/// Forward one keyboard/mouse event down an active outbound input route —
/// the console window's control stream.
#[tauri::command]
async fn send_input(
    mesh: State<'_, Arc<Mesh>>,
    route_id: String,
    action: serde_json::Value,
) -> Result<(), String> {
    let action: allmystuff_session::InputAction =
        serde_json::from_value(action).map_err(|e| e.to_string())?;
    mesh.inner().send_input(route_id, action).await
}

/// Stream one route's inbound display frames into the calling window over
/// an IPC channel, as raw bytes (a fixed header + the JPEG) — no JSON or
/// base64 on the per-frame path, and only the window that's actually
/// watching pays for delivery. Replaces any previous watcher of the route.
#[tauri::command]
fn video_watch(
    mesh: State<'_, Arc<Mesh>>,
    route_id: String,
    on_frame: tauri::ipc::Channel<tauri::ipc::InvokeResponseBody>,
) {
    mesh.video_watch(route_id, on_frame);
}

/// Stop streaming a route's frames to the front-end (console closed or
/// switched input). Idempotent.
#[tauri::command]
fn video_unwatch(mesh: State<'_, Arc<Mesh>>, route_id: String) {
    mesh.video_unwatch(&route_id);
}

/// Open (or focus) a dedicated console window for `node` — its own OS
/// window, so several remote consoles can be on screen at once. The window
/// loads the same app with `?console=<node>`, which renders just the
/// console for that machine.
#[tauri::command]
async fn open_console_window(app: tauri::AppHandle, node: String) -> Result<(), String> {
    let label = format!("console-{}", window_slug(&node));
    if let Some(existing) = app.get_webview_window(&label) {
        let _ = existing.set_focus();
        return Ok(());
    }
    tauri::WebviewWindowBuilder::new(
        &app,
        &label,
        tauri::WebviewUrl::App(format!("index.html?console={node}").into()),
    )
    .title("AllMyStuff console")
    .inner_size(1100.0, 740.0)
    .min_inner_size(560.0, 380.0)
    .build()
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// A node id reduced to the characters Tauri allows in a window label —
/// one stable label per machine, so re-opening focuses instead of stacking.
fn window_slug(node: &str) -> String {
    node.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Current peers + live route states.
#[tauri::command]
fn session_snapshot(mesh: State<'_, Arc<Mesh>>) -> Value {
    mesh.snapshot()
}

/// The owned-fleet roster: the shared key and the devices this owner has
/// claimed (and that have converged via gossip). Drives the Fleet settings
/// view; updated live by the `allmystuff://owned` event.
#[tauri::command]
fn owned_roster(mesh: State<'_, Arc<Mesh>>) -> Value {
    mesh.owned_roster_value()
}

/// Leave the fleet this device belongs to (and release its owner) — the
/// remaining members converge on the bumped roster without us.
#[tauri::command]
async fn fleet_leave(mesh: State<'_, Arc<Mesh>>) -> Result<(), String> {
    mesh.inner().fleet_leave().await
}

/// Kick a device out of the fleet. Only a member may kick (the backend
/// enforces it), and never itself — that's `fleet_leave`.
#[tauri::command]
async fn fleet_kick(mesh: State<'_, Arc<Mesh>>, device: String) -> Result<(), String> {
    mesh.inner().fleet_kick(device).await
}

// ---- mesh control passthroughs ----------------------------------------

#[tauri::command]
async fn mesh_status(state: State<'_, AppState>) -> Result<Value, String> {
    unwrap_response(
        state
            .client
            .request(&Request::Status)
            .await
            .map_err(|e| e.to_string())?,
    )
}

#[tauri::command]
async fn mesh_identity(state: State<'_, AppState>) -> Result<Value, String> {
    unwrap_response(
        state
            .client
            .request(&Request::IdentityShow)
            .await
            .map_err(|e| e.to_string())?,
    )
}

#[tauri::command]
async fn mesh_networks(state: State<'_, AppState>) -> Result<Value, String> {
    unwrap_response(
        state
            .client
            .request(&Request::NetworksList)
            .await
            .map_err(|e| e.to_string())?,
    )
}

#[tauri::command]
async fn mesh_peers(state: State<'_, AppState>, network: String) -> Result<Value, String> {
    unwrap_response(
        state
            .client
            .request(&Request::PeersList { network })
            .await
            .map_err(|e| e.to_string())?,
    )
}

#[tauri::command]
async fn mesh_network_add(
    state: State<'_, AppState>,
    mesh: State<'_, Arc<Mesh>>,
    config: Value,
) -> Result<Value, String> {
    let data = unwrap_response(
        state
            .client
            .request(&Request::NetworkAdd { config })
            .await
            .map_err(|e| e.to_string())?,
    )?;
    // Subscribe + advertise on the freshly-joined network now, not just at
    // next launch — so a network joined mid-session lights up immediately.
    mesh.inner().sync_networks().await;
    Ok(data)
}

/// The whole daemon config — every network with its full signaling / STUN /
/// TURN settings. The Servers settings pane reads this to populate its editor
/// (`NetworksList` only carries summaries).
#[tauri::command]
async fn mesh_config_show(state: State<'_, AppState>) -> Result<Value, String> {
    unwrap_response(
        state
            .client
            .request(&Request::ConfigShow)
            .await
            .map_err(|e| e.to_string())?,
    )
}

/// Replace one network's config (its signaling / STUN / TURN servers, label,
/// etc.). The daemon hot-applies cosmetic changes and restarts the transport
/// for server changes; we re-subscribe afterwards so the session reconnects.
#[tauri::command]
async fn mesh_network_update(
    state: State<'_, AppState>,
    mesh: State<'_, Arc<Mesh>>,
    config: Value,
) -> Result<Value, String> {
    let data = unwrap_response(
        state
            .client
            .request(&Request::NetworkUpdate { config })
            .await
            .map_err(|e| e.to_string())?,
    )?;
    mesh.inner().sync_networks().await;
    Ok(data)
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
            .request(&Request::RosterApprove {
                network,
                device_id,
                label,
            })
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

#[tauri::command]
async fn mesh_roster_list(state: State<'_, AppState>, network: String) -> Result<Value, String> {
    unwrap_response(
        state
            .client
            .request(&Request::RosterList { network })
            .await
            .map_err(|e| e.to_string())?,
    )
}

/// Ask the daemon for a fresh, valid network id (the shareable handle peers
/// join with). Used by the "create network" flow.
#[tauri::command]
async fn mesh_network_id_generate(state: State<'_, AppState>) -> Result<Value, String> {
    unwrap_response(
        state
            .client
            .request(&Request::NetworkIdGenerate)
            .await
            .map_err(|e| e.to_string())?,
    )
}

#[tauri::command]
async fn mesh_network_remove(
    state: State<'_, AppState>,
    mesh: State<'_, Arc<Mesh>>,
    network: String,
) -> Result<Value, String> {
    let data = unwrap_response(
        state
            .client
            .request(&Request::NetworkRemove { network })
            .await
            .map_err(|e| e.to_string())?,
    )?;
    mesh.inner().sync_networks().await;
    Ok(data)
}

/// Set this device's display-name override. Persists in the daemon identity
/// and updates the live presence profile so peers see the new name on the
/// next broadcast. An empty string resets the name to the hostname.
#[tauri::command]
async fn mesh_identity_set_label(
    state: State<'_, AppState>,
    mesh: State<'_, Arc<Mesh>>,
    label: String,
) -> Result<Value, String> {
    let data = unwrap_response(
        state
            .client
            .request(&Request::IdentitySetLabel {
                label: label.clone(),
            })
            .await
            .map_err(|e| e.to_string())?,
    )?;
    mesh.inner().set_label(label).await;
    Ok(data)
}

// ---- self-update (AllMyStuff's own updater, not the daemon's) ----------

#[tauri::command]
async fn update_status() -> Result<Value, String> {
    serde_json::to_value(allmystuff_updater::status().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn update_check() -> Result<Value, String> {
    let outcome = allmystuff_updater::check_now(true)
        .await
        .map_err(|e| e.to_string())?;
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
            claim_node,
            set_claimable,
            send_input,
            video_watch,
            video_unwatch,
            open_console_window,
            session_snapshot,
            owned_roster,
            fleet_leave,
            fleet_kick,
            mesh_status,
            mesh_identity,
            mesh_networks,
            mesh_peers,
            mesh_network_add,
            mesh_network_remove,
            mesh_network_update,
            mesh_config_show,
            mesh_network_id_generate,
            mesh_roster_approve,
            mesh_roster_remove,
            mesh_roster_list,
            mesh_identity_set_label,
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
                            handle
                                .state::<AppState>()
                                .daemon_child
                                .lock()
                                .replace(child);
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
