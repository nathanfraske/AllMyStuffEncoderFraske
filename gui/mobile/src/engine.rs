//! The whole stack, one process: the mesh daemon, the node engine, the GUI.
//!
//! On every other platform these are three processes — the GUI spawns
//! `allmystuff-serve` (the node), which spawns-or-attaches `myownmesh serve`
//! (the daemon) — because daemons can run in the background and be shared
//! across apps. iOS forbids spawning processes *and* offers no background
//! daemons to share, so the separation buys nothing here: this module runs
//! the identical pieces as tasks in the app's own process, piled together.
//!
//! The boot is `node/src/bin/serve.rs` minus the two spawns:
//!
//! 1. [`myownmesh::embedded::start`] — the daemon, in-process, listening on
//!    the same control socket (a unix socket inside the app sandbox; sockets
//!    are allowed, processes aren't).
//! 2. [`allmystuff_node::mesh::Mesh::new`] with a Tauri-backed [`UiSink`] —
//!    the engine, emitting `allmystuff://…` events straight onto the
//!    webview bus instead of fanning them over the node socket.
//! 3. Tauri commands (see [`crate::commands`]) call
//!    [`node_control::dispatch`] directly — the same match the desktop
//!    reaches over its socket, minus the socket.
//!
//! Everything above the process boundary is byte-identical to desktop: the
//! daemon wire protocol, the engine's bring-up (identity → profile → claim
//! networks → subscriptions → presence), the frontend event contract.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use allmystuff_node::control_client::ControlClient;
use allmystuff_node::mesh::Mesh;
use allmystuff_node::networks_store::DisabledNetworks;
use allmystuff_node::node_control::{dispatch, DispatchOut, NodeRequest};
use allmystuff_node::UiSink;

/// The live in-process stack. Held in Tauri state for the app's lifetime;
/// dropping it would tear the mesh down, so nothing drops it.
pub struct Engine {
    mesh: Arc<Mesh>,
    client: Arc<ControlClient>,
    disabled: Arc<DisabledNetworks>,
    /// The embedded daemon. Never shut down explicitly — iOS gives no
    /// "about to terminate" moment worth trusting; peers age the phone out
    /// through the same heartbeat that covers a battery dying.
    _daemon: myownmesh::embedded::EmbeddedDaemon,
}

impl Engine {
    /// One command through the node's dispatch — the same entry the desktop
    /// reaches over the node socket.
    pub async fn request(&self, cmd: &str, args: Value) -> Result<Value, String> {
        let req = NodeRequest {
            cmd: cmd.to_string(),
            args,
        };
        match dispatch(&self.mesh, &self.client, &self.disabled, req).await {
            DispatchOut::Json(v) => Ok(v),
            DispatchOut::Bytes(_) => Err(format!("{cmd}: binary reply for a json command")),
            DispatchOut::Err(e) => Err(e),
        }
    }

    /// [`request`](Self::request) for the byte-returning commands
    /// (video/term/file polls).
    pub async fn request_bytes(&self, cmd: &str, args: Value) -> Result<Vec<u8>, String> {
        let req = NodeRequest {
            cmd: cmd.to_string(),
            args,
        };
        match dispatch(&self.mesh, &self.client, &self.disabled, req).await {
            DispatchOut::Bytes(b) => Ok(b),
            DispatchOut::Json(_) => Err(format!("{cmd}: json reply for a binary command")),
            DispatchOut::Err(e) => Err(e),
        }
    }

    /// The engine's device id, for [`crate::scan_self`]'s node card.
    pub async fn device_id(&self) -> Option<String> {
        self.request("mesh_identity", json!({}))
            .await
            .ok()
            .and_then(|v| {
                v.get("device_id")
                    .or_else(|| v.get("id"))
                    .and_then(|s| s.as_str())
                    .map(str::to_string)
            })
    }
}

/// Tauri state: `None` until [`boot`] finishes on its background task.
/// Commands answer "node not ready" meanwhile and the frontend's `tryInvoke`
/// degrades exactly as it does on a desktop whose node hasn't answered yet.
#[derive(Default)]
pub struct EngineState(pub Mutex<Option<Arc<Engine>>>);

impl EngineState {
    pub fn engine(&self) -> Result<Arc<Engine>, String> {
        self.0
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "node not ready".to_string())
    }
}

/// The engine's front-end seam, made real: `lib.rs` describes `UiSink` as
/// "the GUI supplies a Tauri-backed sink" — this is that sink. Every event
/// the engine emits (`allmystuff://session`, `…/video-ready`, `…/term-exit`)
/// lands on the webview bus directly; the desktop's socket fan-out
/// (`SocketSink` → `NodeEvent` → `run_event_pump` → `app.emit`) collapses to
/// one call.
struct TauriSink(AppHandle);

impl UiSink for TauriSink {
    fn emit(&self, event: &str, payload: Value) {
        let _ = self.0.emit(event, payload);
    }

