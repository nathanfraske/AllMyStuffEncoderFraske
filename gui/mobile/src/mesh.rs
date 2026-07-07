//! Wires the shared Svelte frontend to a **real in-process mesh node**.
//!
//! On launch the app opens the embedded `myownmesh-core` engine
//! ([`allmystuff_mesh::EngineMesh`]), joins the LAN rendezvous over mDNS — no
//! fleet, account, or relay needed — and rejoins every network the user added,
//! so peers appear on the graph. The engine is held in Tauri managed state; the
//! frontend's commands are answered straight from it, and a presence change
//! re-emits `allmystuff://session` so the graph re-renders live.
//!
//! Beyond discovery, this module answers the **mesh management** commands the
//! shared UI drives — rename (`mesh_identity_set_label`), joining/leaving
//! meshes (`mesh_network_add`/`remove`/`update`), reconnect, and parking
//! (`network_set_enabled` / `disabled_networks`) — in the same shapes the
//! desktop's node answers them, so the venue/network UI works unchanged. The
//! phone has no daemon: the "park store" and the network list persist in a
//! small JSON settings file in the app's data dir, and the engine is told
//! directly.
//!
//! Gated behind the default `mesh` feature so the NDK-free `gui-mobile` CI job
//! (which builds `--no-default-features`) still type-checks the shell without
//! linking the engine's C deps.

use std::sync::{Arc, Mutex};

use allmystuff_mesh::{lan_discovery_config, EngineMesh, NetworkConfig, LOCAL_CLAIM_NETWORK_ID};
use allmystuff_mobile_core::prelude::{
    mobile_profile, profile_request, Inbound, MeshClient, MobileNodeConfig, NodeId,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::{boot_id, os_label};

/// The embedded mesh node once joined. `None` until [`join`] completes. Held in
/// Tauri managed state so every command reads the same engine.
#[derive(Default)]
pub struct MeshState(pub Arc<Mutex<Option<EngineMesh>>>);

/// Persistent device-key file, in the app's data dir. v1 stores the raw seed in
/// the app sandbox; moving it into the iOS Keychain / Android Keystore is a
/// later hardening step (the engine already takes the key either way).
const SEED_FILE: &str = "device-seed.bin";

/// The phone's mesh settings — its display name, the networks the user has
/// joined, and the parked ("switched off") ones. The mobile counterpart of the
/// desktop node's config + park store, persisted as JSON in the app data dir.
const SETTINGS_FILE: &str = "mesh-settings.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct MeshSettings {
    /// Display-name override; `None` = the default label.
    label: Option<String>,
    /// Every network the user added (their configs as handed to
    /// `mesh_network_add`), rejoined on launch.
    networks: Vec<Value>,
    /// Parked networks — full configs kept so switching one back on restores
    /// it exactly (the desktop's disabled-networks store).
    disabled: Vec<Value>,
    /// The LAN rendezvous is node-owned and can't be removed, only switched
    /// off; this is its park flag.
    lan_disabled: bool,
}

fn settings_path(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join(SETTINGS_FILE))
}

fn load_settings(app: &AppHandle) -> MeshSettings {
    settings_path(app)
        .ok()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_settings(app: &AppHandle, settings: &MeshSettings) -> Result<(), String> {
    let path = settings_path(app)?;
    let bytes = serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(path, bytes).map_err(|e| e.to_string())
}

/// The default display name before the user renames the phone.
const DEFAULT_LABEL: &str = "My Phone";

/// The config id / network id a settings entry answers to (either handle
/// works, like the daemon).
fn config_matches(config: &Value, network: &str) -> bool {
    config.get("id").and_then(Value::as_str) == Some(network)
        || config.get("network_id").and_then(Value::as_str) == Some(network)
}

/// Load the phone's persistent 32-byte ed25519 seed, generating + storing one
/// on first run so the device keeps a stable mesh identity across launches.
fn load_or_create_seed(app: &AppHandle) -> Result<[u8; 32], String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(SEED_FILE);
    if let Ok(bytes) = std::fs::read(&path) {
        if bytes.len() == 32 {
            let mut seed = [0u8; 32];
            seed.copy_from_slice(&bytes);
            return Ok(seed);
        }
    }
    let mut seed = [0u8; 32];
    getrandom::getrandom(&mut seed).map_err(|e| e.to_string())?;
    std::fs::write(&path, seed).map_err(|e| e.to_string())?;
    Ok(seed)
}

