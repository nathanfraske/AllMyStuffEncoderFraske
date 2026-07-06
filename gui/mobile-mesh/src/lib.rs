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

use std::sync::Arc;

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
    ChannelError, Identity, JoinedNetwork, Mesh, MeshConfig, MeshHandle, NetworkConfig,
};
use serde_json::Value;
use tokio::runtime::Runtime;

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
    /// Kept alive for the node's lifetime: dropping the signaling drivers stops
    /// peer discovery. `None` when signaling wasn't attached (hermetic tests).
    _signaling: Option<SignalingDrivers>,
}

impl EngineMesh {
    /// Open the engine with a 32-byte ed25519 `seed` (from the platform
    /// keystore), join `network_id`, and attach signaling so peers are
    /// discovered. `label` is this device's display name. Every classified
    /// inbound frame off the five AllMyStuff channels is delivered to
    /// `on_inbound`.
    pub fn open_and_join(
        seed: [u8; 32],
        label: impl Into<String>,
        network_id: impl Into<String>,
        on_inbound: InboundSink,
    ) -> Result<Self, EngineError> {
        Self::open_inner(seed, label, network_id, on_inbound, true)
    }

    fn open_inner(
        seed: [u8; 32],
        label: impl Into<String>,
        network_id: impl Into<String>,
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
        let network_id = network_id.into();

        let (handle, net, signaling) = rt.block_on(async {
            let handle = Mesh::open_with_identity(MeshConfig::default(), identity)
                .await
                .map_err(|e| EngineError::Open(e.to_string()))?;
            // A phone joins a network whose config id and wire id are the same
            // fleet handle; defaults carry the reference STUN/TURN + signaling.
            let net = handle
                .join(NetworkConfig::from_network_id(
                    network_id.clone(),
                    network_id.clone(),
                ))
                .await
                .map_err(|e| EngineError::Join(e.to_string()))?;
            let signaling = if attach_sig {
                attach_signaling(&net.state())
            } else {
                None
            };
            Ok::<_, EngineError>((handle, net, signaling))
        })?;

        let device_id = handle.device_id();

        // One pump task per channel: inbound `{from, body}` → classify → sink.
        for chan in CHANNELS {
            let mut sub = net.channel::<Value>(chan).subscribe();
            let chan = chan.to_string();
            let sink = on_inbound.clone();
            rt.spawn(async move {
                loop {
                    match sub.recv().await {
                        // A frame this build understands (or an ignorable
                        // Unknown) — hand the typed value to the host.
                        Some(Ok(msg)) => {
                            if let Some(inbound) = classify(&chan, &msg.from, msg.body) {
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
            _signaling: signaling,
        })
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
        let mesh = EngineMesh::open_inner(
            [9u8; 32],
            "test-phone",
            "allmystuff-mesh-test-net",
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

        // No peers yet → empty snapshot.
        assert!(mesh.peers().is_empty());

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
