//! The live mesh: wires the daemon's typed channels to the
//! [`allmystuff_session::Session`] state machine and the [`AudioBridge`].
//!
//! On start it subscribes to the AllMyStuff presence / control / media
//! channels on the active network, broadcasts this node's
//! [`NodeProfile`], and pumps inbound frames:
//!
//!  * **presence** → updates the peer set (the graph fills with real peers).
//!  * **control** → drives the route handshake; the [`Effect`]s it returns
//!    send replies and start/stop audio.
//!  * **media** → audio frames fed to the playback side of active routes.
//!
//! Everything the front-end sees comes through `allmystuff://session`
//! snapshots emitted after each change.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;

use allmystuff_graph::{MediaKind, NodeId, Route};
use allmystuff_protocol::{
    ClientId, ControlMessage, NodeProfile, OwnershipControl, Request, RouteControl, CHANNEL_CONTROL,
    CHANNEL_MEDIA, CHANNEL_PRESENCE, PROTOCOL_VERSION,
};
use allmystuff_session::{AudioFrame, Effect, Session};

use crate::audio::AudioBridge;
use crate::control_client::ControlClient;
use crate::ownership::Ownership;

pub struct Mesh {
    client: Arc<ControlClient>,
    app: AppHandle,
    audio: Arc<AudioBridge>,
    state: Mutex<State>,
    /// This device's persisted ownership record — who owns it and whether
    /// it's currently offering itself for adoption (claim mode).
    ownership: Arc<Ownership>,
    /// Outbound audio: capture callbacks push `(peer, frame)`; a forwarder
    /// task sends them on the media channel.
    audio_out: mpsc::UnboundedSender<(String, AudioFrame)>,
}

struct State {
    session: Option<Session>,
    /// Primary network — where route control/media operate.
    network: Option<String>,
    /// Every joined network. Presence is broadcast on all of them so peers
    /// find each other regardless of which network the daemon lists first.
    networks: Vec<String>,
    client_id: Option<ClientId>,
    profile: Option<NodeProfile>,
}

impl Mesh {
    pub fn new(client: Arc<ControlClient>, app: AppHandle) -> Arc<Self> {
        let (audio_out, mut audio_rx) = mpsc::unbounded_channel::<(String, AudioFrame)>();
        let mesh = Arc::new(Mesh {
            client: client.clone(),
            app,
            audio: Arc::new(AudioBridge::new()),
            state: Mutex::new(State {
                session: None,
                network: None,
                networks: Vec::new(),
                client_id: None,
                profile: None,
            }),
            ownership: Arc::new(Ownership::load()),
            audio_out,
        });

        // Forwarder: drain captured frames out to peers on the media channel.
        {
            let mesh = mesh.clone();
            tauri::async_runtime::spawn(async move {
                while let Some((peer, frame)) = audio_rx.recv().await {
                    let Some(network) = mesh.network() else { continue };
                    let payload = match serde_json::to_value(&frame) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    let _ = mesh
                        .client
                        .request(&Request::ChannelSendTo {
                            network,
                            channel: CHANNEL_MEDIA.to_string(),
                            peer,
                            payload,
                        })
                        .await;
                }
            });
        }
        mesh
    }

    fn network(&self) -> Option<String> {
        self.state.lock().network.clone()
    }

    /// This node's mesh id once known (the daemon device id), else `None`.
    pub fn local_node_id(&self) -> Option<String> {
        self.state.lock().session.as_ref().map(|s| s.me().to_string())
    }

