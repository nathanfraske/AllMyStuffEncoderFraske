//! # allmystuff-mesh
//!
//! Backs [`allmystuff_mobile_core`]'s `MeshClient` seam with an embedded
//! [`myownmesh_core`] engine.
//!
//! On the desktop the app spawns `myownmesh serve` and talks to it over a local
//! socket. iOS forbids a sandboxed app from spawning that child process, so on
//! the phone the app **links the engine in-process** and becomes a first-class
//! peer — its own ed25519 identity, direct WebRTC to its peers, no central
//! server. This crate is the bridge between the two halves:
//!
//! * it opens the engine with a **caller-supplied identity** (the phone's key,
//!   from the iOS Keychain / Android Keystore) via `Mesh::open_with_identity`,
//!   joins one network, and attaches signaling so peers are discovered;
//! * it maps the five AllMyStuff channels onto the engine's typed `Channel`
//!   API — `advertise` broadcasts presence, `send_control` / `send_media`
//!   publish to one peer, `peers` snapshots the connected set;
//! * it pumps every inbound frame off those channels through
//!   [`allmystuff_mobile_core::classify`] into the host's [`InboundSink`].
//!
//! Everything `allmystuff-mobile-core` builds (offers, control messages, the
//! media planes) rides this seam, so the same tested wire logic that ran
//! against an in-memory fake now runs against the real radio, unchanged.
//!
//! The engine is async; [`EngineMesh`] owns a multi-thread tokio runtime and
//! bridges to the synchronous `MeshClient` surface with `block_on`. The
//! `MeshClient` methods must therefore be called from outside the runtime
//! (they are — a Tauri command thread), never from within [`InboundSink`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use allmystuff_mobile_core::{classify, Inbound, MeshClient, MeshError, MeshResult};
use allmystuff_protocol::{
    ControlMessage, NodeProfile, CHANNEL_CONTROL, CHANNEL_MEDIA, CHANNEL_OWNED, CHANNEL_PRESENCE,
    CHANNEL_ROOMS, LOCAL_CLAIM_NETWORK_ID,
};
use ed25519_dalek::SigningKey;
use myownmesh_core::engine::attach_signaling;
use myownmesh_core::engine::connection::PeerStatus;
use myownmesh_core::engine::SignalingDrivers;
use myownmesh_core::{
    CapabilityAdvert, ChannelError, Identity, JoinedNetwork, Mesh, MeshConfig, MeshHandle,
    NetworkConfig,
};
use serde_json::{json, Value};
use tokio::runtime::Runtime;

/// A live roster of the peers we've heard presence from this session, keyed by
/// device id, each value the peer's serialized [`NodeProfile`]. Filled by the
/// presence pump; read back (intersected with the connected set) as the
/// `session_snapshot` peers the shared frontend renders.
type Roster = Arc<Mutex<HashMap<String, Value>>>;

/// The always-on **LAN rendezvous** network config — mDNS/DNS-SD only, no
/// remote signaling, no STUN/TURN — byte-for-byte the desktop's local-claim
/// network (see `node`'s `ensure_claim_networks`). Peers on the same LAN
/// discover each other here with **zero configuration and zero infrastructure**:
/// no fleet, no account, no relay. Built as JSON (like the desktop's
/// `NetworkAdd`) so it rides `NetworkConfig`'s `#[serde(default)]` and doesn't
/// need the engine's private `SignalingConfig` type.
pub fn lan_discovery_config() -> NetworkConfig {
    serde_json::from_value(json!({
        "id": LOCAL_CLAIM_NETWORK_ID,
        "network_id": LOCAL_CLAIM_NETWORK_ID,
        "label": "Local (this LAN)",
        "kind": "open",
        "auto_approve": true,
        "signaling": { "strategy": "none", "mdns": true },
        "stun_servers": [],
        "turn_servers": [],
    }))
    .expect("the local-claim network config is a valid NetworkConfig")
}

/// The AllMyStuff channels the phone subscribes to for inbound traffic — the
/// same set `allmystuff-mobile-core::classify` recognises.
const CHANNELS: &[&str] = &[
    CHANNEL_PRESENCE,
    CHANNEL_CONTROL,
    CHANNEL_MEDIA,
    CHANNEL_OWNED,
    CHANNEL_ROOMS,
];

/// A sink the host installs to receive inbound mesh traffic, already typed by
/// [`classify`]. Invoked from the engine's runtime threads, so keep it cheap
/// and non-blocking — hand off to a queue or emit a UI event and return. It
/// must **not** call back into [`MeshClient`] methods (those `block_on` the
/// runtime and would deadlock/panic from within it).
pub type InboundSink = Arc<dyn Fn(Inbound) + Send + Sync>;