    /// Desktop relaunches onto a freshly-applied self-update here. iOS apps
    /// update through the App Store and must not relaunch themselves; the
    /// honest translation is a clean exit (never reached — the updater is
    /// config-off in the embedded daemon and the node ships no update path
    /// on this platform).
    fn restart(&self) -> ! {
        std::process::exit(0)
    }
}

/// Boot the piled-together stack: migrate any adapter-era state, start the
/// embedded daemon, then bring the engine up against it. Mirrors
/// `serve.rs`'s order with the spawns deleted. Must run on the Tauri async
/// runtime — `Mesh::start` binds the engine's task spawner to the runtime
/// it is started on.
pub async fn boot(app: AppHandle) -> Result<Arc<Engine>, String> {
    migrate_adapter_state(&app);

    // The daemon, in-process. Config lives at `$HOME/.myownmesh/` — inside
    // the app sandbox $HOME is the app container, so the daemon's socket,
    // identity, and config land in app-private storage, and the engine's
    // ControlClient (which resolves the same way) finds the same socket.
    let cfg = myownmesh_core::MeshConfig::load().map_err(|e| format!("load mesh config: {e}"))?;
    let daemon = myownmesh::embedded::start(cfg)
        .await
        .map_err(|e| format!("start embedded daemon: {e}"))?;
    tracing::info!(
        device_id = %daemon.mesh().identity().display_id(),
        "embedded daemon up"
    );

    // The engine, exactly as serve.rs builds it: resolve the daemon socket,
    // build the mesh with the front-end sink, share the park store, start
    // the session pump (which registers the runtime and brings the node up:
    // identity → profile → claim networks → subscriptions → presence).
    let client = Arc::new(ControlClient::new().map_err(|e| format!("resolve daemon socket: {e}"))?);
    let disabled = Arc::new(DisabledNetworks::load());
    let mesh = Mesh::new(client.clone(), Arc::new(TauriSink(app)));
    mesh.attach_disabled_networks(disabled.clone());
    mesh.clone().start().await;

    Ok(Arc::new(Engine {
        mesh,
        client,
        disabled,
        _daemon: daemon,
    }))
}

// ---- one-time migration from the adapter-era stores ----------------------
//
// Earlier builds of this app ran a hand-rolled engine adapter that kept its
// own state in the Tauri app-data dir: `device-seed.bin` (the ed25519 seed)
// and `mesh-settings.json` ({label, networks, disabled, lan_disabled}).
// The real stack keeps identity in the daemon's anchor file and networks in
// the daemon config / the node's park store. Carry everything over so the
// phone keeps its device id (peers know it) and its joined networks, then
// rename the old files `.migrated` so this never runs twice.

const SEED_FILE: &str = "device-seed.bin";
const SETTINGS_FILE: &str = "mesh-settings.json";

fn migrate_adapter_state(app: &AppHandle) {
    let Ok(dir) = app.path().app_data_dir() else {
        return;
    };

    let seed_path = dir.join(SEED_FILE);
    if seed_path.exists() {
        match migrate_identity(&seed_path, &dir.join(SETTINGS_FILE)) {
            Ok(true) => tracing::info!("identity migrated from the adapter-era device seed"),
            Ok(false) => {}
            Err(e) => tracing::warn!("identity migration failed (a fresh id will be minted): {e}"),
        }
        let _ = std::fs::rename(&seed_path, dir.join(format!("{SEED_FILE}.migrated")));
    }

    let settings_path = dir.join(SETTINGS_FILE);
    if settings_path.exists() {
        match migrate_settings(&settings_path) {
            Ok(()) => tracing::info!("network settings migrated into the daemon config"),
            Err(e) => tracing::warn!("settings migration failed (re-add networks by hand): {e}"),
        }
        let _ = std::fs::rename(
            &settings_path,
            dir.join(format!("{SETTINGS_FILE}.migrated")),
        );
    }
}