/// Build + broadcast this phone's AllMyStuff presence so peers put it on their
/// graph. Called off the runtime (from commands / [`join`]), never from the
/// inbound sink (`advertise` blocks on the engine runtime).
fn advertise_self(mesh: &EngineMesh) {
    let id = mesh.device_id().to_string();
    let cfg = MobileNodeConfig::new(mesh.label(), os_label());
    let profile = mobile_profile(
        &NodeId::from(id.as_str()),
        &cfg,
        boot_id(),
        env!("CARGO_PKG_VERSION"),
    );
    let _ = mesh.advertise(&profile);
}

fn emit_session(app: &AppHandle, mesh: &EngineMesh) {
    let _ = app.emit("allmystuff://session", mesh.session_snapshot());
}

/// Open the engine, join the LAN mesh + every stored network, and install the
/// presence→UI bridge. Idempotent. Runs on a background thread from the app's
/// `setup` hook so the UI comes up immediately while the node connects.
pub fn join(app: &AppHandle) -> Result<String, String> {
    let state = app.state::<MeshState>();
    if let Some(m) = state.0.lock().unwrap().as_ref() {
        return Ok(m.device_id().to_string());
    }

    let seed = load_or_create_seed(app)?;
    let settings = load_settings(app);
    let label = settings
        .label
        .clone()
        .unwrap_or_else(|| DEFAULT_LABEL.into());

    // A presence advert changed the roster → poke the frontend to re-render the
    // graph with the fresh snapshot. Reads the same engine slot we store below;
    // by the time inbound frames flow (after join), the slot is populated.
    let slot = state.0.clone();
    let emit_app = app.clone();
    let sink: allmystuff_mesh::InboundSink = Arc::new(move |inbound| {
        if matches!(inbound, Inbound::Presence(_)) {
            if let Some(m) = slot.lock().unwrap().as_ref() {
                let _ = emit_app.emit("allmystuff://session", m.session_snapshot());
            }
        }
    });

    let mesh = EngineMesh::open(seed, label, sink).map_err(|e| e.to_string())?;
    if !settings.lan_disabled {
        if let Err(e) = mesh.join_network(lan_discovery_config()) {
            eprintln!("[mesh] LAN join failed: {e}");
        }
    }
    // Rejoin every network the user added; a failure (bad stored config, no
    // route) skips that network rather than sinking the whole node.
    for config in &settings.networks {
        match serde_json::from_value::<NetworkConfig>(config.clone()) {
            Ok(cfg) => {
                let id = cfg.id.clone();
                if let Err(e) = mesh.join_network(cfg) {
                    eprintln!("[mesh] rejoin of {id} failed: {e}");
                }
            }
            Err(e) => eprintln!("[mesh] stored network config unreadable: {e}"),
        }
    }

    let id = mesh.device_id().to_string();
    advertise_self(&mesh);
    let snapshot = mesh.session_snapshot();
    *state.0.lock().unwrap() = Some(mesh);
    // Flip the UI to the live (ready, real-id) state immediately — even with
    // zero peers — rather than waiting for the first inbound presence advert.
    let _ = app.emit("allmystuff://session", snapshot);
    Ok(id)
}

/// Run `f` against the live engine, or fail the way the desktop does before
/// its node is up.
fn with_mesh<T>(
    state: &State<'_, MeshState>,
    f: impl FnOnce(&EngineMesh) -> Result<T, String>,
) -> Result<T, String> {
    let guard = state.0.lock().unwrap();
    match guard.as_ref() {
        Some(m) => f(m),
        None => Err("the mesh node isn't up yet".into()),
    }
}

// ---- discovery/graph commands the shared frontend polls --------------------
//
// These degrade to the empty/not-ready shape before the node has joined, so
// the UI shows its demo state until the mesh is up rather than erroring.

/// `{ready, me, network, peers, routes, shares}` — the peers presence has found.
#[tauri::command]
pub fn session_snapshot(state: State<'_, MeshState>) -> Value {
    state
        .0
        .lock()
        .unwrap()
        .as_ref()
        .map(|m| m.session_snapshot())
        .unwrap_or_else(|| json!({ "ready": false }))
}

/// `{networks: [...]}` — every joined network, daemon `NetworkSummary` shape.
#[tauri::command]
pub fn mesh_networks(state: State<'_, MeshState>) -> Value {
    state
        .0
        .lock()
        .unwrap()
        .as_ref()
        .map(|m| m.networks())
        .unwrap_or_else(|| json!({ "networks": [] }))
}

/// `{peers: [...]}` — the connected peers + capability adverts (the liveness
/// feed), scoped to `network` when given.
#[tauri::command]
pub fn mesh_peers(state: State<'_, MeshState>, network: Option<String>) -> Value {
    state
        .0
        .lock()
        .unwrap()
        .as_ref()
        .map(|m| m.mesh_peers(network.as_deref()))
        .unwrap_or_else(|| json!({ "peers": [] }))
}

