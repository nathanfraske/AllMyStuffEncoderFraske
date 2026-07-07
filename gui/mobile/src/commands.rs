//! The desktop GUI's command surface, dispatched in-process.
//!
//! Same command names, same argument names, same JSON shapes as
//! `../../src-tauri/src/main.rs` — the shared Svelte front-end invokes these
//! without ever knowing which platform answered. What differs is the far side
//! of the call: the desktop forwards each command over the per-machine node's
//! control socket, while the phone has no separate node process to talk to
//! (iOS forbids spawning one), so every command here hands its request
//! straight to the in-process engine's `node_control::dispatch` — the same
//! match the desktop reaches over its socket, minus the socket. See
//! [`crate::engine`].
//!
//! Only the node-backed commands are mirrored. The desktop's GUI-side
//! commands (secondary windows, the self-updater, the Always-On service,
//! tray behaviour) stay desktop-only — the phone has no windows to open and
//! the app store owns updates.

use serde_json::{json, Value};
use tauri::Manager;

/// The engine behind an `AppHandle`, for the commands whose desktop
/// signatures are infallible (they answer a default, never an error). `None`
/// until [`crate::engine::boot`] finishes — the same "node not ready" window
/// the fallible commands surface as an `Err`.
fn engine_of(app: &tauri::AppHandle) -> Option<std::sync::Arc<crate::engine::Engine>> {
    app.state::<crate::engine::EngineState>().engine().ok()
}

// ---- live mesh (presence + routing) ------------------------------------