/// Write the daemon's identity anchor from the adapter's raw 32-byte seed —
/// same key, same device id, new home. Refuses to overwrite an existing
/// anchor (that identity is already the live one). Returns whether an
/// anchor was written.
fn migrate_identity(
    seed_path: &std::path::Path,
    settings_path: &std::path::Path,
) -> Result<bool, String> {
    let anchor_dir = myownmesh_core::dirs::data_dir()
        .map_err(|e| e.to_string())?
        .join(".secrets");
    let anchor_path = anchor_dir.join("identity.json");
    if anchor_path.exists() {
        return Ok(false);
    }

    let bytes = std::fs::read(seed_path).map_err(|e| e.to_string())?;
    let seed: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| format!("seed file is {} bytes, expected 32", bytes.len()))?;
    let signing = ed25519_dalek::SigningKey::from_bytes(&seed);
    let b32 = |b: &[u8]| data_encoding::BASE32_NOPAD.encode(b).to_lowercase();

    // The label rode the adapter's settings file; it belongs in the anchor.
    let label = std::fs::read(settings_path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|s| s.get("label").and_then(|l| l.as_str()).map(str::to_string))
        .unwrap_or_default();

    let anchor = json!({
        "version": 1,
        "created_at": format!(
            "unix:{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        ),
        "secret_key": b32(&seed),
        "public_key": b32(signing.verifying_key().as_bytes()),
        "label": label,
    });

    std::fs::create_dir_all(&anchor_dir).map_err(|e| e.to_string())?;
    std::fs::write(
        &anchor_path,
        serde_json::to_vec_pretty(&anchor).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(true)
}

/// Fold the adapter's joined networks into the daemon config (so
/// `embedded::start` joins them, like `serve`), and its parked ones into
/// the node's park store. `lan_disabled` parks the LAN local-claim config —
/// presence in the park store *is* that network's off switch (see
/// `ensure_claim_networks`).
fn migrate_settings(settings_path: &std::path::Path) -> Result<(), String> {
    let settings: Value =
        serde_json::from_slice(&std::fs::read(settings_path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;

    let mut cfg = myownmesh_core::MeshConfig::load().map_err(|e| e.to_string())?;
    if let Some(networks) = settings.get("networks").and_then(|n| n.as_array()) {
        for net in networks {
            match serde_json::from_value::<myownmesh_core::NetworkConfig>(net.clone()) {
                Ok(parsed) => {
                    if !cfg
                        .networks
                        .iter()
                        .any(|n| n.network_id == parsed.network_id)
                    {
                        cfg.networks.push(parsed);
                    }
                }
                Err(e) => tracing::warn!("skipping unparseable stored network: {e}"),
            }
        }
    }
    cfg.save().map_err(|e| e.to_string())?;

    let disabled = DisabledNetworks::load();
    if let Some(parked) = settings.get("disabled").and_then(|d| d.as_array()) {
        for net in parked {
            disabled.park(net.clone());
        }
    }
    if settings
        .get("lan_disabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        // The exact config ensure_claim_networks would add — parking it is
        // how "off" is expressed for the un-removable LAN rendezvous.
        disabled.park(json!({
            "id": allmystuff_protocol::LOCAL_CLAIM_NETWORK_ID,
            "network_id": allmystuff_protocol::LOCAL_CLAIM_NETWORK_ID,
            "label": "Local claiming (this LAN)",
            "kind": "open",
            "auto_approve": true,
            "signaling": { "strategy": "none", "mdns": true },
            "stun_servers": [],
            "turn_servers": [],
        }));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test fn on purpose: the migration resolves every store through
    /// `MYOWNMESH_HOME`, and env vars are process-global — parallel test fns
    /// would race the variable.
    #[test]
    fn adapter_state_migrates_into_the_engine_stores() {
        let home = std::env::temp_dir().join(format!("ams-migrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("MYOWNMESH_HOME", &home);

        // -- identity: seed bytes → anchor with the same key, label carried.
        let seed = [7u8; 32];
        let seed_path = home.join("device-seed.bin");
        std::fs::write(&seed_path, seed).unwrap();
        let settings_path = home.join("mesh-settings.json");
        std::fs::write(
            &settings_path,
            serde_json::to_vec(&json!({
                "label": "Chris's phone",
                "networks": [{
                    "id": "venue",
                    "network_id": "abcdefabcdefabcdefabcdefabcdefab",
                    "label": "Venue",
                }],
                "disabled": [],
                "lan_disabled": true,
            }))
            .unwrap(),
        )
        .unwrap();

        assert!(migrate_identity(&seed_path, &settings_path).unwrap());
        let anchor: Value =
            serde_json::from_slice(&std::fs::read(home.join(".secrets/identity.json")).unwrap())
                .unwrap();
        let expect_pub = data_encoding::BASE32_NOPAD
            .encode(
                ed25519_dalek::SigningKey::from_bytes(&seed)
                    .verifying_key()
                    .as_bytes(),
            )
            .to_lowercase();
        assert_eq!(anchor["public_key"], json!(expect_pub));
        assert_eq!(anchor["label"], json!("Chris's phone"));
        // The daemon must load it back as the same identity.
        let loaded = myownmesh_core::identity::load_or_create().unwrap();
        assert_eq!(loaded.public_id(), expect_pub);
        // Second run refuses to clobber the live anchor.
        assert!(!migrate_identity(&seed_path, &settings_path).unwrap());

        // -- settings: networks → daemon config; lan_disabled → park store.
        migrate_settings(&settings_path).unwrap();
        let cfg = myownmesh_core::MeshConfig::load().unwrap();
        assert!(cfg
            .networks
            .iter()
            .any(|n| n.network_id == "abcdefabcdefabcdefabcdefabcdefab"));
        let disabled = DisabledNetworks::load();
        assert!(disabled.contains(allmystuff_protocol::LOCAL_CLAIM_NETWORK_ID));

        std::env::remove_var("MYOWNMESH_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }
}