    /// Bring the session online: identify, pick a network, subscribe, and
    /// start pumping events. Safe to call once the daemon socket is up.
    pub async fn start(self: Arc<Self>) {
        let (tx, mut rx) = mpsc::channel::<Value>(512);
        let client_id = match self.client.subscribe_events(tx).await {
            Ok(id) => id,
            Err(e) => {
                tracing::warn!("mesh: event subscribe failed: {e}");
                self.emit_status("disconnected", Some(&e.to_string()));
                return;
            }
        };

        // Identity → our node id + presence profile. The label is the
        // user's optional override; `build_profile` falls back to the
        // hostname when it's unset.
        let me = self.fetch_identity().await.unwrap_or_else(|| NodeId::this().to_string());
        let label = self.fetch_identity_label().await;
        let profile = self.build_profile(&me, label);
        // Every joined network; route control/media operate on the primary.
        let networks = self.fetch_networks().await;
        let primary = networks.first().cloned();

        {
            let mut st = self.state.lock();
            st.client_id = Some(client_id);
            st.session = Some(Session::new(me.clone()));
            st.profile = Some(profile.clone());
            st.network = primary.clone();
            st.networks = networks.clone();
        }

        if networks.is_empty() {
            self.emit_status("no_network", None);
        } else {
            // Presence on *every* network so two machines discover each other
            // no matter which network the daemon lists first.
            for network in &networks {
                let _ = self
                    .client
                    .request(&Request::ChannelSubscribe {
                        client_id,
                        network: network.clone(),
                        channel: CHANNEL_PRESENCE.to_string(),
                    })
                    .await;
            }
            // Route control + media ride the primary network.
            if let Some(primary) = &primary {
                for channel in [CHANNEL_CONTROL, CHANNEL_MEDIA] {
                    let _ = self
                        .client
                        .request(&Request::ChannelSubscribe {
                            client_id,
                            network: primary.clone(),
                            channel: channel.to_string(),
                        })
                        .await;
                }
            }
            self.broadcast_presence().await;
            self.emit_status("live", None);
        }

        // Periodic presence re-broadcast so late joiners see us.
        {
            let mesh = self.clone();
            tauri::async_runtime::spawn(async move {
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(20));
                loop {
                    tick.tick().await;
                    mesh.broadcast_presence().await;
                }
            });
        }

