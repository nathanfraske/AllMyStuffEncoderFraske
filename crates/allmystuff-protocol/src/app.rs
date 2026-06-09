//! AllMyStuff's own peer-to-peer protocol — the messages nodes exchange
//! *over* the mesh (inside the daemon's typed-channel frames) once they're
//! connected. Two channels:
//!
//!  * **presence** — each node broadcasts a [`NodeProfile`]: who it is, a
//!    thumbnail of its hardware, and the capabilities it's willing to wire
//!    up. This is what populates the other side's graph.
//!  * **control** — point-to-point [`ControlMessage`]s that set up and
//!    tear down routes and negotiate shares.
//!
//! Authorization is *not* on the wire: a node only ever advertises or
//! accepts what the local [`allmystuff_graph::Catalog`] already permits.
//! The wire carries intent; the catalog is the gate.

use serde::{Deserialize, Serialize};

use allmystuff_graph::{Capability, Grant, NodeId, Person, Route};

/// Mesh app-id for AllMyStuff peers. Distinct from `myownmesh` and
/// `myownllm` so the ecosystems don't collide on signaling (the same
/// non-interop discipline MyOwnMesh documents).
pub const APP_ID: &str = "allmystuff-cloud-mesh-v1";

/// Bumped when a message shape changes incompatibly. Peers include it in
/// presence so a newer node can downgrade its offers for an older one.
pub const PROTOCOL_VERSION: u32 = 1;

/// Typed-channel name for periodic presence broadcasts.
pub const CHANNEL_PRESENCE: &str = "allmystuff/presence/v1";

/// Typed-channel name for point-to-point route/share control.
pub const CHANNEL_CONTROL: &str = "allmystuff/control/v1";

/// Typed-channel name carrying the media plane (audio frames) of active
/// routes. Frames self-identify by route id, so one channel demuxes them
/// all.
pub const CHANNEL_MEDIA: &str = "allmystuff/media/v1";

/// A thumbnail of a node's hardware — enough for the graph's node card
/// without shipping the whole [`allmystuff_inventory::Inventory`]. The
/// backend fills this from a scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventorySummary {
    pub os: String,
    pub cpu: String,
    pub ram_bytes: u64,
    /// Headline device count for the "12 things" chip.
    pub device_count: u32,
}

/// What a node tells its peers about itself. Broadcast on the presence
/// channel when joining and whenever its inventory or offered capabilities
/// change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeProfile {
    pub protocol: u32,
    pub node: NodeId,
    /// Display name for this node — the machine's hostname by default, or a
    /// user-set override. When it differs from `hostname`, the UI renders
    /// "label (hostname)" so the real machine is always visible.
    pub label: String,
    /// The node's real machine hostname, always straight from its own scan.
    /// `#[serde(default)]` so presence from an older peer (no hostname field)
    /// still decodes — the UI just falls back to `label`.
    #[serde(default)]
    pub hostname: String,
    pub summary: InventorySummary,
    /// The capabilities this node is willing to expose. The owner curates
    /// this; nothing here is reachable without the receiver's catalog also
    /// authorizing the route.
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    /// Who owns this device, if it has recorded an owner — the node id that
    /// claimed or was configured to own it. A peer reads this to decide
    /// whether the device is *theirs* (owner == them) or someone else's;
    /// a device you don't own can't be silently adopted. `None` = unowned.
    /// `#[serde(default)]` so presence from an older peer still decodes.
    #[serde(default)]
    pub owner: Option<NodeId>,
    /// `true` only when this device was started in **claim mode** and has no
    /// owner yet — i.e. it is *offering* itself to be adopted. Claiming is
    /// refused unless this is set, so you can't flat-out take a device that
    /// hasn't been put up for adoption (it defines its own owner instead).
    #[serde(default)]
    pub claimable: bool,
}

/// Point-to-point control traffic. Tagged on `t` so route, share, and
/// ownership negotiation share one channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ControlMessage {
    Route(RouteControl),
    Share(ShareControl),
    Ownership(OwnershipControl),
}

/// Lifecycle of a single cross-node route. The sourcing side offers; the
/// other side accepts to start media flowing, or rejects with a reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RouteControl {
    /// "I'd like to connect this." Carries the full route so the receiver
    /// can show exactly what's being asked and check it against its own
    /// catalog before accepting.
    Offer { route: Route },
    /// "Go ahead" — media may start.
    Accept { route_id: String },
    /// "No" — with a human reason ("not authorized", "device busy").
    Reject { route_id: String, reason: String },
    /// "Stop" — either side can tear a live route down.
    Teardown { route_id: String },
}