/// What can go wrong bringing the embedded engine up.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// The tokio runtime couldn't be built.
    #[error("failed to build the mesh runtime: {0}")]
    Runtime(String),
    /// `Mesh::open_with_identity` failed (WebRTC stack, identity).
    #[error("failed to open the mesh: {0}")]
    Open(String),
    /// Joining the network failed.
    #[error("failed to join the network: {0}")]
    Join(String),
}

/// An embedded-engine mesh node joined to one network, exposed to the phone as
/// a [`MeshClient`]. Owns the tokio runtime that drives the engine; dropping it
/// tears the node down.
pub struct EngineMesh {
    rt: Arc<Runtime>,
    /// Kept alive so the shared WebRTC stack / identity outlive their networks.
    handle: MeshHandle,
    net: JoinedNetwork,
    device_id: String,
    /// Peers heard via presence this session (device id → serialized profile),
    /// filled by the pump and read back by [`EngineMesh::snapshot_peers`].
    roster: Roster,
    /// Kept alive for the node's lifetime: dropping the signaling drivers stops
    /// peer discovery. `None` when signaling wasn't attached (hermetic tests).
    _signaling: Option<SignalingDrivers>,
}

impl EngineMesh {
    /// Open the engine with a 32-byte ed25519 `seed` (from the platform
    /// keystore) and join the **LAN rendezvous** ([`lan_discovery_config`]) —
    /// the zero-configuration path a phone uses to discover peers on the same
    /// network over mDNS, with no fleet, account, or relay. `label` is this
    /// device's display name; every classified inbound frame off the five
    /// AllMyStuff channels is delivered to `on_inbound`.
    pub fn open_lan(
        seed: [u8; 32],
        label: impl Into<String>,
        on_inbound: InboundSink,
    ) -> Result<Self, EngineError> {
        Self::open_inner(seed, label, lan_discovery_config(), on_inbound, true)
    }

    /// Open the engine and join an explicit `network` (e.g. a fleet's closed
    /// network once paired). Use [`open_lan`](Self::open_lan) for plain LAN
    /// discovery.
    pub fn open_and_join(
        seed: [u8; 32],
        label: impl Into<String>,
        network: NetworkConfig,
        on_inbound: InboundSink,
    ) -> Result<Self, EngineError> {
        Self::open_inner(seed, label, network, on_inbound, true)
    }

