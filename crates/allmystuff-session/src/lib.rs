//! # allmystuff-session
//!
//! The live half of AllMyStuff: what the static [`allmystuff_graph`] model
//! becomes once peers are actually connected. It tracks **who's here**
//! (presence) and **what's running** (the route handshake), as a pure,
//! deterministic state machine — no sockets, no audio devices — so the
//! whole connection lifecycle is unit-tested here and the Tauri backend
//! just feeds it messages and carries out the [`Effect`]s it returns.
//!
//! Lifecycle of a route:
//!
//! ```text
//!   offerer                         receiver
//!     │  Offer(route)  ───────────────►│   (Incoming)
//!     │                                ├─ accept ─► Active + StartMedia
//!     │◄────────────── Accept(id)      │
//!  Active + StartMedia                 │
//!     │  …audio frames flow…           │
//!     │  Teardown(id) ────────────────►│   StopMedia
//! ```

mod audio;
mod media;

use std::collections::HashMap;

use allmystuff_graph::{NodeId, Route};
use allmystuff_protocol::{
    ControlMessage, NodeProfile, OwnershipControl, RouteControl, ShareControl,
};

pub use allmystuff_protocol::{CHANNEL_CONTROL, CHANNEL_PRESENCE};
pub use audio::AudioFrame;
pub use media::{InputAction, InputEvent, MediaPayload, VideoAssembler, VideoFrame};

/// Which side of a route we are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// We sent the offer.
    Outbound,
    /// A peer offered it to us.
    Inbound,
}

/// Negotiation state of a single route.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RouteState {
    /// We offered; awaiting the peer's accept.
    Offered,
    /// The peer offered; awaiting our decision (only seen transiently when
    /// auto-accept is off).
    Incoming,
    /// Live — media may flow.
    Active,
    /// The peer said no.
    Rejected { reason: String },
    /// Either side tore it down.
    TornDown,
}

/// A route plus its live state and which peer it runs with.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LiveRoute {
    pub route: Route,
    pub peer: NodeId,
    pub origin: Origin,
    pub state: RouteState,
}

impl LiveRoute {
    pub fn is_active(&self) -> bool {
        self.state == RouteState::Active
    }
}

/// Something the backend must do as a result of handling a message. The
/// session never performs I/O itself; it returns intent.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Send this control message to a peer (on [`CHANNEL_CONTROL`]).
    Send {
        peer: NodeId,
        message: ControlMessage,
    },
    /// Start carrying media for this now-active route.
    StartMedia(Route),
    /// Stop carrying media for this route id.
    StopMedia(String),
    /// A share negotiation message arrived; apply it against the catalog.
    Share { from: NodeId, message: ShareControl },
    /// An ownership/claim message arrived; the backend applies it against
    /// this device's ownership record (claim mode, recorded owner).
    Ownership {
        from: NodeId,
        message: OwnershipControl,
    },
}

/// The live session for this node.
#[derive(Debug)]
pub struct Session {
    me: NodeId,
    peers: HashMap<NodeId, NodeProfile>,
    routes: HashMap<String, LiveRoute>,
    /// Auto-accept inbound offers from known peers (the offerer already
    /// authorized them against their catalog). The backend can turn this
    /// off to prompt the user per offer.
    pub auto_accept: bool,
}

impl Session {
    pub fn new(me: impl Into<NodeId>) -> Self {
        Session {
            me: me.into(),
            peers: HashMap::new(),
            routes: HashMap::new(),
            auto_accept: true,
        }
    }

    pub fn me(&self) -> &NodeId {
        &self.me
    }

    // ---- presence ----------------------------------------------------

    /// Record (or refresh) a peer's presence advert. Ignores our own echo.
    /// Returns `true` if this was new or changed.
    pub fn apply_presence(&mut self, profile: NodeProfile) -> bool {
        if profile.node == self.me {
            return false;
        }
        match self.peers.get(&profile.node) {
            Some(existing) if existing == &profile => false,
            _ => {
                self.peers.insert(profile.node.clone(), profile);
                true
            }
        }
    }

    pub fn peers(&self) -> impl Iterator<Item = &NodeProfile> {
        self.peers.values()
    }

    pub fn peer(&self, id: &NodeId) -> Option<&NodeProfile> {
        self.peers.get(id)
    }

    /// Drop a peer that left, tearing down any routes with it.
    pub fn drop_peer(&mut self, id: &NodeId) -> Vec<Effect> {
        self.peers.remove(id);
        let mut effects = Vec::new();
        let ids: Vec<String> = self
            .routes
            .iter()
            .filter(|(_, r)| &r.peer == id && r.is_active())
            .map(|(rid, _)| rid.clone())
            .collect();
        for rid in ids {
            if let Some(r) = self.routes.get_mut(&rid) {
                r.state = RouteState::TornDown;
            }
            effects.push(Effect::StopMedia(rid));
        }
        effects
    }