/// `{roster: [...]}` — empty: the phone's networks auto-approve today, so
/// there is no curated roster to return.
#[tauri::command]
pub fn mesh_roster_list(_state: State<'_, MeshState>, network: Option<String>) -> Value {
    let _ = network;
    json!({ "roster": [] })
}

// ---- mesh management: rename, join/leave, reconnect, park ------------------

/// `{device_id, pubkey, label}` — daemon `IdentityShow` shape (`device_id` is
/// the display id, pubkey + 5-char suffix).
#[tauri::command]
pub fn mesh_identity(state: State<'_, MeshState>) -> Result<Value, String> {
    with_mesh(&state, |m| {
        Ok(json!({
            "device_id": m.display_id(),
            "pubkey": m.device_id(),
            "label": m.label(),
        }))
    })
}

/// Rename this device. Persists the label, updates the engine identity, and
/// re-broadcasts presence so every peer's graph re-labels the phone.
#[tauri::command]
pub fn mesh_identity_set_label(
    app: AppHandle,
    state: State<'_, MeshState>,
    label: String,
) -> Result<Value, String> {
    let out = with_mesh(&state, |m| {
        m.set_label(&label);
        advertise_self(m);
        Ok(json!({
            "device_id": m.display_id(),
            "pubkey": m.device_id(),
            "label": m.label(),
        }))
    })?;
    let mut settings = load_settings(&app);
    settings.label = Some(label);
    save_settings(&app, &settings)?;
    if let Some(m) = state.0.lock().unwrap().as_ref() {
        emit_session(&app, m);
    }
    Ok(out)
}

/// Join a network from its full config (the daemon `network_add` shape) and
/// persist it so the phone rejoins on relaunch.
#[tauri::command]
pub fn mesh_network_add(
    app: AppHandle,
    state: State<'_, MeshState>,
    config: Value,
) -> Result<Value, String> {
    let cfg: NetworkConfig =
        serde_json::from_value(config.clone()).map_err(|e| format!("bad network config: {e}"))?;
    let id = cfg.id.clone();
    with_mesh(&state, |m| m.join_network(cfg).map_err(|e| e.to_string()))?;
    let mut settings = load_settings(&app);
    settings.networks.retain(|c| !config_matches(c, &id));
    settings.networks.push(config);
    save_settings(&app, &settings)?;
    Ok(json!({ "joined": id }))
}

/// Leave a network (config id or wire id) and forget it. The LAN rendezvous
/// can't be left — only switched off — exactly like the desktop node.
#[tauri::command]
pub fn mesh_network_remove(
    app: AppHandle,
    state: State<'_, MeshState>,
    network: String,
) -> Result<Value, String> {
    if network == LOCAL_CLAIM_NETWORK_ID {
        return Err("the local claiming network can't be left — switch it off instead".into());
    }
    with_mesh(&state, |m| {
        m.leave_network(&network).map_err(|e| e.to_string())
    })?;
    let mut settings = load_settings(&app);
    settings.networks.retain(|c| !config_matches(c, &network));
    settings.disabled.retain(|c| !config_matches(c, &network));
    save_settings(&app, &settings)?;
    Ok(json!({ "removed": network }))
}

/// Edit a network's config: leave the old instance, rejoin with the new
/// settings, persist. The LAN rendezvous has no settings to edit.
#[tauri::command]
pub fn mesh_network_update(
    app: AppHandle,
    state: State<'_, MeshState>,
    config: Value,
) -> Result<Value, String> {
    let cfg: NetworkConfig =
        serde_json::from_value(config.clone()).map_err(|e| format!("bad network config: {e}"))?;
    if cfg.id == LOCAL_CLAIM_NETWORK_ID || cfg.network_id == LOCAL_CLAIM_NETWORK_ID {
        return Err(
            "the local claiming network has no settings to edit — it's the fixed \
             mDNS passthrough for claiming and local pairing"
                .into(),
        );
    }
    let id = cfg.id.clone();
    with_mesh(&state, |m| {
        // Leave whichever instance answers to this handle (it may be joined
        // under the old config), then rejoin with the new settings.
        let _ = m.leave_network(&id);
        m.join_network(cfg).map_err(|e| e.to_string())
    })?;
    let mut settings = load_settings(&app);
    settings.networks.retain(|c| !config_matches(c, &id));
    settings.networks.push(config);
    save_settings(&app, &settings)?;
    Ok(json!({ "updated": id }))
}