    fn open_inner(
        seed: [u8; 32],
        label: impl Into<String>,
        network: NetworkConfig,
        on_inbound: InboundSink,
        attach_sig: bool,
    ) -> Result<Self, EngineError> {
        let rt = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|e| EngineError::Runtime(e.to_string()))?,
        );

        // The phone owns its key: build the identity from the injected seed
        // rather than the on-disk anchor the desktop uses.
        let identity = Arc::new(Identity::from_signing_key(
            SigningKey::from_bytes(&seed),
            label,
        ));

        let (handle, net, signaling) = rt.block_on(async {
            let handle = Mesh::open_with_identity(MeshConfig::default(), identity)
                .await
                .map_err(|e| EngineError::Open(e.to_string()))?;
            let net = handle
                .join(network)
                .await
                .map_err(|e| EngineError::Join(e.to_string()))?;
            // Tag ourselves on the *myownmesh* capability layer (distinct from
            // our AllMyStuff `NodeProfile` presence): peers' liveness poll reads
            // this to mark us online + "on AllMyStuff", so the phone shows up as
            // a real node on their graph rather than an anonymous mesh peer.
            // Must run inside the runtime context (it drives the RPC engine).
            net.advertise(CapabilityAdvert {
                tags: vec!["allmystuff".to_string()],
                app_version: Some(env!("CARGO_PKG_VERSION").to_string()),
                max_connections: None,
                extra: json!({}),
            });
            let signaling = if attach_sig {
                attach_signaling(&net.state())
            } else {
                None
            };
            Ok::<_, EngineError>((handle, net, signaling))
        })?;

        let device_id = handle.device_id();
        let roster: Roster = Arc::new(Mutex::new(HashMap::new()));

        // One pump task per channel: inbound `{from, body}` → classify → sink,
        // and presence adverts additionally land in the roster for snapshots.
        for chan in CHANNELS {
            let mut sub = net.channel::<Value>(chan).subscribe();
            let chan = chan.to_string();
            let sink = on_inbound.clone();
            let roster = roster.clone();
            rt.spawn(async move {
                loop {
                    match sub.recv().await {
                        // A frame this build understands (or an ignorable
                        // Unknown) — record presence, then hand it to the host.
                        Some(Ok(msg)) => {
                            if let Some(inbound) = classify(&chan, &msg.from, msg.body) {
                                if let Inbound::Presence(profile) = &inbound {
                                    if let Ok(value) = serde_json::to_value(profile.as_ref()) {
                                        roster
                                            .lock()
                                            .unwrap()
                                            .insert(profile.node.as_str().to_string(), value);
                                    }
                                }
                                sink(inbound);
                            }
                        }
                        // An undecodable frame: skip it, keep the stream — a
                        // newer peer can't knock the phone off the channel.
                        Some(Err(_)) => continue,
                        // The channel (network) was torn down: stop pumping.
                        None => break,
                    }
                }
            });
        }

        Ok(EngineMesh {
            rt,
            handle,
            net,
            device_id,
            roster,
            _signaling: signaling,
        })
    }

    /// The peers to render on the graph: every currently-connected peer we've
    /// also heard a presence advert from, as its serialized [`NodeProfile`].
    /// This is the `peers` array the shared frontend's `session_snapshot`
    /// expects. Intersecting the connected set with the roster drops peers that
    /// have gone offline since we last heard them.
    pub fn snapshot_peers(&self) -> Vec<Value> {
        let roster = self.roster.lock().unwrap();
        self.peers()
            .into_iter()
            .filter_map(|id| roster.get(&id).cloned())
            .collect()
    }

    /// The body of the frontend's **`session_snapshot`** command:
    /// `{ready, me, network, peers, routes, shares}`. `peers` are the serialized
    /// [`NodeProfile`]s from [`snapshot_peers`](Self::snapshot_peers); routes and
    /// shares are empty until those planes are wired.
    pub fn session_snapshot(&self) -> Value {
        json!({
            "ready": true,
            "me": self.device_id,
            "network": self.net.network_id(),
            "peers": self.snapshot_peers(),
            "routes": [],
            "shares": [],
        })
    }

    /// The body of the frontend's **`mesh_networks`** command — the single
    /// network this phone is on.
    pub fn networks(&self) -> Value {
        let phase = serde_json::to_value(self.net.current_phase()).unwrap_or(Value::Null);
        json!({ "networks": [{
            "config_id": self.net.config_id(),
            "network_id": self.net.network_id(),
            "label": self.net.label(),
            "phase": phase,
        }]})
    }

    /// The body of the frontend's **`mesh_peers`** command — the connected peers
    /// with their capability adverts (`status`, `tags`, `app_version`), which is
    /// the liveness feed that marks a peer online + on-AllMyStuff. The engine's
    /// `PeerInfo` serializes with the `device_id` / `label` / `status` /
    /// `capabilities` fields the frontend reads (a superset is harmless).
    pub fn mesh_peers(&self) -> Value {
        let peers: Vec<Value> = self
            .net
            .peers()
            .iter()
            .filter_map(|p| serde_json::to_value(p).ok())
            .collect();
        json!({ "peers": peers })
    }

    /// This device's mesh id (bare ed25519 pubkey).
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// The underlying device handle, for callers that want the engine's own
    /// event stream ([`MeshHandle::events`]) or to join further networks.
    pub fn handle(&self) -> &MeshHandle {
        &self.handle
    }

    fn broadcast(&self, channel: &str, payload: Value) -> MeshResult<()> {
        let chan = self.net.channel::<Value>(channel);
        self.rt
            .block_on(async move { chan.broadcast(&payload).await })
            .map(|_dispatched| ())
            .map_err(map_channel_err)
    }

    fn send_one(&self, channel: &str, peer: &str, payload: Value) -> MeshResult<()> {
        let chan = self.net.channel::<Value>(channel);
        let peer = peer.to_string();
        self.rt
            .block_on(async move { chan.send_to(&peer, &payload).await })
            .map_err(map_channel_err)
    }
}

impl MeshClient for EngineMesh {
    fn device_id(&self) -> String {
        self.device_id.clone()
    }

    fn advertise(&self, profile: &NodeProfile) -> MeshResult<()> {
        let payload = serde_json::to_value(profile)?;
        self.broadcast(CHANNEL_PRESENCE, payload)
    }

    fn peers(&self) -> Vec<String> {
        self.net
            .peers()
            .into_iter()
            // "Connected and passing app traffic" = Active; Shelved is a
            // connected-but-demoted heartbeat peer we can still reach.
            .filter(|p| matches!(p.status, PeerStatus::Active | PeerStatus::Shelved))
            .map(|p| p.device_id)
            .collect()
    }