    // ---- routes ------------------------------------------------------

    /// Offer a route to the peer that owns its other endpoint. Records it
    /// as `Outbound`/`Offered` and returns the message to send.
    pub fn offer(&mut self, route: Route, peer: impl Into<NodeId>) -> ControlMessage {
        let peer = peer.into();
        self.routes.insert(
            route.id.clone(),
            LiveRoute {
                route: route.clone(),
                peer,
                origin: Origin::Outbound,
                state: RouteState::Offered,
            },
        );
        ControlMessage::Route(RouteControl::Offer { route })
    }

    /// Locally tear a route down. Returns the message to send the peer (if
    /// the route was known) so they stop too.
    pub fn teardown(&mut self, route_id: &str) -> Option<ControlMessage> {
        let r = self.routes.get_mut(route_id)?;
        let peer = r.peer.clone();
        r.state = RouteState::TornDown;
        let _ = peer;
        Some(ControlMessage::Route(RouteControl::Teardown {
            route_id: route_id.to_string(),
        }))
    }

    pub fn routes(&self) -> impl Iterator<Item = &LiveRoute> {
        self.routes.values()
    }

    pub fn route(&self, id: &str) -> Option<&LiveRoute> {
        self.routes.get(id)
    }

    pub fn active_routes(&self) -> impl Iterator<Item = &LiveRoute> {
        self.routes.values().filter(|r| r.is_active())
    }

    // ---- inbound control ---------------------------------------------

    /// Drive the state machine from a control message a peer sent us.
    pub fn handle(&mut self, from: NodeId, message: ControlMessage) -> Vec<Effect> {
        match message {
            ControlMessage::Route(rc) => self.handle_route(from, rc),
            ControlMessage::Share(sc) => vec![Effect::Share { from, message: sc }],
            ControlMessage::Ownership(oc) => vec![Effect::Ownership { from, message: oc }],
        }
    }