/// Reconnect in place — redial signaling and renegotiate ICE without leaving.
/// `network` scopes to one mesh; `peer` to one node.
#[tauri::command]
pub fn network_reconnect(
    state: State<'_, MeshState>,
    network: Option<String>,
    peer: Option<String>,
) -> Result<Value, String> {
    with_mesh(&state, |m| {
        m.reconnect(network.as_deref(), peer.as_deref())
            .map_err(|e| e.to_string())
    })?;
    Ok(json!({}))
}

/// Switch a network off (leave, but park its config) or back on (rejoin from
/// the parked config). The roster/settings survive the off period.
#[tauri::command]
pub fn network_set_enabled(
    app: AppHandle,
    state: State<'_, MeshState>,
    network: String,
    enabled: bool,
) -> Result<Value, String> {
    let mut settings = load_settings(&app);

    if network == LOCAL_CLAIM_NETWORK_ID {
        with_mesh(&state, |m| {
            if enabled && !m.has_network(LOCAL_CLAIM_NETWORK_ID) {
                m.join_network(lan_discovery_config())
                    .map_err(|e| e.to_string())?;
            } else if !enabled && m.has_network(LOCAL_CLAIM_NETWORK_ID) {
                m.leave_network(LOCAL_CLAIM_NETWORK_ID)
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        })?;
        settings.lan_disabled = !enabled;
        save_settings(&app, &settings)?;
        return Ok(json!({}));
    }

    if enabled {
        let Some(pos) = settings
            .disabled
            .iter()
            .position(|c| config_matches(c, &network))
        else {
            return Err(format!("unknown disabled network: {network}"));
        };
        let config = settings.disabled.remove(pos);
        let cfg: NetworkConfig = serde_json::from_value(config.clone())
            .map_err(|e| format!("parked config unreadable: {e}"))?;
        with_mesh(&state, |m| m.join_network(cfg).map_err(|e| e.to_string()))?;
        settings.networks.retain(|c| !config_matches(c, &network));
        settings.networks.push(config);
    } else {
        let Some(pos) = settings
            .networks
            .iter()
            .position(|c| config_matches(c, &network))
        else {
            return Err(format!("unknown network: {network}"));
        };
        with_mesh(&state, |m| {
            m.leave_network(&network).map_err(|e| e.to_string())
        })?;
        let config = settings.networks.remove(pos);
        settings.disabled.push(config);
    }
    save_settings(&app, &settings)?;
    Ok(json!({}))
}

/// The parked networks (their full configs) for the pill menu's disabled rows.
#[tauri::command]
pub fn disabled_networks(app: AppHandle) -> Vec<Value> {
    let settings = load_settings(&app);
    let mut out = settings.disabled.clone();
    if settings.lan_disabled {
        out.push(serde_json::to_value(lan_discovery_config()).unwrap_or(Value::Null));
    }
    out
}

/// A fresh, well-formed network id — daemon `NetworkIdGenerate` shape.
#[tauri::command]
pub fn mesh_network_id_generate() -> Value {
    json!({ "network_id": allmystuff_mesh::generate_network_id() })
}

/// Daemon `Status`-shaped summary of this node.
#[tauri::command]
pub fn mesh_status(state: State<'_, MeshState>) -> Result<Value, String> {
    with_mesh(&state, |m| {
        let networks = m.networks();
        let joined: Vec<Value> = networks["networks"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|n| n.get("network_id").cloned())
                    .collect()
            })
            .unwrap_or_default();
        Ok(json!({
            "version": env!("CARGO_PKG_VERSION"),
            "device_id": m.display_id(),
            "joined_networks": joined,
        }))
    })
}

/// Daemon `ConfigShow`-shaped view: every network's full config (joined +
/// parked), for the Servers settings pane.
#[tauri::command]
pub fn mesh_config_show(app: AppHandle, state: State<'_, MeshState>) -> Result<Value, String> {
    let settings = load_settings(&app);
    with_mesh(&state, |m| {
        let mut networks: Vec<Value> = m
            .network_configs()
            .into_iter()
            .filter_map(|c| serde_json::to_value(c).ok())
            .collect();
        networks.extend(settings.disabled.iter().cloned());
        Ok(json!({ "config": { "networks": networks } }))
    })
}

/// Re-learn a node's details. `node` given: ask that peer to re-announce (a
/// `ProfileRequest`) and redial it; omitted: re-broadcast our own presence.
#[tauri::command]
pub fn refresh_node(state: State<'_, MeshState>, node: Option<String>) -> Result<Value, String> {
    with_mesh(&state, |m| {
        match node {
            Some(peer) => {
                let _ = m.send_control(&peer, &profile_request());
                let _ = m.reconnect(None, Some(&peer));
            }
            None => advertise_self(m),
        }
        Ok(json!({}))
    })
}
