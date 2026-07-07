//! Wires the shared Svelte frontend to a **real in-process mesh node**.
//!
//! On launch the app opens the embedded `myownmesh-core` engine
//! ([`allmystuff_mesh::EngineMesh`]) and joins the LAN rendezvous over mDNS —
//! no fleet, account, or relay needed — so peers on the same network appear on
//! the graph. The engine is held in Tauri managed state; the frontend's
//! existing commands (`session_snapshot`, `mesh_networks`, `mesh_peers`,
//! `mesh_roster_list`) are answered straight from it, and a presence change
//! re-emits `allmystuff://session` so the graph re-renders live.
//!
//! Gated behind the default `mesh` feature so the NDK-free `gui-mobile` CI job
//! (which builds `--no-default-features`) still type-checks the shell without
//! linking the engine's C deps.

use std::sync::{Arc, Mutex};

use allmystuff_mesh::EngineMesh;
use allmystuff_mobile_core::prelude::{
    mobile_profile, Inbound, MeshClient, MobileNodeConfig, NodeId,
};
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

/// This phone's display name on the graph. A settings screen can override it
/// later; for now a stable placeholder.
fn device_label() -> String {
    "My Phone".to_string()
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
/// graph. Called off the runtime (from [`join`]), never from the inbound sink
/// (`advertise` blocks on the engine runtime).
fn advertise_self(mesh: &EngineMesh) {
    let id = mesh.device_id().to_string();
    let cfg = MobileNodeConfig::new(device_label(), os_label());
    let profile = mobile_profile(
        &NodeId::from(id.as_str()),
        &cfg,
        boot_id(),
        env!("CARGO_PKG_VERSION"),
    );
    let _ = mesh.advertise(&profile);
}

/// Open the engine, join the LAN mesh, and install the presence→UI bridge.
/// Idempotent. Runs on a background thread from the app's `setup` hook so the
/// UI comes up immediately while the node connects.
pub fn join(app: &AppHandle) -> Result<String, String> {
    let state = app.state::<MeshState>();
    if let Some(m) = state.0.lock().unwrap().as_ref() {
        return Ok(m.device_id().to_string());
    }

    let seed = load_or_create_seed(app)?;

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

    let mesh = EngineMesh::open_lan(seed, device_label(), sink).map_err(|e| e.to_string())?;
    let id = mesh.device_id().to_string();
    advertise_self(&mesh);
    let snapshot = mesh.session_snapshot();
    *state.0.lock().unwrap() = Some(mesh);
    // Flip the UI to the live (ready, real-id) state immediately — even with
    // zero peers — rather than waiting for the first inbound presence advert.
    let _ = app.emit("allmystuff://session", snapshot);
    Ok(id)
}

// ---- the discovery/graph commands the shared frontend calls ----------------
//
// All degrade to the empty/not-ready shape before the node has joined, so the
// UI shows its demo state until the mesh is up rather than erroring.

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

/// `{networks: [...]}` — the one LAN network this phone is on.
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

/// `{peers: [...]}` — the connected peers + their capability adverts (the
/// liveness feed). `network` is accepted for API parity but ignored: the phone
/// is on a single network.
#[tauri::command]
pub fn mesh_peers(state: State<'_, MeshState>, network: Option<String>) -> Value {
    let _ = network;
    state
        .0
        .lock()
        .unwrap()
        .as_ref()
        .map(|m| m.mesh_peers())
        .unwrap_or_else(|| json!({ "peers": [] }))
}

/// `{roster: [...]}` — empty: the LAN network auto-approves, so there is no
/// curated roster to return.
#[tauri::command]
pub fn mesh_roster_list(_state: State<'_, MeshState>, network: Option<String>) -> Value {
    let _ = network;
    json!({ "roster": [] })
}