    fn handle_route(&mut self, from: NodeId, rc: RouteControl) -> Vec<Effect> {
        match rc {
            RouteControl::Offer { route } => {
                let accept = self.auto_accept;
                let state = if accept {
                    RouteState::Active
                } else {
                    RouteState::Incoming
                };
                self.routes.insert(
                    route.id.clone(),
                    LiveRoute {
                        route: route.clone(),
                        peer: from.clone(),
                        origin: Origin::Inbound,
                        state,
                    },
                );
                if accept {
                    vec![
                        Effect::Send {
                            peer: from,
                            message: ControlMessage::Route(RouteControl::Accept {
                                route_id: route.id.clone(),
                            }),
                        },
                        Effect::StartMedia(route),
                    ]
                } else {
                    Vec::new()
                }
            }
            RouteControl::Accept { route_id } => {
                if let Some(r) = self.routes.get_mut(&route_id) {
                    if r.origin == Origin::Outbound && r.state == RouteState::Offered {
                        r.state = RouteState::Active;
                        return vec![Effect::StartMedia(r.route.clone())];
                    }
                }
                Vec::new()
            }
            RouteControl::Reject { route_id, reason } => {
                if let Some(r) = self.routes.get_mut(&route_id) {
                    r.state = RouteState::Rejected { reason };
                }
                Vec::new()
            }
            RouteControl::Teardown { route_id } => {
                let mut effects = Vec::new();
                if let Some(r) = self.routes.get_mut(&route_id) {
                    let was_active = r.is_active();
                    r.state = RouteState::TornDown;
                    if was_active {
                        effects.push(Effect::StopMedia(route_id));
                    }
                }
                effects
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use allmystuff_graph::{Flow, MediaKind};
    use allmystuff_protocol::InventorySummary;

    fn profile(node: &str) -> NodeProfile {
        NodeProfile {
            protocol: allmystuff_protocol::PROTOCOL_VERSION,
            node: node.into(),
            label: node.into(),
            hostname: node.into(),
            summary: InventorySummary {
                os: "linux".into(),
                cpu: "cpu".into(),
                ram_bytes: 1 << 30,
                device_count: 3,
            },
            capabilities: vec![allmystuff_graph::Capability::new(
                node,
                format!("{node}:mic"),
                "Mic",
                MediaKind::Audio,
                Flow::Source,
                "microphone",
            )],
            owner: None,
            claimable: false,
            boot: 0,
        }
    }

    fn route(id: &str) -> Route {
        Route {
            id: id.into(),
            from: "this:mic".into(),
            to: "desk:system-audio".into(),
            media: MediaKind::Audio,
            group: None,
        }
    }

    #[test]
    fn presence_tracks_peers_and_ignores_self_echo() {
        let mut s = Session::new("this");
        assert!(s.apply_presence(profile("desk")));
        assert!(!s.apply_presence(profile("desk"))); // unchanged
        assert!(!s.apply_presence(profile("this"))); // our own echo
        assert_eq!(s.peers().count(), 1);
        assert!(s.peer(&"desk".into()).is_some());
    }

    #[test]
    fn outbound_offer_goes_active_on_accept_and_starts_media() {
        let mut s = Session::new("this");
        let msg = s.offer(route("r1"), "desk");
        assert!(matches!(
            msg,
            ControlMessage::Route(RouteControl::Offer { .. })
        ));
        assert_eq!(s.route("r1").unwrap().state, RouteState::Offered);

        let effects = s.handle(
            "desk".into(),
            ControlMessage::Route(RouteControl::Accept {
                route_id: "r1".into(),
            }),
        );
        assert_eq!(s.route("r1").unwrap().state, RouteState::Active);
        assert!(matches!(effects.as_slice(), [Effect::StartMedia(r)] if r.id == "r1"));
    }

    #[test]
    fn inbound_offer_auto_accepts_and_replies() {
        let mut s = Session::new("desk");
        let effects = s.handle(
            "this".into(),
            ControlMessage::Route(RouteControl::Offer { route: route("r1") }),
        );
        assert_eq!(s.route("r1").unwrap().state, RouteState::Active);
        // Replies Accept and starts media.
        assert!(effects.iter().any(|e| matches!(
            e,
            Effect::Send { message: ControlMessage::Route(RouteControl::Accept { route_id }), .. } if route_id == "r1"
        )));
        assert!(effects
            .iter()
            .any(|e| matches!(e, Effect::StartMedia(r) if r.id == "r1")));
    }

    #[test]
    fn inbound_offer_with_auto_accept_off_waits() {
        let mut s = Session::new("desk");
        s.auto_accept = false;
        let effects = s.handle(
            "this".into(),
            ControlMessage::Route(RouteControl::Offer { route: route("r1") }),
        );
        assert_eq!(s.route("r1").unwrap().state, RouteState::Incoming);
        assert!(effects.is_empty());
    }

    #[test]
    fn reject_marks_rejected_with_reason() {
        let mut s = Session::new("this");
        s.offer(route("r1"), "desk");
        s.handle(
            "desk".into(),
            ControlMessage::Route(RouteControl::Reject {
                route_id: "r1".into(),
                reason: "device busy".into(),
            }),
        );
        assert_eq!(
            s.route("r1").unwrap().state,
            RouteState::Rejected {
                reason: "device busy".into()
            }
        );
    }

    #[test]
    fn teardown_stops_media_on_both_sides() {
        // Receiver: offer accepted → active, then peer tears down.
        let mut s = Session::new("desk");
        s.handle(
            "this".into(),
            ControlMessage::Route(RouteControl::Offer { route: route("r1") }),
        );
        let effects = s.handle(
            "this".into(),
            ControlMessage::Route(RouteControl::Teardown {
                route_id: "r1".into(),
            }),
        );
        assert_eq!(s.route("r1").unwrap().state, RouteState::TornDown);
        assert!(matches!(effects.as_slice(), [Effect::StopMedia(id)] if id == "r1"));
    }

    #[test]
    fn dropping_a_peer_tears_down_its_active_routes() {
        let mut s = Session::new("desk");
        s.handle(
            "this".into(),
            ControlMessage::Route(RouteControl::Offer { route: route("r1") }),
        );
        let effects = s.drop_peer(&"this".into());
        assert!(matches!(effects.as_slice(), [Effect::StopMedia(id)] if id == "r1"));
        assert!(s.peer(&"this".into()).is_none());
    }

    #[test]
    fn share_messages_surface_for_the_catalog() {
        let mut s = Session::new("this");
        let effects = s.handle("alex".into(), ControlMessage::Share(ShareControl::Decline));
        assert!(matches!(
            effects.as_slice(),
            [Effect::Share { from, message: ShareControl::Decline }] if from == &NodeId::from("alex")
        ));
    }

    #[test]
    fn ownership_messages_surface_for_the_backend() {
        // A peer claims this device; the session just hands it up as an
        // Ownership effect — the backend decides whether the claim takes.
        let mut s = Session::new("puck");
        let effects = s.handle(
            "phone".into(),
            ControlMessage::Ownership(OwnershipControl::Claim {
                owner: "phone".into(),
            }),
        );
        assert!(matches!(
            effects.as_slice(),
            [Effect::Ownership { from, message: OwnershipControl::Claim { owner } }]
                if from == &NodeId::from("phone") && owner == &NodeId::from("phone")
        ));
    }
}
