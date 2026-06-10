//! The live mesh: wires the daemon's typed channels to the
//! [`allmystuff_session::Session`] state machine and the [`AudioBridge`].
//!
//! On start it subscribes to the AllMyStuff presence / control / media
//! channels on every joined network, broadcasts this node's
//! [`NodeProfile`], and pumps inbound frames:
//!
//!  * **presence** → updates the peer set (the graph fills with real peers).
//!  * **control** → drives the route handshake; the [`Effect`]s it returns
//!    send replies and start/stop audio.
//!  * **media** → audio frames fed to the playback side of active routes.
//!
//! Everything the front-end sees comes through `allmystuff://session`
//! snapshots emitted after each change.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;

use allmystuff_graph::{MediaKind, NodeId, Route};
use allmystuff_protocol::{
    ClientId, ControlMessage, NodeProfile, OwnedRoster, OwnershipControl, Request, RouteControl,
    CHANNEL_CONTROL, CHANNEL_MEDIA, CHANNEL_OWNED, CHANNEL_PRESENCE, PROTOCOL_VERSION,
};
use allmystuff_session::{
    AudioFrame, Effect, InputAction, InputEvent, MediaPayload, Session, VideoFrame,
};

use crate::audio::AudioBridge;
use crate::control_client::ControlClient;
use crate::input_inject::Injector;
use crate::ownership::Ownership;
use crate::video::VideoBridge;

pub struct Mesh {
    client: Arc<ControlClient>,
    app: AppHandle,
    audio: Arc<AudioBridge>,
    /// Screen capture for display routes this machine sources (the far end
    /// of a console session looking at us).
    video: Arc<VideoBridge>,
    /// Keyboard/mouse injection for input routes that sink here — gated on
    /// the sender being our owner or a fleet member.
    injector: Injector,
    state: Mutex<State>,
    /// This device's persisted ownership record — who owns it and whether
    /// it's currently offering itself for adoption (claim mode).
    ownership: Arc<Ownership>,
    /// Outbound audio: capture callbacks push `(peer, frame)`; a forwarder
    /// task sends them on the media channel.
    audio_out: mpsc::UnboundedSender<(String, AudioFrame)>,
    /// Outbound video, deliberately *bounded*: when the link can't keep up
    /// the capture side drops frames (each is a standalone JPEG, so a drop
    /// costs nothing but freshness) instead of queueing stale ones.
    video_out: mpsc::Sender<(String, VideoFrame)>,
    /// Sequence for outbound input events (one stream per app run).
    input_seq: AtomicU64,
}

struct State {
    session: Option<Session>,
    /// Primary network — the fallback for route control/media when we don't
    /// yet know which network a peer is on.
    network: Option<String>,
    /// Every joined network. Presence is broadcast on all of them so peers
    /// find each other regardless of which network the daemon lists first.
    networks: Vec<String>,
    /// Which network each peer was last seen on (canonical pubkey → network
    /// config_id). You can be on several networks at once and a given peer may
    /// only share one of them, so control/media must be addressed to the
    /// network that peer actually lives on — not a single "primary" mesh.
    peer_networks: HashMap<String, String>,
    client_id: Option<ClientId>,
    profile: Option<NodeProfile>,
}

impl Mesh {
    pub fn new(client: Arc<ControlClient>, app: AppHandle) -> Arc<Self> {
        let (audio_out, mut audio_rx) = mpsc::unbounded_channel::<(String, AudioFrame)>();
        // A shallow queue: at most a few frames in flight, so a slow link
        // sheds load by dropping captures rather than growing latency.
        let (video_out, mut video_rx) = mpsc::channel::<(String, VideoFrame)>(4);
        let mesh = Arc::new(Mesh {
            client: client.clone(),
            app,
            audio: Arc::new(AudioBridge::new()),
            video: Arc::new(VideoBridge::new()),
            injector: Injector::new(),
            state: Mutex::new(State {
                session: None,
                network: None,
                networks: Vec::new(),
                peer_networks: HashMap::new(),
                client_id: None,
                profile: None,
            }),
            ownership: Arc::new(Ownership::load()),
            audio_out,
            video_out,
            input_seq: AtomicU64::new(0),
        });

        // Forwarders: drain captured frames out to peers on the media
        // channel — audio unbounded (tiny, ordered), video bounded.
        {
            let mesh = mesh.clone();
            tauri::async_runtime::spawn(async move {
                while let Some((peer, frame)) = audio_rx.recv().await {
                    let Ok(payload) = serde_json::to_value(&frame) else {
                        continue;
                    };
                    mesh.send_media_value(&peer, payload).await;
                }
            });
        }
        {
            let mesh = mesh.clone();
            tauri::async_runtime::spawn(async move {
                while let Some((peer, frame)) = video_rx.recv().await {
                    let Ok(payload) = serde_json::to_value(&frame) else {
                        continue;
                    };
                    mesh.send_media_value(&peer, payload).await;
                }
            });
        }
        mesh
    }