#[tauri::command]
pub async fn connect_route(
    state: tauri::State<'_, crate::engine::EngineState>,
    from: String,
    to: String,
    media: String,
    video: Option<Vec<String>>,
    session: Option<String>,
) -> Result<String, String> {
    let v = state
        .engine()?
        .request(
            "connect_route",
            json!({ "from": from, "to": to, "media": media, "video": video, "session": session }),
        )
        .await?;
    serde_json::from_value(v).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn disconnect_route(
    state: tauri::State<'_, crate::engine::EngineState>,
    route_id: String,
) -> Result<(), String> {
    state
        .engine()?
        .request("disconnect_route", json!({ "route_id": route_id }))
        .await?;
    Ok(())
}

// ---- claims + fleet device control --------------------------------------

#[tauri::command]
pub async fn claim_node(
    state: tauri::State<'_, crate::engine::EngineState>,
    node: String,
) -> Result<(), String> {
    state
        .engine()?
        .request("claim_node", json!({ "node": node }))
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn upgrade_node(
    state: tauri::State<'_, crate::engine::EngineState>,
    node: String,
) -> Result<(), String> {
    state
        .engine()?
        .request("upgrade_node", json!({ "node": node }))
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn restart_node(
    state: tauri::State<'_, crate::engine::EngineState>,
    node: String,
) -> Result<(), String> {
    state
        .engine()?
        .request("restart_node", json!({ "node": node }))
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn restart_device(
    state: tauri::State<'_, crate::engine::EngineState>,
    node: String,
) -> Result<(), String> {
    state
        .engine()?
        .request("restart_device", json!({ "node": node }))
        .await?;
    Ok(())
}

/// `node` omitted = refresh **this** device, a peer id asks that node —
/// mirrored from the desktop, including the omitted-vs-null distinction in
/// the args it sends.
#[tauri::command]
pub async fn refresh_node(
    state: tauri::State<'_, crate::engine::EngineState>,
    node: Option<String>,
) -> Result<(), String> {
    let arg = match node {
        Some(node) => json!({ "node": node }),
        None => json!({}),
    };
    state.engine()?.request("refresh_node", arg).await?;
    Ok(())
}

#[tauri::command]
pub async fn set_claimable(
    state: tauri::State<'_, crate::engine::EngineState>,
    claimable: bool,
) -> Result<bool, String> {
    let v = state
        .engine()?
        .request("set_claimable", json!({ "claimable": claimable }))
        .await?;
    serde_json::from_value(v).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_public_claims(
    state: tauri::State<'_, crate::engine::EngineState>,
    on: bool,
) -> Result<bool, String> {
    let v = state
        .engine()?
        .request("set_public_claims", json!({ "on": on }))
        .await?;
    serde_json::from_value(v).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn claim_via_code(
    state: tauri::State<'_, crate::engine::EngineState>,
    code: String,
) -> Result<(), String> {
    state
        .engine()?
        .request("claim_via_code", json!({ "code": code }))
        .await?;
    Ok(())
}

// ---- KVM appliances ------------------------------------------------------

#[tauri::command]
pub async fn kvm_attach(
    state: tauri::State<'_, crate::engine::EngineState>,
    node: String,
    target: String,
) -> Result<(), String> {
    state
        .engine()?
        .request("kvm_attach", json!({ "node": node, "target": target }))
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn kvm_detach(
    state: tauri::State<'_, crate::engine::EngineState>,
    node: String,
) -> Result<(), String> {
    state
        .engine()?
        .request("kvm_detach", json!({ "node": node }))
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn kvm_mesh_add(
    state: tauri::State<'_, crate::engine::EngineState>,
    node: String,
    network_id: String,
) -> Result<(), String> {
    state
        .engine()?
        .request(
            "kvm_mesh_add",
            json!({ "node": node, "network_id": network_id }),
        )
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn kvm_mesh_remove(
    state: tauri::State<'_, crate::engine::EngineState>,
    node: String,
    network_id: String,
) -> Result<(), String> {
    state
        .engine()?
        .request(
            "kvm_mesh_remove",
            json!({ "node": node, "network_id": network_id }),
        )
        .await?;
    Ok(())
}

// ---- sharing with people -------------------------------------------------

/// `person` / `grant` are the desktop's `Person` / `Grant` structs riding as
/// raw JSON here — the graph types aren't linked into this shell, and the
/// node's dispatch (de)serialises them itself, so the wire shape is
/// identical either way.
#[tauri::command]
pub async fn share_grant(
    state: tauri::State<'_, crate::engine::EngineState>,
    person: Value,
    node: String,
    grant: Value,
) -> Result<(), String> {
    state
        .engine()?
        .request(
            "share_grant",
            json!({ "person": person, "node": node, "grant": grant }),
        )
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn share_revoke(
    state: tauri::State<'_, crate::engine::EngineState>,
    person: String,
    grant_id: String,
) -> Result<(), String> {
    state
        .engine()?
        .request(
            "share_revoke",
            json!({ "person": person, "grant_id": grant_id }),
        )
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn share_stop(
    state: tauri::State<'_, crate::engine::EngineState>,
    person: String,
) -> Result<(), String> {
    state
        .engine()?
        .request("share_stop", json!({ "person": person }))
        .await?;
    Ok(())
}

// ---- console input + clipboard -------------------------------------------

#[tauri::command]
pub async fn send_input(
    state: tauri::State<'_, crate::engine::EngineState>,
    route_id: String,
    action: Value,
) -> Result<(), String> {
    state
        .engine()?
        .request(
            "send_input",
            json!({ "route_id": route_id, "action": action }),
        )
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn clipboard_paste(
    state: tauri::State<'_, crate::engine::EngineState>,
    route_id: String,
) -> Result<(), String> {
    state
        .engine()?
        .request("clipboard_paste", json!({ "route_id": route_id }))
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn clipboard_pull(
    state: tauri::State<'_, crate::engine::EngineState>,
    route_id: String,
) -> Result<(), String> {
    state
        .engine()?
        .request("clipboard_pull", json!({ "route_id": route_id }))
        .await?;
    Ok(())
}

// ---- video (watch / poll / tune) ------------------------------------------

#[tauri::command]
pub async fn video_watch(app: tauri::AppHandle, route_id: String, decode: Option<bool>) -> u64 {
    let Some(engine) = engine_of(&app) else {
        return 0;
    };
    match engine
        .request(
            "video_watch",
            json!({ "route_id": route_id, "decode": decode }),
        )
        .await
    {
        Ok(v) => serde_json::from_value(v).unwrap_or_default(),
        Err(_) => 0,
    }
}

#[tauri::command]
pub async fn video_poll(app: tauri::AppHandle, route_id: String) -> tauri::ipc::Response {
    let bytes = match engine_of(&app) {
        Some(engine) => engine
            .request_bytes("video_poll", json!({ "route_id": route_id }))
            .await
            .unwrap_or_default(),
        None => Vec::new(),
    };
    tauri::ipc::Response::new(bytes)
}

#[tauri::command]
pub async fn video_unwatch(app: tauri::AppHandle, route_id: String, token: u64) {
    if let Some(engine) = engine_of(&app) {
        let _ = engine
            .request(
                "video_unwatch",
                json!({ "route_id": route_id, "token": token }),
            )
            .await;
    }
}

#[tauri::command]
pub async fn video_refresh(
    state: tauri::State<'_, crate::engine::EngineState>,
    route_id: String,
) -> Result<(), String> {
    state
        .engine()?
        .request("video_refresh", json!({ "route_id": route_id }))
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn video_feedback(
    state: tauri::State<'_, crate::engine::EngineState>,
    route_id: String,
    recv_fps: u32,
    decode_fails: u32,
    queue_depth: u32,
) -> Result<(), String> {
    state
        .engine()?
        .request(
            "video_feedback",
            json!({
                "route_id": route_id,
                "recv_fps": recv_fps,
                "decode_fails": decode_fails,
                "queue_depth": queue_depth,
            }),
        )
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn tune_route(
    state: tauri::State<'_, crate::engine::EngineState>,
    route_id: String,
    max_edge: Option<u32>,
    bitrate: Option<u32>,
    fps: Option<u32>,
) -> Result<(), String> {
    state
        .engine()?
        .request(
            "tune_route",
            json!({ "route_id": route_id, "max_edge": max_edge, "bitrate": bitrate, "fps": fps }),
        )
        .await?;
    Ok(())
}

// ---- terminal (the mesh-native shell) --------------------------------------

#[tauri::command]
pub async fn term_send(
    state: tauri::State<'_, crate::engine::EngineState>,
    route_id: String,
    event: Value,
) -> Result<(), String> {
    state
        .engine()?
        .request("term_send", json!({ "route_id": route_id, "event": event }))
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn term_watch(app: tauri::AppHandle, route_id: String) -> u64 {
    let Some(engine) = engine_of(&app) else {
        return 0;
    };
    match engine
        .request("term_watch", json!({ "route_id": route_id }))
        .await
    {
        Ok(v) => serde_json::from_value(v).unwrap_or_default(),
        Err(_) => 0,
    }
}

#[tauri::command]
pub async fn term_poll(app: tauri::AppHandle, route_id: String) -> tauri::ipc::Response {
    let bytes = match engine_of(&app) {
        Some(engine) => engine
            .request_bytes("term_poll", json!({ "route_id": route_id }))
            .await
            .unwrap_or_default(),
        None => Vec::new(),
    };
    tauri::ipc::Response::new(bytes)
}

#[tauri::command]
pub async fn term_unwatch(app: tauri::AppHandle, route_id: String, token: u64) {
    if let Some(engine) = engine_of(&app) {
        let _ = engine
            .request(
                "term_unwatch",
                json!({ "route_id": route_id, "token": token }),
            )
            .await;
    }
}

/// The desktop deserialises the reply into typed
/// `allmystuff_protocol::TerminalSessionInfo`s; here the JSON (a list, or
/// `null` while a remote host answers asynchronously) passes straight
/// through — same wire shape, no protocol crate linked in.
#[tauri::command]
pub async fn terminal_sessions(
    state: tauri::State<'_, crate::engine::EngineState>,
    node: String,
) -> Result<Value, String> {
    state
        .engine()?
        .request("terminal_sessions", json!({ "node": node }))
        .await
}

// ---- files (the mesh-native file manager) -----------------------------------

#[tauri::command]
pub async fn file_send(
    state: tauri::State<'_, crate::engine::EngineState>,
    route_id: String,
    event: Value,
) -> Result<(), String> {
    state
        .engine()?
        .request("file_send", json!({ "route_id": route_id, "event": event }))
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn file_watch(app: tauri::AppHandle, route_id: String) -> u64 {
    let Some(engine) = engine_of(&app) else {
        return 0;
    };
    match engine
        .request("file_watch", json!({ "route_id": route_id }))
        .await
    {
        Ok(v) => serde_json::from_value(v).unwrap_or_default(),
        Err(_) => 0,
    }
}

#[tauri::command]
pub async fn file_poll(app: tauri::AppHandle, route_id: String) -> tauri::ipc::Response {
    let bytes = match engine_of(&app) {
        Some(engine) => engine
            .request_bytes("file_poll", json!({ "route_id": route_id }))
            .await
            .unwrap_or_default(),
        None => Vec::new(),
    };
    tauri::ipc::Response::new(bytes)
}

#[tauri::command]
pub async fn file_unwatch(app: tauri::AppHandle, route_id: String, token: u64) {
    if let Some(engine) = engine_of(&app) {
        let _ = engine
            .request(
                "file_unwatch",
                json!({ "route_id": route_id, "token": token }),
            )
            .await;
    }
}

#[tauri::command]
pub async fn file_download(
    state: tauri::State<'_, crate::engine::EngineState>,
    route_id: String,
    req: u64,
    name: String,
) -> Result<String, String> {
    let v = state
        .engine()?
        .request(
            "file_download",
            json!({ "route_id": route_id, "req": req, "name": name }),
        )
        .await?;
    serde_json::from_value(v).map_err(|e| e.to_string())
}

// ---- sites (the reverse proxy) ----------------------------------------------

/// The desktop types this as `Vec<allmystuff_inventory::ListeningService>`;
/// the JSON passes straight through here — same wire shape, no inventory
/// crate linked in.
#[tauri::command]
pub async fn site_scan(
    state: tauri::State<'_, crate::engine::EngineState>,
) -> Result<Value, String> {
    state.engine()?.request("site_scan", json!({})).await
}

#[tauri::command]
pub async fn site_exposed(app: tauri::AppHandle) -> std::collections::BTreeMap<String, String> {
    let Some(engine) = engine_of(&app) else {
        return Default::default();
    };
    match engine.request("site_exposed", json!({})).await {
        Ok(v) => serde_json::from_value(v).unwrap_or_default(),
        Err(_) => Default::default(),
    }
}

#[tauri::command]
pub async fn site_set_exposed(
    state: tauri::State<'_, crate::engine::EngineState>,
    exposed: std::collections::BTreeMap<String, String>,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    let v = state
        .engine()?
        .request("site_set_exposed", json!({ "exposed": exposed }))
        .await?;
    serde_json::from_value(v).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn site_map(
    state: tauri::State<'_, crate::engine::EngineState>,
    node: String,
    port: u16,
) -> Result<Value, String> {
    state
        .engine()?
        .request("site_map", json!({ "node": node, "port": port }))
        .await
}

#[tauri::command]
pub async fn site_unmap(
    state: tauri::State<'_, crate::engine::EngineState>,
    node: String,
    port: u16,
) -> Result<(), String> {
    state
        .engine()?
        .request("site_unmap", json!({ "node": node, "port": port }))
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn site_mappings(app: tauri::AppHandle) -> Vec<Value> {
    let Some(engine) = engine_of(&app) else {
        return Vec::new();
    };
    match engine.request("site_mappings", json!({})).await {
        Ok(v) => serde_json::from_value(v).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

#[tauri::command]
pub async fn site_remote_list(
    state: tauri::State<'_, crate::engine::EngineState>,
    node: String,
) -> Result<(), String> {
    state
        .engine()?
        .request("site_remote_list", json!({ "node": node }))
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn site_remote_set_exposed(
    state: tauri::State<'_, crate::engine::EngineState>,
    node: String,
    exposed: std::collections::BTreeMap<String, String>,
) -> Result<(), String> {
    state
        .engine()?
        .request(
            "site_remote_set_exposed",
            json!({ "node": node, "exposed": exposed }),
        )
        .await?;
    Ok(())
}

// ---- session + fleet -----------------------------------------------------

#[tauri::command]
pub async fn session_snapshot(app: tauri::AppHandle) -> Value {
    let Some(engine) = engine_of(&app) else {
        return Value::Null;
    };
    engine
        .request("session_snapshot", json!({}))
        .await
        .unwrap_or(Value::Null)
}

#[tauri::command]
pub async fn owned_roster(app: tauri::AppHandle) -> Value {
    let Some(engine) = engine_of(&app) else {
        return Value::Null;
    };
    engine
        .request("owned_roster", json!({}))
        .await
        .unwrap_or(Value::Null)
}

#[tauri::command]
pub async fn fleet_leave(
    state: tauri::State<'_, crate::engine::EngineState>,
) -> Result<(), String> {
    state.engine()?.request("fleet_leave", json!({})).await?;
    Ok(())
}

#[tauri::command]
pub async fn fleet_kick(
    state: tauri::State<'_, crate::engine::EngineState>,
    device: String,
    code: Option<String>,
) -> Result<(), String> {
    state
        .engine()?
        .request("fleet_kick", json!({ "device": device, "code": code }))
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn fleet_set_name(
    state: tauri::State<'_, crate::engine::EngineState>,
    name: String,
) -> Result<(), String> {
    state
        .engine()?
        .request("fleet_set_name", json!({ "name": name }))
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn fleet_grant_role(
    state: tauri::State<'_, crate::engine::EngineState>,
    device: String,
    role: String,
    code: Option<String>,
) -> Result<(), String> {
    state
        .engine()?
        .request(
            "fleet_grant_role",
            json!({ "device": device, "role": role, "code": code }),
        )
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn fleet_revoke_role(
    state: tauri::State<'_, crate::engine::EngineState>,
    device: String,
    code: Option<String>,
) -> Result<(), String> {
    state
        .engine()?
        .request(
            "fleet_revoke_role",
            json!({ "device": device, "code": code }),
        )
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn fleet_mfa_status(
    state: tauri::State<'_, crate::engine::EngineState>,
) -> Result<Value, String> {
    state.engine()?.request("fleet_mfa_status", json!({})).await
}

#[tauri::command]
pub async fn fleet_mfa_enroll(
    state: tauri::State<'_, crate::engine::EngineState>,
) -> Result<Value, String> {
    state.engine()?.request("fleet_mfa_enroll", json!({})).await
}

#[tauri::command]
pub async fn fleet_mfa_disable(
    state: tauri::State<'_, crate::engine::EngineState>,
    code: String,
) -> Result<Value, String> {
    state
        .engine()?
        .request("fleet_mfa_disable", json!({ "code": code }))
        .await
}

// ---- rooms + Shared Files --------------------------------------------------

#[tauri::command]
pub async fn room_send(
    state: tauri::State<'_, crate::engine::EngineState>,
    members: Vec<String>,
    message: Value,
) -> Result<u32, String> {
    let v = state
        .engine()?
        .request(
            "room_send",
            json!({ "members": members, "message": message }),
        )
        .await?;
    serde_json::from_value(v).map_err(|e| e.to_string())
}

/// The desktop types the reply as `Vec<allmystuff_protocol::SharedFileMeta>`
/// (`{ token, name, size }` each); the JSON passes straight through here —
/// same wire shape, no protocol crate linked in.
#[tauri::command]
pub async fn room_share_files(
    app: tauri::AppHandle,
    members: Vec<String>,
    paths: Vec<String>,
) -> Vec<Value> {
    let Some(engine) = engine_of(&app) else {
        return Vec::new();
    };
    match engine
        .request(
            "room_share_files",
            json!({ "members": members, "paths": paths }),
        )
        .await
    {
        Ok(v) => serde_json::from_value(v).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

#[tauri::command]
pub async fn room_set_share_peers(
    app: tauri::AppHandle,
    tokens: Vec<String>,
    members: Vec<String>,
) {
    if let Some(engine) = engine_of(&app) {
        let _ = engine
            .request(
                "room_set_share_peers",
                json!({ "tokens": tokens, "members": members }),
            )
            .await;
    }
}

#[tauri::command]
pub async fn room_unshare(app: tauri::AppHandle, tokens: Vec<String>) {
    if let Some(engine) = engine_of(&app) {
        let _ = engine
            .request("room_unshare", json!({ "tokens": tokens }))
            .await;
    }
}

// ---- mesh control passthroughs ----------------------------------------------

#[tauri::command]
pub async fn mesh_status(
    state: tauri::State<'_, crate::engine::EngineState>,
) -> Result<Value, String> {
    state.engine()?.request("mesh_status", json!({})).await
}

#[tauri::command]
pub async fn mesh_identity(
    state: tauri::State<'_, crate::engine::EngineState>,
) -> Result<Value, String> {
    state.engine()?.request("mesh_identity", json!({})).await
}

#[tauri::command]
pub async fn mesh_networks(
    state: tauri::State<'_, crate::engine::EngineState>,
) -> Result<Value, String> {
    state.engine()?.request("mesh_networks", json!({})).await
}

#[tauri::command]
pub async fn mesh_peers(
    state: tauri::State<'_, crate::engine::EngineState>,
    network: String,
) -> Result<Value, String> {
    state
        .engine()?
        .request("mesh_peers", json!({ "network": network }))
        .await
}

#[tauri::command]
pub async fn link_status(
    state: tauri::State<'_, crate::engine::EngineState>,
) -> Result<Value, String> {
    state.engine()?.request("link_status", json!({})).await
}

#[tauri::command]
pub async fn mesh_network_add(
    state: tauri::State<'_, crate::engine::EngineState>,
    config: Value,
) -> Result<Value, String> {
    state
        .engine()?
        .request("mesh_network_add", json!({ "config": config }))
        .await
}

#[tauri::command]
pub async fn mesh_config_show(
    state: tauri::State<'_, crate::engine::EngineState>,
) -> Result<Value, String> {
    state.engine()?.request("mesh_config_show", json!({})).await
}

#[tauri::command]
pub async fn mesh_network_update(
    state: tauri::State<'_, crate::engine::EngineState>,
    config: Value,
) -> Result<Value, String> {
    state
        .engine()?
        .request("mesh_network_update", json!({ "config": config }))
        .await
}

#[tauri::command]
pub async fn mesh_roster_approve(
    state: tauri::State<'_, crate::engine::EngineState>,
    network: String,
    device_id: String,
    label: Option<String>,
) -> Result<Value, String> {
    state
        .engine()?
        .request(
            "mesh_roster_approve",
            json!({ "network": network, "device_id": device_id, "label": label }),
        )
        .await
}

#[tauri::command]
pub async fn mesh_roster_remove(
    state: tauri::State<'_, crate::engine::EngineState>,
    network: String,
    device_id: String,
) -> Result<Value, String> {
    state
        .engine()?
        .request(
            "mesh_roster_remove",
            json!({ "network": network, "device_id": device_id }),
        )
        .await
}

#[tauri::command]
pub async fn mesh_roster_list(
    state: tauri::State<'_, crate::engine::EngineState>,
    network: String,
) -> Result<Value, String> {
    state
        .engine()?
        .request("mesh_roster_list", json!({ "network": network }))
        .await
}

#[tauri::command]
pub async fn mesh_network_id_generate(
    state: tauri::State<'_, crate::engine::EngineState>,
) -> Result<Value, String> {
    state
        .engine()?
        .request("mesh_network_id_generate", json!({}))
        .await
}

#[tauri::command]
pub async fn mesh_network_remove(
    state: tauri::State<'_, crate::engine::EngineState>,
    network: String,
) -> Result<Value, String> {
    state
        .engine()?
        .request("mesh_network_remove", json!({ "network": network }))
        .await
}

#[tauri::command]
pub async fn disabled_networks(app: tauri::AppHandle) -> Vec<Value> {
    let Some(engine) = engine_of(&app) else {
        return Vec::new();
    };
    match engine.request("disabled_networks", json!({})).await {
        Ok(v) => serde_json::from_value(v).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

#[tauri::command]
pub async fn network_set_enabled(
    state: tauri::State<'_, crate::engine::EngineState>,
    network: String,
    enabled: bool,
) -> Result<Value, String> {
    state
        .engine()?
        .request(
            "network_set_enabled",
            json!({ "network": network, "enabled": enabled }),
        )
        .await
}

/// GUI name ≠ wire name here, same as the desktop: the frontend invokes
/// `network_reconnect`, the node's dispatch answers `mesh_network_reconnect`.
#[tauri::command]
pub async fn network_reconnect(
    state: tauri::State<'_, crate::engine::EngineState>,
    network: Option<String>,
    peer: Option<String>,
) -> Result<Value, String> {
    state
        .engine()?
        .request(
            "mesh_network_reconnect",
            json!({ "network": network, "peer": peer }),
        )
        .await
}

#[tauri::command]
pub async fn mesh_identity_set_label(
    state: tauri::State<'_, crate::engine::EngineState>,
    label: String,
) -> Result<Value, String> {
    state
        .engine()?
        .request("mesh_identity_set_label", json!({ "label": label }))
        .await
}