    fn send_control(&self, peer: &str, msg: &ControlMessage) -> MeshResult<()> {
        let payload = serde_json::to_value(msg)?;
        self.send_one(CHANNEL_CONTROL, peer, payload)
    }

    fn send_media(&self, peer: &str, payload: &Value) -> MeshResult<()> {
        self.send_one(CHANNEL_MEDIA, peer, payload.clone())
    }
}

/// Map the engine's channel error onto the seam's `MeshError`, so the phone's
/// UI sees the same typed failures whether it runs against the real radio or
/// the in-memory test fake.
fn map_channel_err(e: ChannelError) -> MeshError {
    match e {
        ChannelError::NetworkDown => MeshError::NotConnected,
        ChannelError::PeerNotFound(p) => MeshError::NoSuchPeer(p),
        other => MeshError::Send(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noop_sink() -> InboundSink {
        Arc::new(|_inbound| {})
    }

    /// Opening the engine with an injected seed and joining a network yields a
    /// working `MeshClient`: its device id derives from the seed (not a disk
    /// anchor), and with no peers connected the roster is empty and a targeted
    /// send fails with the typed `NoSuchPeer` rather than panicking. Signaling
    /// is left off so the test is hermetic (no relay/network access).
    #[test]
    fn open_join_exposes_a_working_mesh_client() {
        // Join the LAN rendezvous config, but with signaling off so the test is
        // hermetic (no mDNS traffic, no relay/network access).
        let mesh = EngineMesh::open_inner(
            [9u8; 32],
            "test-phone",
            lan_discovery_config(),
            noop_sink(),
            false,
        )
        .expect("open + join");

        // Device id is deterministic from the injected key.
        let want = Identity::from_signing_key(SigningKey::from_bytes(&[9u8; 32]), "")
            .public_id()
            .to_string();
        assert_eq!(MeshClient::device_id(&mesh), want);
        assert_eq!(mesh.device_id(), want);

        // No peers yet → empty connected set and empty snapshot.
        assert!(mesh.peers().is_empty());
        assert!(mesh.snapshot_peers().is_empty());

        // A send to an unknown peer is a typed error, not a panic.
        let err = mesh
            .send_media("nobody", &serde_json::json!({"t": "term"}))
            .unwrap_err();
        assert!(matches!(
            err,
            MeshError::NoSuchPeer(_) | MeshError::NotConnected
        ));
    }

    #[test]
    fn command_bodies_match_the_frontend_contract() {
        let mesh = EngineMesh::open_inner(
            [3u8; 32],
            "test-phone",
            lan_discovery_config(),
            noop_sink(),
            false,
        )
        .expect("open + join");

        // session_snapshot: ready, self id, the network, and an (empty) peers array.
        let snap = mesh.session_snapshot();
        assert_eq!(snap["ready"], true);
        assert_eq!(snap["me"], mesh.device_id());
        assert_eq!(snap["network"], LOCAL_CLAIM_NETWORK_ID);
        assert!(snap["peers"].as_array().unwrap().is_empty());
        assert!(snap["routes"].is_array());

        // mesh_networks: the one LAN network, keyed as the frontend expects.
        let nets = mesh.networks();
        assert_eq!(nets["networks"][0]["network_id"], LOCAL_CLAIM_NETWORK_ID);
        assert!(nets["networks"][0]["config_id"].is_string());

        // mesh_peers: an array (empty with no peers connected).
        assert!(mesh.mesh_peers()["peers"].as_array().unwrap().is_empty());
    }

    #[test]
    fn lan_discovery_config_is_mdns_only_no_infrastructure() {
        let cfg = lan_discovery_config();
        assert_eq!(cfg.network_id, LOCAL_CLAIM_NETWORK_ID);
        assert_eq!(cfg.signaling.strategy, "none");
        assert!(cfg.signaling.mdns);
        assert!(cfg.stun_servers.is_empty());
        assert!(cfg.turn_servers.is_empty());
        assert!(cfg.auto_approve);
    }

    #[test]
    fn channel_error_maps_to_the_seam_error() {
        assert!(matches!(
            map_channel_err(ChannelError::NetworkDown),
            MeshError::NotConnected
        ));
        assert!(matches!(
            map_channel_err(ChannelError::PeerNotFound("p".into())),
            MeshError::NoSuchPeer(p) if p == "p"
        ));
        assert!(matches!(
            map_channel_err(ChannelError::Transport("boom".into())),
            MeshError::Send(_)
        ));
    }
}