    /// Send one media-channel payload to `peer` (canonicalised to the bare
    /// pubkey the daemon's peer set is keyed by).
    async fn send_media_value(&self, peer: &str, payload: Value) {
        let Some(network) = self.network_for_peer(peer) else {
            return;
        };
        let _ = self
            .client
            .request(&Request::ChannelSendTo {
                network,
                channel: CHANNEL_MEDIA.to_string(),
                peer: pubkey_part(peer).to_string(),
                payload,
            })
            .await;
    }

    fn network(&self) -> Option<String> {
        self.state.lock().network.clone()
    }

    /// The network to reach `peer` on: the one we last saw them advertise on,
    /// falling back to the primary. This is what lets a connection cross to a
    /// peer that only shares a secondary network with us.
    fn network_for_peer(&self, peer: &str) -> Option<String> {
        let st = self.state.lock();
        st.peer_networks
            .get(pubkey_part(peer))
            .cloned()
            .or_else(|| st.network.clone())
    }

    /// This node's mesh id once known (the daemon device id), else `None`.
    pub fn local_node_id(&self) -> Option<String> {
        self.state
            .lock()
            .session
            .as_ref()
            .map(|s| s.me().to_string())
    }

    /// This node's mesh id, resolved even before the live session starts: the
    /// session id once `start()` has run, else the daemon identity's device id
    /// (available as soon as the control socket is up). So a scan at launch
    /// already carries the real id and the local node never lingers under the
    /// `"this"` placeholder (which is what made this machine briefly show as a
    /// bare "not on AllMyStuff" twin). `None` only when the daemon is
    /// unreachable.
    pub async fn resolve_local_id(&self) -> Option<String> {
        if let Some(id) = self.local_node_id() {
            return Some(id);
        }
        self.fetch_identity().await
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
        let me = self
            .fetch_identity()
            .await
            .unwrap_or_else(|| NodeId::this().to_string());
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
            // Still run the claim-status check (it sanitizes stale fleet
            // residue and refreshes the UI); the broadcasts inside are
            // no-ops with no networks to send on.
            self.ownership_check(None).await;
            self.emit_status("no_network", None);
        } else {
            // Every AllMyStuff channel on *every* network. Presence + the
            // owned-fleet roster so two machines discover each other (and
            // converge their fleet) no matter which network the daemon lists
            // first — and control + media too, because point-to-point traffic
            // is addressed to whichever network *we* last saw the peer on,
            // which need not be the peer's first-listed one. With these on
            // the primary only, a claim or route offer arriving on a shared
            // secondary network had no subscriber on the receiving side and
            // the daemon silently dropped it.
            self.subscribe_channels(client_id, &networks).await;
            // App-load trigger of the claim-status check: sanitize stale
            // fleet residue, then assert presence + roster to everyone.
            self.ownership_check(None).await;
            self.emit_status("live", None);
        }

        // Periodic presence + owned-roster re-broadcast so late joiners see us
        // and the fleet converges.
        {
            let mesh = self.clone();
            tauri::async_runtime::spawn(async move {
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(20));
                loop {
                    tick.tick().await;
                    mesh.broadcast_presence().await;
                    mesh.broadcast_owned().await;
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
                    .filter_map(|n| {
                        n.get("config_id")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                    })
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
        let Ok(payload) = serde_json::to_value(&profile) else {
            return;
        };
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
                let from = value
                    .get("from")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // The network this frame arrived on — so we learn which network
                // each peer lives on and can address replies back to it.
                let network = value
                    .get("network")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let payload = value.get("payload").cloned().unwrap_or(Value::Null);
                self.handle_channel(channel, from, network, payload).await;
            }
            "event" => {
                if let Some(event) = value.get("event") {
                    // Connection establishment is a claim-status trigger: a
                    // peer just went live for app traffic ("approved"), so
                    // re-assert presence + fleet roster straight at it —
                    // both sides converge now instead of waiting for the
                    // next periodic broadcast.
                    let approved = event.get("event_kind").and_then(|v| v.as_str()) == Some("peer")
                        && event.get("kind").and_then(|v| v.as_str()) == Some("approved");
                    if approved {
                        if let Some(device) = event.get("device_id").and_then(|v| v.as_str()) {
                            let mesh = self.clone();
                            let device = device.to_string();
                            tauri::async_runtime::spawn(async move {
                                mesh.ownership_check(Some(&device)).await;
                            });
                        }
                    }
                    let _ = self.app.emit("allmystuff://event", event.clone());
                }
            }
            _ => {}
        }
    }

