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
//!   from the iOS Keychain / Android Keystore) via `Mesh::open_with_identity`;
//! * it manages the phone's **networks-as-venues**: join/leave/reconnect any
//!   number of networks at runtime (the LAN mDNS rendezvous, a named venue, a
//!   fleet mesh later), each with its own signaling drivers, mirroring the
//!   daemon the desktop drives over its control socket;
//! * it maps the five AllMyStuff channels onto the engine's typed `Channel`
//!   API per network — `advertise` broadcasts presence everywhere,
//!   `send_control` / `send_media` route to the network the peer is connected
//!   on, `peers` snapshots the connected union;
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
    CHANNEL_ROOMS,
};
use ed25519_dalek::SigningKey;
use myownmesh_core::engine::attach_signaling;
use myownmesh_core::engine::connection::PeerStatus;
use myownmesh_core::engine::SignalingDrivers;
use myownmesh_core::{
    ChannelError, Identity, JoinedNetwork, Mesh, MeshConfig, MeshEvent, MeshHandle, PeerEvent,
};
use serde_json::{json, Value};
use tokio::runtime::Runtime;

// Re-exported for the shell (`gui/mobile`), which builds network configs and
// answers the frontend's management commands without depending on the engine
// crate directly.
pub use allmystuff_protocol::LOCAL_CLAIM_NETWORK_ID;
pub use myownmesh_core::{generate_network_id, NetworkConfig};

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

/// What can go wrong bringing the embedded engine up or managing networks.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// The tokio runtime couldn't be built.
    #[error("failed to build the mesh runtime: {0}")]
    Runtime(String),
    /// `Mesh::open_with_identity` failed (WebRTC stack, identity).
    #[error("failed to open the mesh: {0}")]
    Open(String),
    /// Joining a network failed.
    #[error("failed to join the network: {0}")]
    Join(String),
    /// A network id that matches nothing this node is joined to.
    #[error("unknown network: {0}")]
    UnknownNetwork(String),
    /// A join for a config id that's already joined — leave it first (or
    /// use update, which does).
    #[error("network already joined: {0}")]
    AlreadyJoined(String),
}

/// One joined network: the engine handle, its signaling drivers, the config it
/// was joined with (handed back for parking / settings panes), and the pump
/// tasks feeding its channels into the host sink.
struct NetEntry {
    net: JoinedNetwork,
    /// Kept alive for the network's lifetime: dropping the signaling drivers
    /// stops peer discovery. `None` when signaling wasn't attached (tests).
    _signaling: Option<SignalingDrivers>,
    /// The config as joined — `config.id` is this entry's key.
    config: NetworkConfig,
    pumps: Vec<tokio::task::JoinHandle<()>>,
}

/// A peer we've heard AllMyStuff presence from: which network it spoke on,
/// its serialized [`NodeProfile`], and the boot id it last advertised (the
/// event-driven gossip clock — see `NodeProfile::boot`).
struct PeerSeen {
    network: String,
    profile: Value,
    boot: u64,
}

/// The event-driven presence gossip: a boot id we haven't recorded for this
/// peer means it (re)started and missed our earlier adverts — answer with our
/// own presence directly. `0` = an older peer without the field.
fn boot_is_new(prev: Option<u64>, boot: u64) -> bool {
    boot != 0 && prev != Some(boot)
}

/// An embedded-engine mesh node, exposed to the phone as a [`MeshClient`] plus
/// the network-management surface the frontend's venue UI drives. Owns the
/// tokio runtime that drives the engine; dropping it tears the node down.
pub struct EngineMesh {
    rt: Arc<Runtime>,
    handle: MeshHandle,
    device_id: String,
    /// Joined networks by config id. `Arc` so the peer-connect introduction
    /// task can reach the presence channels without borrowing `self`.
    nets: Arc<Mutex<HashMap<String, NetEntry>>>,
    /// Peers heard via presence this session, across all networks. `Arc` so
    /// the per-network pump tasks hold it without borrowing `self`.
    roster: Arc<Mutex<HashMap<String, PeerSeen>>>,
    /// The last [`NodeProfile`] the host advertised, kept so the node can
    /// re-introduce itself on its own: to a peer that just connected, to a
    /// [`ControlMessage::ProfileRequest`], and to an unseen boot id. Without
    /// this the launch broadcast goes out once — to however many peers are
    /// connected at that instant, usually zero — and the phone stays mute at
    /// the AllMyStuff layer forever.
    self_profile: Arc<Mutex<Option<Value>>>,
    sink: InboundSink,
}