        // Event loop.
        let mesh = self.clone();
        tauri::async_runtime::spawn(async move {
            while let Some(value) = rx.recv().await {
                mesh.handle_value(value).await;
            }
            mesh.emit_status("disconnected", None);
        });
    }

    async fn fetch_identity(&self) -> Option<String> {
        let resp = self.client.request(&Request::IdentityShow).await.ok()?;
        resp.data?
            .get("device_id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    }

    /// The user's optional display-name override from the daemon identity.
    /// `None` (or empty) means "use the hostname".
    async fn fetch_identity_label(&self) -> Option<String> {
        let resp = self.client.request(&Request::IdentityShow).await.ok()?;
        resp.data?
            .get("label")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.trim().is_empty())
    }

    /// Update this node's display label (the identity override) on the live
    /// presence profile and re-broadcast so peers pick it up. An empty label
    /// resets the display to the machine hostname.
    pub async fn set_label(self: &Arc<Self>, label: String) {
        {
            let mut st = self.state.lock();
            if let Some(p) = st.profile.as_mut() {
                p.label = if label.trim().is_empty() {
                    p.hostname.clone()
                } else {
                    label
                };
            }
        }
        self.broadcast_presence().await;
    }

    /// All joined networks' config ids. The daemon wraps the list as
    /// `{ "networks": [...] }`, so we read that field (an earlier version
    /// called `as_array()` on the wrapper and always got nothing — which left
    /// presence un-subscribed and peers unable to see each other).
    async fn fetch_networks(&self) -> Vec<String> {
        let Some(resp) = self.client.request(&Request::NetworksList).await.ok() else {
            return Vec::new();
        };
        resp.data
            .as_ref()
            .and_then(|d| d.get("networks"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|n| n.get("config_id").and_then(|v| v.as_str()).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn build_profile(&self, me: &str, label_override: Option<String>) -> NodeProfile {
        let inv = allmystuff_inventory::scan();
        let node = NodeId::from(me);
        let hostname = inv.host.hostname.clone();
        // Display name = override if the user set one, else the hostname.
        let label = label_override
            .filter(|l| !l.trim().is_empty())
            .unwrap_or_else(|| hostname.clone());
        NodeProfile {
            protocol: PROTOCOL_VERSION,
            node: node.clone(),
            label,
            hostname,
            summary: allmystuff_bridge::node_summary(&inv),
            capabilities: allmystuff_bridge::capabilities_from_inventory(&inv, &node),
            // Tell peers who owns this device and whether it's up for
            // adoption, so they can't silently grab a box that's already
            // spoken for (or one that was never put into claim mode).
            owner: self.ownership.owner().map(NodeId::from),
            claimable: self.ownership.claimable(),
        }
    }

    async fn broadcast_presence(&self) {
        let (networks, profile) = {
            let st = self.state.lock();
            (st.networks.clone(), st.profile.clone())
        };
        let Some(profile) = profile else { return };
        let Ok(payload) = serde_json::to_value(&profile) else { return };
        for network in networks {
            let _ = self
                .client
                .request(&Request::ChannelSendAll {
                    network,
                    channel: CHANNEL_PRESENCE.to_string(),
                    payload: payload.clone(),
                })
                .await;
        }
    }

    async fn handle_value(self: &Arc<Self>, value: Value) {
        let Some(kind) = value.get("kind").and_then(|v| v.as_str()) else {
            return;
        };
        match kind {
            "channel_inbound" => {
                let channel = value.get("channel").and_then(|v| v.as_str()).unwrap_or("");
                let from = value.get("from").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let payload = value.get("payload").cloned().unwrap_or(Value::Null);
                self.handle_channel(channel, from, payload).await;
            }
            "event" => {
                if let Some(event) = value.get("event") {
                    let _ = self.app.emit("allmystuff://event", event.clone());
                }
            }
            _ => {}
        }
    }

    async fn handle_channel(self: &Arc<Self>, channel: &str, from: String, payload: Value) {
        match channel {
            CHANNEL_PRESENCE => {
                if let Ok(profile) = serde_json::from_value::<NodeProfile>(payload) {
                    let changed = {
                        let mut st = self.state.lock();
                        st.session.as_mut().map(|s| s.apply_presence(profile)).unwrap_or(false)
                    };
                    if changed {
                        self.emit_snapshot();
                    }
                }
            }
            CHANNEL_CONTROL => {
                if let Ok(msg) = serde_json::from_value::<ControlMessage>(payload) {
                    let effects = {
                        let mut st = self.state.lock();
                        st.session
                            .as_mut()
                            .map(|s| s.handle(NodeId::from(from.as_str()), msg))
                            .unwrap_or_default()
                    };
                    self.process_effects(effects).await;
                    self.emit_snapshot();
                }
            }
            CHANNEL_MEDIA => {
                if let Ok(frame) = serde_json::from_value::<AudioFrame>(payload) {
                    self.audio.feed(&frame.route, &frame);
                }
            }
            _ => {}
        }
    }

    /// Front-end command: offer a route from `from` to `to`.
    pub async fn connect(self: &Arc<Self>, from: String, to: String, media: String) -> Result<String, String> {
        let me = self.local_node_id().ok_or("mesh not ready")?;
        let media = parse_media(&media);
        let route = Route {
            id: format!("route:{from}→{to}"),
            from: from.clone().into(),
            to: to.clone().into(),
            media,
            group: None,
        };
        let from_node = node_of(&from);
        let to_node = node_of(&to);
        let peer = if from_node == me { to_node } else { from_node };

        if peer == me {
            // Local loopback (e.g. this machine's mic to its own speakers):
            // no peer to negotiate with — record it active and stream now.
            // Offer-then-Accept drives the session to Active and yields the
            // StartMedia effect we process below.
            let effects = {
                let mut st = self.state.lock();
                let s = st.session.as_mut().ok_or("mesh not ready")?;
                let _ = s.offer(route.clone(), me.as_str());
                s.handle(
                    NodeId::from(me.as_str()),
                    ControlMessage::Route(RouteControl::Accept { route_id: route.id.clone() }),
                )
            };
            self.process_effects(effects).await;
            self.emit_snapshot();
            return Ok(route.id);
        }

        let msg = {
            let mut st = self.state.lock();
            let s = st.session.as_mut().ok_or("mesh not ready")?;
            s.offer(route.clone(), peer.as_str())
        };
        self.send_control(&peer, &msg).await;
        self.emit_snapshot();
        Ok(route.id)
    }

    pub async fn disconnect(self: &Arc<Self>, route_id: String) -> Result<(), String> {
        let msg = {
            let mut st = self.state.lock();
            st.session.as_mut().and_then(|s| s.teardown(&route_id))
        };
        self.audio.stop(&route_id);
        if let (Some(msg), Some(peer)) = (&msg, self.route_peer(&route_id)) {
            self.send_control(&peer, msg).await;
        }
        self.emit_snapshot();
        Ok(())
    }

    pub fn snapshot(&self) -> Value {
        let st = self.state.lock();
        let Some(session) = st.session.as_ref() else {
            return json!({ "ready": false });
        };
        let me = session.me().to_string();
        let network = st.network.clone();
        let peers: Vec<_> = session.peers().collect();
        let routes: Vec<_> = session.routes().collect();
        json!({
            "ready": true,
            "me": me,
            "network": network,
            "peers": peers,
            "routes": routes,
        })
    }

    fn route_peer(&self, route_id: &str) -> Option<String> {
        self.state
            .lock()
            .session
            .as_ref()
            .and_then(|s| s.route(route_id).map(|r| r.peer.to_string()))
    }

    async fn process_effects(self: &Arc<Self>, effects: Vec<Effect>) {
        for e in effects {
            match e {
                Effect::Send { peer, message } => self.send_control(&peer.to_string(), &message).await,
                Effect::StartMedia(route) => self.start_media(&route),
                Effect::StopMedia(id) => self.audio.stop(&id),
                Effect::Share { from, message } => {
                    let _ = self.app.emit(
                        "allmystuff://share",
                        json!({ "from": from.to_string(), "message": message }),
                    );
                }
                Effect::Ownership { from, message } => self.handle_ownership(from, message).await,
            }
        }
    }

    /// Apply an inbound ownership message. A [`OwnershipControl::Claim`] is
    /// the load-bearing one: this device only lets the claim take if it's
    /// actually claimable (in claim mode and unowned) — that's the rule that
    /// stops a peer flat-out taking a box. The other variants are feedback
    /// the claimer's UI surfaces.
    async fn handle_ownership(self: &Arc<Self>, from: NodeId, message: OwnershipControl) {
        match message {
            OwnershipControl::Claim { owner } => {
                // The owner of record is the *authenticated sender* the mesh
                // delivered (`from`), never an arbitrary value in the body —
                // otherwise a peer could claim a box "for" someone else. The
                // claimer asserts its display id while the daemon delivers the
                // bare pubkey, so compare by pubkey (self-asserted) and record
                // the authenticated `from`.
                let reply = if pubkey_part(owner.as_str()) != pubkey_part(from.as_str()) {
                    OwnershipControl::Declined {
                        reason: "a claim must be self-asserted".into(),
                    }
                } else if self.ownership.try_accept_claim(from.as_str()) {
                    // The claim took — re-advertise with the new owner so the
                    // claimer (and everyone) sees it's now spoken for.
                    self.refresh_profile_ownership().await;
                    OwnershipControl::Claimed { owner }
                } else {
                    OwnershipControl::Declined {
                        reason: "this device isn't in claim mode".into(),
                    }
                };
                self.send_control(&from.to_string(), &ControlMessage::Ownership(reply))
                    .await;
            }
            OwnershipControl::Release => {
                // The recorded owner is letting this device go (compare by
                // pubkey — same display-vs-bare id reconciliation as Claim).
                let owner = self.ownership.owner();
                if owner.as_deref().map(pubkey_part) == Some(pubkey_part(from.as_str())) {
                    self.ownership.set_owner(None);
                    self.refresh_profile_ownership().await;
                }
            }
            other => {
                // Claimed / Declined — feedback for the claimer's UI.
                let _ = self.app.emit(
                    "allmystuff://ownership",
                    json!({ "from": from.to_string(), "message": other }),
                );
            }
        }
    }

    /// Re-stamp the live presence profile's owner/claimable from the store
    /// and broadcast, so an ownership change propagates immediately.
    async fn refresh_profile_ownership(self: &Arc<Self>) {
        {
            let mut st = self.state.lock();
            if let Some(p) = st.profile.as_mut() {
                p.owner = self.ownership.owner().map(NodeId::from);
                p.claimable = self.ownership.claimable();
            }
        }
        self.broadcast_presence().await;
        self.emit_snapshot();
    }

    /// Front-end command: claim `node` as owned by this device. Only the
    /// target deciding it's claimable makes it stick; we just send intent.
    pub async fn claim(self: &Arc<Self>, node: String) -> Result<(), String> {
        let me = self.local_node_id().ok_or("mesh not ready")?;
        let msg = ControlMessage::Ownership(OwnershipControl::Claim { owner: me.into() });
        self.send_control(&node, &msg).await;
        Ok(())
    }

    /// Front-end command: put *this* device into (or out of) claim mode, so
    /// another of your machines can adopt it. Re-advertises immediately.
    pub async fn set_claimable(self: &Arc<Self>, on: bool) -> Result<bool, String> {
        self.ownership.set_claim_mode(on);
        self.refresh_profile_ownership().await;
        Ok(self.ownership.claimable())
    }

    /// Begin carrying media for a now-active route. Only audio is wired
    /// today; the route still shows active for other media so the UI is
    /// honest about what's connected vs streaming.
    fn start_media(&self, route: &Route) {
        if route.media != MediaKind::Audio {
            tracing::info!("route {} active ({:?}); media transport for it is a follow-up", route.id, route.media);
            return;
        }
        let Some(me) = self.local_node_id() else { return };
        let from_node = node_of(route.from.as_str());
        let to_node = node_of(route.to.as_str());

        // We source: capture the default mic and stream to the sink node.
        if from_node == me {
            let peer = to_node.clone();
            let rid = route.id.clone();
            let tx = self.audio_out.clone();
            let seq = Arc::new(AtomicU64::new(0));
            self.audio.start_capture(route.id.clone(), move |pcm, rate| {
                let s = seq.fetch_add(1, Ordering::Relaxed);
                let frame = AudioFrame::new(rid.clone(), s, rate, 1, pcm);
                let _ = tx.send((peer.clone(), frame));
            });
        }
        // We sink: play inbound frames for this route.
        if to_node == me {
            self.audio.start_playback(route.id.clone());
        }
    }

    async fn send_control(&self, peer: &str, message: &ControlMessage) {
        let Some(network) = self.network() else { return };
        if let Ok(payload) = serde_json::to_value(message) {
            let _ = self
                .client
                .request(&Request::ChannelSendTo {
                    network,
                    channel: CHANNEL_CONTROL.to_string(),
                    peer: peer.to_string(),
                    payload,
                })
                .await;
        }
    }

    fn emit_snapshot(&self) {
        let _ = self.app.emit("allmystuff://session", self.snapshot());
    }

    fn emit_status(&self, status: &str, error: Option<&str>) {
        let _ = self.app.emit(
            "allmystuff://subscription",
            json!({ "status": status, "error": error }),
        );
    }
}

/// Node id from a capability id (`"<node>:<device>"`). The node segment is
/// everything before the first colon.
fn node_of(cap_id: &str) -> String {
    cap_id.split_once(':').map(|(n, _)| n.to_string()).unwrap_or_else(|| cap_id.to_string())
}

/// The stable pubkey portion of a mesh device id — strip MyOwnMesh's trailing
/// 5-char display suffix (`-AB12C`). Mirrors the core's `signing::pubkey_part`,
/// so a device id in display form (`pubkey-SUFFIX`, what `IdentityShow` and
/// presence use) and bare form (`pubkey`, what the daemon delivers as a
/// channel `from`) compare equal.
fn pubkey_part(id: &str) -> &str {
    if let Some((body, suffix)) = id.rsplit_once('-') {
        if suffix.len() == 5 && suffix.chars().all(|c| c.is_ascii_alphanumeric()) {
            return body;
        }
    }
    id
}

fn parse_media(s: &str) -> MediaKind {
    match s {
        "audio" => MediaKind::Audio,
        "video" => MediaKind::Video,
        "display" => MediaKind::Display,
        "input" => MediaKind::Input,
        "storage" => MediaKind::Storage,
        _ => MediaKind::Generic,
    }
}