    async fn handle_channel(
        self: &Arc<Self>,
        channel: &str,
        from: String,
        network: String,
        payload: Value,
    ) {
        // Remember which network this peer is reachable on, so control/media
        // we send back goes to the right one (a peer may share only one of the
        // several networks we're on).
        if !network.is_empty() && !from.is_empty() {
            self.state
                .lock()
                .peer_networks
                .insert(pubkey_part(&from).to_string(), network);
        }
        match channel {
            CHANNEL_PRESENCE => {
                if let Ok(profile) = serde_json::from_value::<NodeProfile>(payload) {
                    let changed = {
                        let mut st = self.state.lock();
                        st.session
                            .as_mut()
                            .map(|s| s.apply_presence(profile))
                            .unwrap_or(false)
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
                let Some(media) = MediaPayload::decode(payload) else {
                    return;
                };
                match media {
                    MediaPayload::Audio(frame) => self.audio.feed(&frame.route, &frame),
                    MediaPayload::Video(frame) => {
                        // Surface frames only for a route this session knows
                        // is live, sinks here, and belongs to the sender —
                        // the console window(s) render them.
                        if self.inbound_media_ok(&frame.route, &from, MediaKind::Display) {
                            let _ = self.app.emit("allmystuff://video", &frame);
                        } else {
                            tracing::debug!(
                                "dropped video frame for {} from {} (route not live here)",
                                frame.route,
                                short_id(&from)
                            );
                        }
                    }
                    MediaPayload::Input(ev) => {
                        // Injecting keystrokes is the most privileged thing
                        // on the mesh, so it takes both gates: a live input
                        // route from this exact sender, *and* the sender
                        // being this device's recorded owner or a co-owned
                        // fleet member. (Share-grant-based control rides on
                        // the share enforcement work — not wired yet.)
                        if self.inbound_media_ok(&ev.route, &from, MediaKind::Input)
                            && self.sender_may_control(&from)
                        {
                            self.injector.apply(ev.action);
                        } else {
                            tracing::warn!(
                                "dropped input event from {from}: not an authorized controller"
                            );
                        }
                    }
                }
            }
            CHANNEL_OWNED => {
                // A peer gossiped its fleet roster. Merge it; if our copy
                // changed (a new member, or we adopted the key as a freshly
                // adopted device), re-broadcast so the fleet converges and
                // tell the front-end.
                if let Ok(roster) = serde_json::from_value::<OwnedRoster>(payload) {
                    let Some(me) = self.local_node_id() else {
                        return;
                    };
                    let structural = self.ownership.merge_fleet(&me, &roster);
                    let outcome = if structural {
                        "merged"
                    } else if self.ownership.fleet().is_some_and(|f| f.key == roster.key) {
                        "in sync"
                    } else {
                        "ignored (not our fleet)"
                    };
                    tracing::info!(
                        "owned roster from {}: key …{} v{} ({} members) → {outcome}",
                        short_id(&from),
                        key_tail(&roster.key),
                        roster.version,
                        roster.members.len(),
                    );
                    if structural {
                        self.broadcast_owned().await;
                        self.emit_owned();
                    }
                }
            }
            _ => {}
        }
    }

    /// Front-end command: offer a route from `from` to `to`.
    pub async fn connect(
        self: &Arc<Self>,
        from: String,
        to: String,
        media: String,
    ) -> Result<String, String> {
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
                    ControlMessage::Route(RouteControl::Accept {
                        route_id: route.id.clone(),
                    }),
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
        if let Err(e) = self.send_control(&peer, &msg).await {
            // The peer never saw the offer — drop it rather than leave a
            // phantom half-open route in the session.
            tracing::warn!(
                "route {} offer to {} undeliverable: {e}",
                route.id,
                short_id(&peer)
            );
            let mut st = self.state.lock();
            if let Some(s) = st.session.as_mut() {
                let _ = s.teardown(&route.id);
            }
            return Err(e);
        }
        tracing::info!(
            "route {} offered to {} — awaiting accept",
            route.id,
            short_id(&peer)
        );
        self.emit_snapshot();
        Ok(route.id)
    }

    pub async fn disconnect(self: &Arc<Self>, route_id: String) -> Result<(), String> {
        let msg = {
            let mut st = self.state.lock();
            st.session.as_mut().and_then(|s| s.teardown(&route_id))
        };
        self.audio.stop(&route_id);
        self.video.stop(&route_id);
        if let (Some(msg), Some(peer)) = (&msg, self.route_peer(&route_id)) {
            // Best-effort: the route is gone locally either way.
            let _ = self.send_control(&peer, msg).await;
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
                Effect::Send { peer, message } => {
                    // Replies ride best-effort; the failure is already logged.
                    let _ = self.send_control(&peer.to_string(), &message).await;
                }
                Effect::StartMedia(route) => self.start_media(&route),
                Effect::StopMedia(id) => {
                    self.audio.stop(&id);
                    self.video.stop(&id);
                }
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
                    // The claim took — a claim change runs the full status
                    // check: re-advertise with the new owner so the claimer
                    // (and everyone) sees it's now spoken for. Any stale
                    // fleet state was reset by the accept; the owner's
                    // roster lands next on the owned channel.
                    tracing::info!(
                        "claim accepted: {} now owns this device",
                        short_id(from.as_str())
                    );
                    self.ownership_check(None).await;
                    OwnershipControl::Claimed { owner }
                } else {
                    tracing::info!(
                        "claim from {} declined: not in claim mode",
                        short_id(from.as_str())
                    );
                    OwnershipControl::Declined {
                        reason: "this device isn't in claim mode".into(),
                    }
                };
                if let Err(e) = self
                    .send_control(&from.to_string(), &ControlMessage::Ownership(reply))
                    .await
                {
                    tracing::warn!(
                        "couldn't send the claim reply to {}: {e}",
                        short_id(from.as_str())
                    );
                }
            }
            OwnershipControl::Release => {
                // The recorded owner is letting this device go (compare by
                // pubkey — same display-vs-bare id reconciliation as Claim).
                // A claim change → run the full status check (the release
                // also cleared our fleet membership, so the empty roster
                // reaches the UI).
                let owner = self.ownership.owner();
                if owner.as_deref().map(pubkey_part) == Some(pubkey_part(from.as_str())) {
                    tracing::info!("released by {} — unowned again", short_id(from.as_str()));
                    self.ownership.set_owner(None);
                    self.ownership_check(None).await;
                }
            }
            OwnershipControl::Claimed { owner } => {
                // The device we claimed (`from`) accepted us as its owner.
                // Make the claim *do* something durable: establish or extend
                // the owned fleet — mint our key on the first adoption, add
                // ourselves and the new device, hand the full roster straight
                // to it, and gossip so every co-owned device converges on the
                // same key + membership. This is the "Owned roster" linking the
                // fleet under a shared key.
                self.ownership.ensure_fleet_key();
                if let Some(me) = self.local_node_id() {
                    let my_label = self.profile_label().unwrap_or_else(|| me.clone());
                    self.ownership.upsert_member(&me, &my_label);
                }
                let label = self.peer_label(&from);
                self.ownership.upsert_member(from.as_str(), &label);
                if let Some(r) = self.ownership.fleet() {
                    tracing::info!(
                        "claim confirmed by {}; fleet key …{} now {} members (v{})",
                        short_id(from.as_str()),
                        key_tail(&r.key),
                        r.members.len(),
                        r.version
                    );
                }
                self.send_owned_to(from.as_str()).await;
                self.broadcast_owned().await;
                self.emit_owned();
                // Surface the claim feedback for the claimer's toast, too.
                let _ = self.app.emit(
                    "allmystuff://ownership",
                    json!({
                        "from": from.to_string(),
                        "message": OwnershipControl::Claimed { owner },
                    }),
                );
            }
            other => {
                // Declined — feedback for the claimer's UI.
                tracing::info!(
                    "ownership reply from {}: {:?}",
                    short_id(from.as_str()),
                    other
                );
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

    // ---- owned fleet gossip ------------------------------------------

    /// This node's current display label from the live presence profile.
    fn profile_label(&self) -> Option<String> {
        self.state.lock().profile.as_ref().map(|p| p.label.clone())
    }

    /// Best-known display label for a peer (matched by canonical pubkey, since
    /// the daemon delivers a bare pubkey while presence is keyed by display
    /// id), else a short id. Gives fleet members a friendly name.
    fn peer_label(&self, peer: &NodeId) -> String {
        let canon = pubkey_part(peer.as_str());
        {
            let st = self.state.lock();
            if let Some(session) = st.session.as_ref() {
                for p in session.peers() {
                    if pubkey_part(p.node.as_str()) == canon && !p.label.trim().is_empty() {
                        return p.label.clone();
                    }
                }
            }
        }
        let s = peer.as_str();
        if s.len() > 12 {
            format!("{}…", &s[..10])
        } else {
            s.to_string()
        }
    }

    /// Broadcast this device's fleet roster (if any) on the owned channel to
    /// every network, so co-owned devices converge on one key + membership.
    async fn broadcast_owned(&self) {
        let Some(roster) = self.ownership.fleet() else {
            return;
        };
        self.broadcast_roster(&roster).await;
    }

    /// Broadcast one explicit roster on every network — used for the final
    /// minus-self roster of a leave (our own store is already cleared) and
    /// the bumped roster of a kick. Logs how many peers each network's
    /// broadcast actually reached, so "the roster never arrived" is
    /// diagnosable from this side's log.
    async fn broadcast_roster(&self, roster: &OwnedRoster) {
        let networks = { self.state.lock().networks.clone() };
        let Ok(payload) = serde_json::to_value(roster) else {
            return;
        };
        for network in networks {
            let resp = self
                .client
                .request(&Request::ChannelSendAll {
                    network: network.clone(),
                    channel: CHANNEL_OWNED.to_string(),
                    payload: payload.clone(),
                })
                .await;
            match resp {
                Ok(r) if r.ok => {
                    let n = r
                        .data
                        .as_ref()
                        .and_then(|d| d.get("dispatched_to"))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    tracing::debug!("owned roster broadcast on {network} reached {n} peer(s)");
                }
                Ok(r) => tracing::warn!(
                    "owned roster broadcast on {network} refused: {}",
                    r.error.unwrap_or_else(|| "(no error)".into())
                ),
                Err(e) => tracing::warn!("owned roster broadcast on {network} failed: {e}"),
            }
        }
    }

    /// Send this device's fleet roster straight to one peer — used right after
    /// a claim so the new device gets the key + membership immediately, before
    /// the next periodic broadcast.
    async fn send_owned_to(&self, peer: &str) {
        let Some(roster) = self.ownership.fleet() else {
            return;
        };
        self.send_roster_to(peer, &roster).await;
    }

    /// Send one explicit roster straight to a peer — a kick hands the
    /// kicked device the roster it's no longer in, so it drops out now
    /// rather than at the next periodic broadcast.
    async fn send_roster_to(&self, peer: &str, roster: &OwnedRoster) {
        let Some(network) = self.network_for_peer(peer) else {
            tracing::warn!("no network to hand the fleet roster to {}", short_id(peer));
            return;
        };
        if let Ok(payload) = serde_json::to_value(roster) {
            let resp = self
                .client
                .request(&Request::ChannelSendTo {
                    network: network.clone(),
                    channel: CHANNEL_OWNED.to_string(),
                    peer: pubkey_part(peer).to_string(),
                    payload,
                })
                .await;
            match resp {
                Ok(r) if r.ok => {
                    tracing::info!("fleet roster handed to {} on {network}", short_id(peer));
                }
                Ok(r) => tracing::warn!(
                    "fleet roster to {} refused by daemon: {}",
                    short_id(peer),
                    r.error.unwrap_or_else(|| "(no error)".into())
                ),
                Err(e) => tracing::warn!("fleet roster to {} failed: {e}", short_id(peer)),
            }
        }
    }

    /// Push the current fleet roster to the front-end.
    fn emit_owned(&self) {
        let _ = self
            .app
            .emit("allmystuff://owned", self.owned_roster_value());
    }

    /// The current fleet roster as JSON — for the `owned_roster` command and
    /// the `allmystuff://owned` event. An empty key/members when there's no
    /// fleet yet, so the front-end always gets a well-formed shape.
    pub fn owned_roster_value(&self) -> Value {
        match self.ownership.fleet() {
            Some(r) => serde_json::to_value(r).unwrap_or_else(|_| empty_owned()),
            None => empty_owned(),
        }
    }

    /// Front-end command: claim `node` as owned by this device. Only the
    /// target deciding it's claimable makes it stick; we just send intent —
    /// but a send the daemon couldn't deliver (device dropped offline, no
    /// shared network) is surfaced so the UI can say so rather than leaving
    /// "asking…" hanging forever.
    pub async fn claim(self: &Arc<Self>, node: String) -> Result<(), String> {
        let me = self.local_node_id().ok_or("mesh not ready")?;
        tracing::info!("claiming {} (sending ownership claim)", short_id(&node));
        let msg = ControlMessage::Ownership(OwnershipControl::Claim { owner: me.into() });
        self.send_control(&node, &msg).await
    }

    /// Front-end command: put *this* device into (or out of) claim mode, so
    /// another of your machines can adopt it. Re-advertises immediately.
    pub async fn set_claimable(self: &Arc<Self>, on: bool) -> Result<bool, String> {
        self.ownership.set_claim_mode(on);
        self.refresh_profile_ownership().await;
        Ok(self.ownership.claimable())
    }

    /// The claim-status check — "is what we believe about ownership still
    /// true, and does everyone else know it?" Drops incoherent fleet
    /// residue, re-stamps the live profile from the ownership store, then
    /// re-asserts presence + roster. Runs **targeted** at one peer right
    /// after its connection establishes (so the two sides converge now, not
    /// at the next 20-second tick) and **broadcast** on the local triggers:
    /// session start, a claim/release, and fleet membership changes.
    pub async fn ownership_check(self: &Arc<Self>, peer: Option<&str>) {
        let Some(me) = self.local_node_id() else {
            return;
        };
        if self.ownership.sanitize_fleet(&me) {
            tracing::info!("ownership check dropped a stale fleet roster");
        }
        {
            let mut st = self.state.lock();
            if let Some(p) = st.profile.as_mut() {
                p.owner = self.ownership.owner().map(NodeId::from);
                p.claimable = self.ownership.claimable();
            }
        }
        match peer {
            Some(peer) => {
                tracing::debug!(
                    "ownership check → {} (connection established)",
                    short_id(peer)
                );
                self.send_presence_to(peer).await;
                self.send_owned_to(peer).await;
            }
            None => {
                self.broadcast_presence().await;
                self.broadcast_owned().await;
            }
        }
        self.emit_owned();
        self.emit_snapshot();
    }

    /// Send this node's presence profile straight to one peer — the
    /// targeted half of `broadcast_presence`, for a peer that just
    /// connected and hasn't heard our periodic advert yet.
    async fn send_presence_to(&self, peer: &str) {
        let profile = { self.state.lock().profile.clone() };
        let Some(profile) = profile else { return };
        let Some(network) = self.network_for_peer(peer) else {
            return;
        };
        if let Ok(payload) = serde_json::to_value(&profile) {
            let _ = self
                .client
                .request(&Request::ChannelSendTo {
                    network,
                    channel: CHANNEL_PRESENCE.to_string(),
                    peer: pubkey_part(peer).to_string(),
                    payload,
                })
                .await;
        }
    }

    /// Front-end command: leave the fleet this device belongs to. The
    /// remaining members get the bumped minus-us roster (replacement
    /// semantics drop us everywhere), our own fleet state clears, and —
    /// since membership follows ownership — any recorded owner is let go
    /// and presence re-advertises unowned.
    pub async fn fleet_leave(self: &Arc<Self>) -> Result<(), String> {
        let me = self.local_node_id().ok_or("mesh not ready")?;
        let roster = self
            .ownership
            .leave_fleet(&me)
            .ok_or("this device isn't in a fleet")?;
        tracing::info!(
            "leaving the fleet — broadcasting roster v{} ({} members remain)",
            roster.version,
            roster.members.len()
        );
        self.broadcast_roster(&roster).await;
        if self.ownership.owner().is_some() {
            self.ownership.set_owner(None);
        }
        self.refresh_profile_ownership().await;
        self.emit_owned();
        Ok(())
    }

    /// Front-end command: kick `device` out of the fleet. The store
    /// enforces the rule — only a member may kick, and never itself — and
    /// the kicked device learns immediately: it gets a best-effort
    /// ownership release (honoured when we're its recorded owner) plus the
    /// new roster it's absent from, which its merge treats as "kicked".
    pub async fn fleet_kick(self: &Arc<Self>, device: String) -> Result<(), String> {
        let me = self.local_node_id().ok_or("mesh not ready")?;
        let roster = self.ownership.kick_member(&me, &device)?;
        tracing::info!(
            "kicked {} from the fleet (roster now v{}, {} members)",
            short_id(&device),
            roster.version,
            roster.members.len()
        );
        self.broadcast_roster(&roster).await;
        let _ = self
            .send_control(
                &device,
                &ControlMessage::Ownership(OwnershipControl::Release),
            )
            .await;
        self.send_roster_to(&device, &roster).await;
        self.emit_owned();
        Ok(())
    }

    /// Re-read the joined networks, (re)subscribe every channel on each, then
    /// re-advertise. Called after the set of networks changes (create / join /
    /// leave) or a network's transport is restarted by a signaling/STUN/TURN
    /// edit — so the session follows the user across *every* network they're
    /// on, not just the ones present at launch. Re-subscribing an existing
    /// channel is idempotent on the daemon.
    pub async fn sync_networks(self: &Arc<Self>) {
        let client_id = { self.state.lock().client_id };
        let Some(client_id) = client_id else { return };
        let networks = self.fetch_networks().await;
        let primary = networks.first().cloned();
        {
            let mut st = self.state.lock();
            st.networks = networks.clone();
            st.network = primary.clone();
        }
        self.subscribe_channels(client_id, &networks).await;
        self.broadcast_presence().await;
        self.broadcast_owned().await;
        self.emit_snapshot();
    }

    /// Subscribe presence, owned, control, and media on each given network.
    /// All four ride every network: broadcasts (presence/owned) so peers are
    /// found wherever they are, and point-to-point (control/media) so a frame
    /// addressed to whichever network the *sender* last saw us on always has
    /// a subscriber here.
    async fn subscribe_channels(&self, client_id: ClientId, networks: &[String]) {
        let channels = [
            CHANNEL_PRESENCE,
            CHANNEL_OWNED,
            CHANNEL_CONTROL,
            CHANNEL_MEDIA,
        ];
        for network in networks {
            for channel in channels {
                let _ = self
                    .client
                    .request(&Request::ChannelSubscribe {
                        client_id,
                        network: network.clone(),
                        channel: channel.to_string(),
                    })
                    .await;
            }
        }
    }

    /// Begin carrying media for a now-active route. Audio, display (screen
    /// streaming), and input (remote control) are wired; camera video and
    /// storage still show active without a transport, and the log says so.
    fn start_media(&self, route: &Route) {
        let Some(me) = self.local_node_id() else {
            return;
        };
        let from_node = node_of(route.from.as_str());
        let to_node = node_of(route.to.as_str());

        match route.media {
            MediaKind::Audio => {
                // We source: capture the default mic and stream to the sink.
                if from_node == me {
                    let peer = to_node.clone();
                    let rid = route.id.clone();
                    let tx = self.audio_out.clone();
                    let seq = Arc::new(AtomicU64::new(0));
                    self.audio
                        .start_capture(route.id.clone(), move |pcm, rate| {
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
            MediaKind::Display => {
                // We're the screen being looked at: capture and stream
                // MJPEG to the viewer. The viewer side starts nothing here
                // — its console window renders the emitted frames.
                if from_node == me && to_node != me {
                    tracing::info!(
                        "route {} active — streaming this screen to {}",
                        route.id,
                        short_id(&to_node)
                    );
                    let peer = to_node.clone();
                    let tx = self.video_out.clone();
                    self.video.start_capture(route.id.clone(), move |frame| {
                        // try_send: a full queue drops this frame; the next
                        // capture carries a fresher picture.
                        tx.try_send((peer.clone(), frame)).is_ok()
                    });
                } else if to_node == me {
                    tracing::info!(
                        "route {} active — expecting screen frames from {}",
                        route.id,
                        short_id(&from_node)
                    );
                }
            }
            MediaKind::Input => {
                // The sink injects lazily per inbound event (behind the
                // ownership gate); the source is driven by the console
                // window via `send_input`. Nothing to start eagerly.
            }
            other => {
                tracing::info!(
                    "route {} active ({other:?}); media transport for it is a follow-up",
                    route.id
                );
            }
        }
    }

    /// Whether an inbound media frame is acceptable: its route is one this
    /// session knows, is live, carries `media`, sinks on this machine, and
    /// the daemon-authenticated sender is the route's peer.
    fn inbound_media_ok(&self, route_id: &str, sender: &str, media: MediaKind) -> bool {
        let Some(me) = self.local_node_id() else {
            return false;
        };
        let st = self.state.lock();
        let Some(r) = st.session.as_ref().and_then(|s| s.route(route_id)) else {
            return false;
        };
        r.is_active()
            && r.route.media == media
            && node_of(r.route.to.as_str()) == me
            && pubkey_part(r.peer.as_str()) == pubkey_part(sender)
    }

    /// Whether `sender` may drive this machine's keyboard and mouse: it is
    /// the recorded owner, or a member of the owned fleet this device
    /// belongs to. Nobody else — not even a peer a route auto-accepted for.
    fn sender_may_control(&self, sender: &str) -> bool {
        let canon = pubkey_part(sender);
        if self.ownership.owner().as_deref().map(pubkey_part) == Some(canon) {
            return true;
        }
        self.ownership.fleet().is_some_and(|r| {
            r.members
                .iter()
                .any(|m| pubkey_part(m.device.as_str()) == canon)
        })
    }

    /// Front-end command: forward one keyboard/mouse event down an active
    /// outbound input route (the console window's control stream).
    pub async fn send_input(
        self: &Arc<Self>,
        route_id: String,
        action: InputAction,
    ) -> Result<(), String> {
        let me = self.local_node_id().ok_or("mesh not ready")?;
        let peer = {
            let st = self.state.lock();
            let r = st
                .session
                .as_ref()
                .and_then(|s| s.route(&route_id))
                .ok_or("unknown route")?;
            if !(r.is_active()
                && r.route.media == MediaKind::Input
                && node_of(r.route.from.as_str()) == me)
            {
                return Err("route isn't an active outbound control link".into());
            }
            r.peer.to_string()
        };
        let seq = self.input_seq.fetch_add(1, Ordering::Relaxed);
        let ev = InputEvent::new(route_id, seq, action);
        let payload = serde_json::to_value(&ev).map_err(|e| e.to_string())?;
        self.send_media_value(&peer, payload).await;
        Ok(())
    }

    /// Send a control message to one peer, reporting whether the daemon
    /// actually dispatched it. The daemon's peer set is keyed by the *bare
    /// pubkey* (what signaling announces), while AllMyStuff mostly holds
    /// display ids (`pubkey-SUFFIX`, what presence and `IdentityShow` carry)
    /// — so the id is canonicalised here, at the daemon boundary. Addressing
    /// the display form made every send come back "peer not found", an error
    /// this used to swallow: a claim showed "asking…" and then nothing.
    async fn send_control(&self, peer: &str, message: &ControlMessage) -> Result<(), String> {
        let Some(network) = self.network_for_peer(peer) else {
            return Err(format!("no shared network with {peer}"));
        };
        let payload = serde_json::to_value(message).map_err(|e| e.to_string())?;
        let resp = self
            .client
            .request(&Request::ChannelSendTo {
                network,
                channel: CHANNEL_CONTROL.to_string(),
                peer: pubkey_part(peer).to_string(),
                payload,
            })
            .await
            .map_err(|e| e.to_string())?;
        if resp.ok {
            Ok(())
        } else {
            let err = resp.error.unwrap_or_else(|| "channel send failed".into());
            tracing::warn!("control send to {peer} failed: {err}");
            Err(err)
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

/// A well-formed but empty owned roster (no fleet yet).
fn empty_owned() -> Value {
    json!({ "key": "", "version": 0, "members": [] })
}

/// Node id from a capability id (`"<node>:<device>"`). The node segment is
/// everything before the first colon.
fn node_of(cap_id: &str) -> String {
    cap_id
        .split_once(':')
        .map(|(n, _)| n.to_string())
        .unwrap_or_else(|| cap_id.to_string())
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

/// Log-friendly head of a mesh id — enough to tell two machines apart in a
/// trace without drowning it in base32.
fn short_id(id: &str) -> String {
    if id.len() > 10 {
        format!("{}…", &id[..10])
    } else {
        id.to_string()
    }
}

/// Log-friendly tail of a fleet key — enough to compare two machines' logs
/// ("do we hold the same key?") without printing the grouping secret.
fn key_tail(key: &str) -> &str {
    let n = key.len();
    if n > 6 {
        &key[n - 6..]
    } else {
        key
    }
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