/// Negotiating a *shared* relationship and its grants. This is how the
/// "is this someone I'm sharing with?" answer becomes a mutual fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ShareControl {
    /// "I'd like to share some things with you." Offered grants describe
    /// what the inviter is willing to let the invitee do.
    Invite {
        from: Person,
        #[serde(default)]
        grants: Vec<Grant>,
    },
    /// The invitee accepts the share (optionally proposing grants back, so
    /// sharing can be mutual in one round trip).
    Accept {
        #[serde(default)]
        grants: Vec<Grant>,
    },
    /// The invitee declines.
    Decline,
    /// Either side withdraws a previously-granted permission. Revocation
    /// is unilateral — you can always take back your own stuff.
    Revoke { grant_id: String },
}

/// Adopting an unowned device that's been put up for adoption. Ownership is
/// the answer to "whose machine is this?" — and unlike a share, you can't
/// assert it unilaterally: the device must be in claim mode (advertising
/// [`NodeProfile::claimable`]) for a claim to take. This keeps a stranger
/// on the mesh from flat-out grabbing a box that already has an owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OwnershipControl {
    /// "I'm adopting you." Sent to a claimable device; `owner` is the
    /// claimer's node id. The device records it (if still claimable),
    /// stops being claimable, and re-advertises presence with the new
    /// owner — that presence is the authoritative confirmation.
    Claim { owner: NodeId },
    /// The device confirms the adoption took — it's now owned by `owner`.
    Claimed { owner: NodeId },
    /// The device refuses: it isn't in claim mode, or already has an owner.
    Declined { reason: String },
    /// The owner relinquishes the device, returning it to unowned. Only the
    /// current owner's release is honoured.
    Release,
}

#[cfg(test)]
mod tests {
    use super::*;
    use allmystuff_graph::{Flow, GrantRole, MediaKind};

    #[test]
    fn node_profile_round_trips() {
        let p = NodeProfile {
            protocol: PROTOCOL_VERSION,
            node: "desk".into(),
            label: "Desk PC".into(),
            hostname: "desk-pc.local".into(),
            summary: InventorySummary {
                os: "linux".into(),
                cpu: "Test CPU".into(),
                ram_bytes: 16 << 30,
                device_count: 12,
            },
            capabilities: vec![Capability::new(
                "desk",
                "desk:mic",
                "Mic",
                MediaKind::Audio,
                Flow::Source,
                "microphone",
            )],
            owner: Some("my-laptop".into()),
            claimable: false,
        };
        let s = serde_json::to_string(&p).unwrap();
        let back: NodeProfile = serde_json::from_str(&s).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn presence_without_ownership_fields_still_decodes() {
        // An older peer's advert has no owner/claimable — they default.
        let json = r#"{
            "protocol": 1, "node": "old", "label": "Old", "hostname": "old",
            "summary": {"os":"linux","cpu":"cpu","ram_bytes":1,"device_count":1}
        }"#;
        let p: NodeProfile = serde_json::from_str(json).unwrap();
        assert_eq!(p.owner, None);
        assert!(!p.claimable);
    }

    #[test]
    fn ownership_claim_round_trips_and_tags() {
        let m = ControlMessage::Ownership(OwnershipControl::Claim {
            owner: "my-laptop".into(),
        });
        let j = serde_json::to_value(&m).unwrap();
        assert_eq!(j["t"], "ownership");
        assert_eq!(j["kind"], "claim");
        assert_eq!(j["owner"], "my-laptop");
        let back: ControlMessage = serde_json::from_value(j).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn control_message_tags_are_stable() {
        let m = ControlMessage::Route(RouteControl::Reject {
            route_id: "r1".into(),
            reason: "not authorized".into(),
        });
        let j = serde_json::to_value(&m).unwrap();
        assert_eq!(j["t"], "route");
        assert_eq!(j["kind"], "reject");
        assert_eq!(j["reason"], "not authorized");
    }

    #[test]
    fn share_invite_carries_grants() {
        let m = ControlMessage::Share(ShareControl::Invite {
            from: Person {
                id: "person:me".into(),
                name: "Me".into(),
            },
            grants: vec![Grant {
                id: "g1".into(),
                media: MediaKind::Video,
                role: GrantRole::Consume,
                capability: None,
                label: "Receive your screen".into(),
            }],
        });
        let j = serde_json::to_value(&m).unwrap();
        assert_eq!(j["t"], "share");
        assert_eq!(j["kind"], "invite");
        let back: ControlMessage = serde_json::from_value(j).unwrap();
        assert_eq!(m, back);
    }
}