impl EngineMesh {
    /// Open the engine with a 32-byte ed25519 `seed` (from the platform
    /// keystore) and no networks joined yet. `label` is this device's display
    /// name. Every classified inbound frame off the five AllMyStuff channels —
    /// on any network joined later — is delivered to `on_inbound`.
    pub fn open(
        seed: [u8; 32],
        label: impl Into<String>,
        on_inbound: InboundSink,
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

        let handle = rt
            .block_on(Mesh::open_with_identity(MeshConfig::default(), identity))
            .map_err(|e| EngineError::Open(e.to_string()))?;
        let device_id = handle.device_id();

        let nets: Arc<Mutex<HashMap<String, NetEntry>>> = Arc::new(Mutex::new(HashMap::new()));
        let self_profile: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));

        // Introduce ourselves to every peer that connects: the launch
        // broadcast reaches only the peers connected at that instant (usually
        // none), so without this a later-arriving desktop sees a healthy mesh
        // peer that never speaks AllMyStuff. Mirrors the desktop node, which
        // sends its presence directly to each newly authenticated peer.
        {
            let mut events = handle.events();
            let nets = nets.clone();
            let self_profile = self_profile.clone();
            rt.spawn(async move {
                loop {
                    let ev = match events.recv().await {
                        Ok(ev) => ev,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => return,
                    };
                    let MeshEvent::Peer(peer_ev) = ev else {
                        continue;
                    };
                    let (network_id, peer) = match &peer_ev {
                        PeerEvent::Authenticated {
                            network_id,
                            device_id,
                            ..
                        }
                        | PeerEvent::Approved {
                            network_id,
                            device_id,
                            ..
                        }
                        | PeerEvent::Unshelved {
                            network_id,
                            device_id,
                            ..
                        } => (network_id.clone(), device_id.clone()),
                        _ => continue,
                    };
                    let Some(profile) = self_profile.lock().unwrap().clone() else {
                        continue;
                    };
                    let chan = {
                        let nets = nets.lock().unwrap();
                        nets.values()
                            .find(|e| e.net.network_id() == network_id)
                            .map(|e| e.net.channel::<Value>(CHANNEL_PRESENCE))
                    };
                    let Some(chan) = chan else { continue };
                    // Give the connection a beat to finish the approve
                    // handshake and reach Active before the directed send.
                    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
                    if let Err(e) = chan.send_to(&peer, &profile).await {
                        eprintln!("[mesh] presence intro to {peer} failed: {e}");
                    }
                }
            });
        }

        Ok(EngineMesh {
            rt,
            handle,
            device_id,
            nets,
            roster: Arc::new(Mutex::new(HashMap::new())),
            self_profile,
            sink: on_inbound,
        })
    }

    /// Open the engine and join the **LAN rendezvous**
    /// ([`lan_discovery_config`]) — the zero-configuration path a phone uses to
    /// discover peers on the same network over mDNS, with no fleet, account,
    /// or relay.
    pub fn open_lan(
        seed: [u8; 32],
        label: impl Into<String>,
        on_inbound: InboundSink,
    ) -> Result<Self, EngineError> {
        let mesh = Self::open(seed, label, on_inbound)?;
        mesh.join_network(lan_discovery_config())?;
        Ok(mesh)
    }

    /// Join `config` and attach its signaling drivers — the phone's
    /// counterpart to the daemon's `network_add`. The engine normalizes the
    /// wire-level `network_id`; `config.id` stays the caller's handle for
    /// [`leave_network`] / [`reconnect`].
    pub fn join_network(&self, config: NetworkConfig) -> Result<(), EngineError> {
        self.join_network_inner(config, true)
    }

    fn join_network_inner(
        &self,
        config: NetworkConfig,
        attach_sig: bool,
    ) -> Result<(), EngineError> {
        if self.nets.lock().unwrap().contains_key(&config.id) {
            return Err(EngineError::AlreadyJoined(config.id));
        }

        let (net, signaling) = self.rt.block_on(async {
            let net = self
                .handle
                .join(config.clone())
                .await
                .map_err(|e| EngineError::Join(e.to_string()))?;
            let signaling = if attach_sig {
                attach_signaling(&net.state())
            } else {
                None
            };
            Ok::<_, EngineError>((net, signaling))
        })?;

        // One pump task per channel: inbound `{from, body}` → classify → sink,
        // and presence adverts additionally land in the roster for snapshots.
        let mut pumps = Vec::new();
        for chan in CHANNELS {
            let mut sub = net.channel::<Value>(chan).subscribe();
            let chan = chan.to_string();
            let sink = self.sink.clone();
            let net_id = config.id.clone();
            let roster_map = self.roster.clone();
            let self_profile = self.self_profile.clone();
            // For the presence replies this pump owes: the gossip answer to an
            // unseen boot id, and the answer to a ProfileRequest. Sent from
            // inside the runtime (plain awaits — never the block_on wrappers).
            let reply_chan = net.channel::<Value>(CHANNEL_PRESENCE);
            pumps.push(self.rt.spawn(async move {
                loop {
                    match sub.recv().await {
                        // A frame this build understands (or an ignorable
                        // Unknown) — record presence, then hand it to the host.
                        Some(Ok(msg)) => {
                            if let Some(inbound) = classify(&chan, &msg.from, msg.body) {
                                let mut reply_to: Option<String> = None;
                                match &inbound {
                                    Inbound::Presence(profile) => {
                                        let peer = profile.node.as_str().to_string();
                                        let prev =
                                            roster_map.lock().unwrap().get(&peer).map(|s| s.boot);
                                        // Event-driven gossip: an unseen boot id
                                        // means the peer (re)started and missed
                                        // our adverts — answer directly, like
                                        // the desktop does.
                                        if boot_is_new(prev, profile.boot) {
                                            reply_to = Some(msg.from.clone());
                                        }
                                        if let Ok(value) = serde_json::to_value(profile.as_ref()) {
                                            roster_map.lock().unwrap().insert(
                                                peer,
                                                PeerSeen {
                                                    network: net_id.clone(),
                                                    profile: value,
                                                    boot: profile.boot,
                                                },
                                            );
                                        }
                                    }
                                    // "Re-send me your presence" — the peer's
                                    // per-node refresh. A viewer must answer or
                                    // it looks offline to anyone refreshing it.
                                    Inbound::Control {
                                        from,
                                        msg: ControlMessage::ProfileRequest,
                                    } => reply_to = Some(from.clone()),
                                    _ => {}
                                }
                                if let Some(to) = reply_to {
                                    let profile = self_profile.lock().unwrap().clone();
                                    if let Some(profile) = profile {
                                        if let Err(e) = reply_chan.send_to(&to, &profile).await {
                                            eprintln!("[mesh] presence reply to {to} failed: {e}");
                                        }
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
            }));
        }

        self.nets.lock().unwrap().insert(
            config.id.clone(),
            NetEntry {
                net,
                _signaling: signaling,
                config,
                pumps,
            },
        );
        Ok(())
    }

    /// Leave a network — announce the departure (so peers get a prompt
    /// goodbye), tear the sessions down, and stop its pumps. `network` may be
    /// the config id or the wire network id, like the daemon's `network_remove`.
    pub fn leave_network(&self, network: &str) -> Result<(), EngineError> {
        let key = self
            .resolve_network(network)
            .ok_or_else(|| EngineError::UnknownNetwork(network.to_string()))?;
        let entry = self.nets.lock().unwrap().remove(&key);
        let Some(entry) = entry else {
            return Err(EngineError::UnknownNetwork(network.to_string()));
        };
        // Drop this network's peers from the presence roster — they're only
        // reachable through the network we're leaving.
        self.roster
            .lock()
            .unwrap()
            .retain(|_, seen| seen.network != key);
        self.rt.block_on(async {
            entry.net.announce_leave().await;
            let _ = entry.net.leave().await;
        });
        for pump in entry.pumps {
            pump.abort();
        }
        Ok(())
    }

    /// Reconnect in place — redial signaling and renegotiate ICE without
    /// leaving. `network` picks one network (else all); `peer` narrows to one
    /// peer. Mirrors the daemon's `network_reconnect`.
    pub fn reconnect(&self, network: Option<&str>, peer: Option<&str>) -> Result<(), EngineError> {
        let nets = self.nets.lock().unwrap();
        match network {
            Some(n) => {
                let key = self
                    .resolve_network_locked(&nets, n)
                    .ok_or_else(|| EngineError::UnknownNetwork(n.to_string()))?;
                if let Some(entry) = nets.get(&key) {
                    entry.net.reconnect(peer.map(str::to_string));
                }
            }
            None => {
                for entry in nets.values() {
                    // With a peer: only the network that can reach it needs
                    // the redial. Without: every joined network.
                    if let Some(p) = peer {
                        if !entry.net.peers().iter().any(|info| info.device_id == p) {
                            continue;
                        }
                    }
                    entry.net.reconnect(peer.map(str::to_string));
                }
            }
        }
        Ok(())
    }

    /// Rename this device. Updates the engine identity (peers' rosters and
    /// approval UIs read it) — the caller re-broadcasts AllMyStuff presence
    /// with the new label and persists it.
    pub fn set_label(&self, label: &str) {
        self.handle.identity().set_label(label);
    }

    /// This device's mesh id (bare ed25519 pubkey).
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// The id peers' UIs display — pubkey plus the 5-char verification suffix.
    pub fn display_id(&self) -> String {
        self.handle.identity().display_id()
    }

    /// This device's display name.
    pub fn label(&self) -> String {
        self.handle.identity().label()
    }

    /// The underlying device handle, for callers that want the engine's own
    /// event stream ([`MeshHandle::events`]).
    pub fn handle(&self) -> &MeshHandle {
        &self.handle
    }

    /// Is `network` (config id or wire id) currently joined?
    pub fn has_network(&self, network: &str) -> bool {
        self.resolve_network(network).is_some()
    }

    /// The configs of every joined network — what the parking store hands
    /// back to the daemon-shaped `config_show`.
    pub fn network_configs(&self) -> Vec<NetworkConfig> {
        self.nets
            .lock()
            .unwrap()
            .values()
            .map(|e| e.config.clone())
            .collect()
    }

    fn resolve_network(&self, network: &str) -> Option<String> {
        let nets = self.nets.lock().unwrap();
        self.resolve_network_locked(&nets, network)
    }

    fn resolve_network_locked(
        &self,
        nets: &HashMap<String, NetEntry>,
        network: &str,
    ) -> Option<String> {
        if nets.contains_key(network) {
            return Some(network.to_string());
        }
        nets.iter()
            .find(|(_, e)| e.net.network_id() == network)
            .map(|(k, _)| k.clone())
    }

    // ---- the frontend-contract command bodies ------------------------------

    /// The peers to render on the graph: every currently-connected peer we've
    /// also heard a presence advert from, as its serialized [`NodeProfile`],
    /// across all joined networks.
    pub fn snapshot_peers(&self) -> Vec<Value> {
        let roster = self.roster.lock().unwrap();
        let nets = self.nets.lock().unwrap();
        let mut out = Vec::new();
        for (peer, seen) in roster.iter() {
            let connected = nets.values().any(|e| {
                e.net
                    .peers()
                    .iter()
                    .any(|p| p.device_id == *peer && connected_status(p.status))
            });
            if connected {
                out.push(seen.profile.clone());
            }
        }
        out
    }

    /// The body of the frontend's **`session_snapshot`** command:
    /// `{ready, me, network, peers, routes, shares}`.
    pub fn session_snapshot(&self) -> Value {
        let network = {
            let nets = self.nets.lock().unwrap();
            let mut ids: Vec<&String> = nets.keys().collect();
            ids.sort();
            ids.first().map(|s| s.to_string())
        };
        json!({
            "ready": true,
            "me": self.device_id,
            "network": network,
            "peers": self.snapshot_peers(),
            "routes": [],
            "shares": [],
        })
    }

    /// The body of the frontend's **`mesh_networks`** command — every joined
    /// network, in the daemon's `NetworkSummary` shape.
    pub fn networks(&self) -> Value {
        let nets = self.nets.lock().unwrap();
        let mut list: Vec<Value> = nets
            .values()
            .map(|e| {
                json!({
                    "config_id": e.net.config_id(),
                    "network_id": e.net.network_id(),
                    "label": e.net.label(),
                    "phase": serde_json::to_value(e.net.current_phase()).unwrap_or(Value::Null),
                    "topology": serde_json::to_value(e.net.current_topology()).unwrap_or(Value::Null),
                })
            })
            .collect();
        list.sort_by(|a, b| {
            a["config_id"]
                .as_str()
                .unwrap_or("")
                .cmp(b["config_id"].as_str().unwrap_or(""))
        });
        json!({ "networks": list })
    }

    /// The body of the frontend's **`mesh_peers`** command — the connected
    /// peers of one network (config id or wire id; `None` = union across all),
    /// with their capability adverts: the liveness feed that marks a peer
    /// online + on-AllMyStuff.
    pub fn mesh_peers(&self, network: Option<&str>) -> Value {
        let nets = self.nets.lock().unwrap();
        let mut peers: Vec<Value> = Vec::new();
        let key = network.and_then(|n| self.resolve_network_locked(&nets, n));
        for (id, entry) in nets.iter() {
            if let Some(k) = &key {
                if id != k {
                    continue;
                }
            }
            peers.extend(
                entry
                    .net
                    .peers()
                    .iter()
                    .filter_map(|p| serde_json::to_value(p).ok()),
            );
        }
        json!({ "peers": peers })
    }
}

/// "Connected and passing app traffic" — Active; Shelved is a
/// connected-but-demoted heartbeat peer we can still reach.
fn connected_status(status: PeerStatus) -> bool {
    matches!(status, PeerStatus::Active | PeerStatus::Shelved)
}

impl MeshClient for EngineMesh {
    fn device_id(&self) -> String {
        self.device_id.clone()
    }

    fn advertise(&self, profile: &NodeProfile) -> MeshResult<()> {
        let payload = serde_json::to_value(profile)?;
        // Remember it: the engine re-introduces this profile on its own — to
        // newly connected peers, ProfileRequests, and unseen boot ids.
        *self.self_profile.lock().unwrap() = Some(payload.clone());
        let channels: Vec<_> = {
            let nets = self.nets.lock().unwrap();
            nets.values()
                .map(|e| e.net.channel::<Value>(CHANNEL_PRESENCE))
                .collect()
        };
        if channels.is_empty() {
            return Err(MeshError::NotConnected);
        }
        self.rt.block_on(async move {
            for chan in channels {
                let _ = chan.broadcast(&payload).await;
            }
        });
        Ok(())
    }

    fn peers(&self) -> Vec<String> {
        let nets = self.nets.lock().unwrap();
        let mut out: Vec<String> = Vec::new();
        for entry in nets.values() {
            for p in entry.net.peers() {
                if connected_status(p.status) && !out.contains(&p.device_id) {
                    out.push(p.device_id);
                }
            }
        }
        out
    }

    fn send_control(&self, peer: &str, msg: &ControlMessage) -> MeshResult<()> {
        let payload = serde_json::to_value(msg)?;
        self.send_routed(CHANNEL_CONTROL, peer, payload)
    }

    fn send_media(&self, peer: &str, payload: &Value) -> MeshResult<()> {
        self.send_routed(CHANNEL_MEDIA, peer, payload.clone())
    }
}

impl EngineMesh {
    /// Send one frame to `peer` on `channel`, over whichever joined network
    /// has it connected (falling back to the network its presence arrived on).
    fn send_routed(&self, channel: &str, peer: &str, payload: Value) -> MeshResult<()> {
        let chan = {
            let nets = self.nets.lock().unwrap();
            let by_connection = nets.values().find(|e| {
                e.net
                    .peers()
                    .iter()
                    .any(|p| p.device_id == peer && connected_status(p.status))
            });
            let entry = by_connection.or_else(|| {
                let roster = self.roster.lock().unwrap();
                roster.get(peer).and_then(|seen| nets.get(&seen.network))
            });
            match entry {
                Some(e) => e.net.channel::<Value>(channel),
                None => return Err(MeshError::NoSuchPeer(peer.to_string())),
            }
        };
        let peer = peer.to_string();
        self.rt
            .block_on(async move { chan.send_to(&peer, &payload).await })
            .map_err(map_channel_err)
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

    fn hermetic(seed: u8) -> EngineMesh {
        let mesh = EngineMesh::open([seed; 32], "test-phone", noop_sink()).expect("open");
        mesh.join_network_inner(lan_discovery_config(), false)
            .expect("join lan");
        mesh
    }

    /// Opening the engine with an injected seed and joining a network yields a
    /// working `MeshClient`: its device id derives from the seed (not a disk
    /// anchor), and with no peers connected the roster is empty and a targeted
    /// send fails with the typed `NoSuchPeer` rather than panicking. Signaling
    /// is left off so the test is hermetic (no relay/network access).
    #[test]
    fn open_join_exposes_a_working_mesh_client() {
        let mesh = hermetic(9);

        // Device id is deterministic from the injected key.
        let want = Identity::from_signing_key(SigningKey::from_bytes(&[9u8; 32]), "")
            .public_id()
            .to_string();
        assert_eq!(MeshClient::device_id(&mesh), want);
        assert_eq!(mesh.device_id(), want);
        // The display id is the pubkey plus a suffix.
        assert!(mesh.display_id().starts_with(&want));
        assert_eq!(mesh.label(), "test-phone");

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
    fn join_leave_and_rename_manage_the_network_set() {
        let mesh = hermetic(3);
        assert_eq!(mesh.networks()["networks"].as_array().unwrap().len(), 1);

        // Join a second network (a named venue), signaling off.
        let venue: NetworkConfig = serde_json::from_value(json!({
            "id": "venue-1",
            "network_id": "my-test-venue",
            "label": "Test venue",
            "signaling": { "strategy": "none", "mdns": false },
            "stun_servers": [],
            "turn_servers": [],
        }))
        .unwrap();
        mesh.join_network_inner(venue.clone(), false)
            .expect("join venue");
        let nets = mesh.networks();
        assert_eq!(nets["networks"].as_array().unwrap().len(), 2);
        // Daemon summary shape: config_id + network_id + label + phase.
        assert!(nets["networks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["config_id"] == "venue-1" && n["label"] == "Test venue"));

        // Duplicate join is refused, like the daemon's network_add.
        assert!(matches!(
            mesh.join_network_inner(venue, false),
            Err(EngineError::AlreadyJoined(_))
        ));

        // mesh_peers scoped to one network answers, and to an unknown one is
        // just empty (the poll must never error the UI loop).
        assert!(mesh.mesh_peers(Some("venue-1"))["peers"]
            .as_array()
            .unwrap()
            .is_empty());

        // Leave by wire id (the daemon accepts either handle).
        mesh.leave_network("my-test-venue").expect("leave venue");
        assert_eq!(mesh.networks()["networks"].as_array().unwrap().len(), 1);
        assert!(matches!(
            mesh.leave_network("my-test-venue"),
            Err(EngineError::UnknownNetwork(_))
        ));

        // Rename: the engine identity reflects it immediately.
        mesh.set_label("Chris's iPhone");
        assert_eq!(mesh.label(), "Chris's iPhone");

        // Reconnect is fire-and-forget and tolerant of a peer filter.
        mesh.reconnect(Some(LOCAL_CLAIM_NETWORK_ID), None)
            .expect("reconnect lan");
        assert!(matches!(
            mesh.reconnect(Some("nope"), None),
            Err(EngineError::UnknownNetwork(_))
        ));
    }

    #[test]
    fn command_bodies_match_the_frontend_contract() {
        let mesh = hermetic(5);

        // session_snapshot: ready, self id, a network, and an (empty) peers array.
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
        assert!(mesh.mesh_peers(None)["peers"]
            .as_array()
            .unwrap()
            .is_empty());
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
