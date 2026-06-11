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

/// Typed-channel name for the **owned-fleet** roster gossip. When you adopt
/// a device, the two machines start sharing an [`OwnedRoster`] on this
/// channel — the list of devices one owner has claimed, all linked by a
/// single shared key. It rides the daemon's typed channels exactly like
/// presence, and converges by version the same way a mesh roster does.
pub const CHANNEL_OWNED: &str = "allmystuff/owned/v1";

/// Feature tag a node advertises in [`NodeProfile::features`] when it can
/// host mesh-native terminal sessions (spawn a PTY and stream it over the
/// media channel). A peer only offers a terminal route to nodes that
/// advertise this.
pub const FEATURE_TERMINAL: &str = "terminal";

/// Feature tag a node advertises in [`NodeProfile::features`] when it can
/// host mesh-native file sessions (browse, read and manage its filesystem
/// over the media channel — the "Open Files" console). A peer only offers
/// a files route to nodes that advertise this.
pub const FEATURE_FILES: &str = "files";

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
    /// Random id minted once per app run. Gossip is **event-driven, not a
    /// heartbeat**: a receiver that sees a boot id it hasn't recorded for
    /// this peer knows the peer just (re)started and missed our earlier
    /// adverts, and answers with its own presence + roster directly — so
    /// state converges on the events that change it, with no periodic
    /// re-broadcast bloating the mesh. `0` = an older peer without the
    /// field (those still heartbeat, so no reply is needed).
    #[serde(default)]
    pub boot: u64,
    /// App features this node supports beyond the v1 baseline — e.g.
    /// [`FEATURE_TERMINAL`]. Unknown entries are ignored; absent (an older
    /// peer) decodes as empty, and empty serializes *without* the key so an
    /// older receiver sees exactly the presence shape it always did. A
    /// feature is only ever offered to a peer that advertises it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
}

/// One device in an owned fleet — a machine the same owner has claimed, so
/// it shares the fleet's key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedMember {
    /// The member's stable device id. Stored in canonical (bare-pubkey) form
    /// so a display-id and bare-pubkey view of one machine collapse to a
    /// single member.
    pub device: NodeId,
    /// Best-known display label for the member (cosmetic; the newest non-empty
    /// label a gossip carries wins).
    #[serde(default)]
    pub label: String,
}

/// The gossiped roster of an owner's fleet: a shared key that links every
/// co-owned device, a monotonically-increasing version for last-writer-wins
/// convergence, and the members themselves.
///
/// The key is, for now, an **internal grouping secret** — every device in the
/// fleet holds the same one, minted by the first owner to claim a device and
/// handed down on each adoption. A later edition lets the user link that key
/// to other things; today it exists only to group co-owned devices. It is
/// gossiped on [`CHANNEL_OWNED`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct OwnedRoster {
    /// Shared fleet key. Empty means "no fleet yet".
    #[serde(default)]
    pub key: String,
    /// Bumped on every membership change so peers converge on the newest copy.
    #[serde(default)]
    pub version: u64,
    /// Every device the owner has claimed (and the owner itself).
    #[serde(default)]
    pub members: Vec<OwnedMember>,
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
    Offer {
        route: Route,
        /// Video transports the *offerer* can consume for a display
        /// route, best first (today: `"h264"` — the mesh's RTP track
        /// lane). The accepting side — the machine whose screen will
        /// stream — picks the best one it can produce, falling back to
        /// MJPEG over the media channel when the list is empty or
        /// nothing matches. Absent on v0.1.x offers (`default`) and
        /// ignored by v0.1.x receivers: both skews degrade to MJPEG,
        /// never to a broken stream.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        video: Vec<String>,
        /// Audio transports the *offerer* can consume for an audio
        /// route, best first (today: `"opus"` — the mesh's RTP audio
        /// lane). Same degradation contract as `video`: absent or
        /// unrecognized on either side means PCM frames over the media
        /// channel, never a broken stream. Only meaningful when the
        /// offerer is the route's sink (the console's listen leg).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        audio: Vec<String>,
    },
    /// "Go ahead" — media may start.
    Accept { route_id: String },
    /// "No" — with a human reason ("not authorized", "device busy").
    Reject { route_id: String, reason: String },
    /// "Stop" — either side can tear a live route down.
    Teardown { route_id: String },
    /// "Give me a clean decode entry *now*" — the viewer's decoder lost
    /// its place (a decode error, a rebuilt decoder) and shouldn't sit
    /// out the rest of the periodic IDR interval. The streaming side
    /// forces an IDR on its next capture. Unknown to v0.2.x peers (the
    /// whole message fails their decode and is dropped): recovery then
    /// simply waits for the periodic IDR, as before.
    Refresh { route_id: String },
    /// "Stream with these settings" — the viewer's quality picks for a
    /// display route it consumes; the streaming side restarts its
    /// capture with the overrides. `None` everywhere = automatic (the
    /// streamer's own budget). Unknown to v0.2.x peers and dropped,
    /// leaving their stream on automatic.
    Tune {
        route_id: String,
        /// Longest output edge in pixels (e.g. 1920); `None` = native.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_edge: Option<u32>,
        /// H.264 target bitrate in bits/second; `None` = pixel-budgeted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bitrate: Option<u32>,
        /// Capture rate ceiling; `None` = the streamer's default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fps: Option<u32>,
    },
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
            boot: 7,
            features: vec![FEATURE_TERMINAL.into()],
        };
        let s = serde_json::to_string(&p).unwrap();
        let back: NodeProfile = serde_json::from_str(&s).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn presence_features_accept_skew_both_ways() {
        // An older peer's advert has no `features` — it decodes as empty
        // rather than failing (so the node never vanishes from the graph).
        let json = r#"{
            "protocol": 1, "node": "old", "label": "Old", "hostname": "old",
            "summary": {"os":"linux","cpu":"cpu","ram_bytes":1,"device_count":1}
        }"#;
        let p: NodeProfile = serde_json::from_str(json).unwrap();
        assert!(p.features.is_empty());

        // Empty features serialize *without* the key, so an older receiver
        // sees exactly the presence shape it always did.
        let s = serde_json::to_string(&p).unwrap();
        assert!(!s.contains("features"));

        // A populated list round-trips.
        let p = NodeProfile {
            features: vec![FEATURE_TERMINAL.into()],
            ..p
        };
        let s = serde_json::to_string(&p).unwrap();
        assert!(s.contains("\"features\":[\"terminal\"]"));
        let back: NodeProfile = serde_json::from_str(&s).unwrap();
        assert_eq!(back.features, vec![FEATURE_TERMINAL.to_string()]);
    }

    #[test]
    fn route_offer_video_accepts_skew_both_ways() {
        // A v0.1.x offer has no `video` field — it decodes as "MJPEG
        // only" rather than failing.
        let legacy = r#"{"kind":"offer","route":{
            "id":"r1","from":"a:screen","to":"b:view","media":"display"
        }}"#;
        let rc: RouteControl = serde_json::from_str(legacy).unwrap();
        assert!(matches!(rc, RouteControl::Offer { ref video, .. } if video.is_empty()));

        // An empty accepts list serializes *without* the field, so a
        // v0.1.x receiver sees exactly the shape it always did.
        let s = serde_json::to_string(&rc).unwrap();
        assert!(!s.contains("video"));

        // A populated list round-trips.
        let offered = match rc {
            RouteControl::Offer { route, .. } => RouteControl::Offer {
                route,
                video: vec!["h264".into()],
                audio: Vec::new(),
            },
            _ => unreachable!(),
        };
        let s = serde_json::to_string(&offered).unwrap();
        assert!(s.contains("\"video\":[\"h264\"]"));
        let back: RouteControl = serde_json::from_str(&s).unwrap();
        assert_eq!(offered, back);
    }

    #[test]
    fn route_offer_audio_accepts_skew_both_ways() {
        // The audio accepts ride the same contract as video's: absent
        // decodes as "PCM channel only", empty serializes invisibly,
        // populated round-trips.
        let legacy = r#"{"kind":"offer","route":{
            "id":"r1","from":"a:system-audio","to":"b:system-audio","media":"audio"
        }}"#;
        let rc: RouteControl = serde_json::from_str(legacy).unwrap();
        assert!(matches!(rc, RouteControl::Offer { ref audio, .. } if audio.is_empty()));
        let s = serde_json::to_string(&rc).unwrap();
        assert!(!s.contains("audio\":["));

        let offered = match rc {
            RouteControl::Offer { route, .. } => RouteControl::Offer {
                route,
                video: Vec::new(),
                audio: vec!["opus".into()],
            },
            _ => unreachable!(),
        };
        let s = serde_json::to_string(&offered).unwrap();
        assert!(s.contains("\"audio\":[\"opus\"]"));
        let back: RouteControl = serde_json::from_str(&s).unwrap();
        assert_eq!(offered, back);
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
        assert_eq!(
            p.boot, 0,
            "an older peer reads as boot 0 (heartbeats instead)"
        );
    }

    #[test]
    fn owned_roster_round_trips() {
        let r = OwnedRoster {
            key: "a1b2c3".into(),
            version: 4,
            members: vec![
                OwnedMember {
                    device: "my-laptop".into(),
                    label: "My laptop".into(),
                },
                OwnedMember {
                    device: "spare-nuc".into(),
                    label: "Spare NUC".into(),
                },
            ],
        };
        let s = serde_json::to_string(&r).unwrap();
        let back: OwnedRoster = serde_json::from_str(&s).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn owned_roster_tolerates_a_minimal_advert() {
        // A member from an older peer may carry just the device id.
        let json = r#"{ "key": "k", "members": [{ "device": "d" }] }"#;
        let r: OwnedRoster = serde_json::from_str(json).unwrap();
        assert_eq!(r.version, 0);
        assert_eq!(r.members.len(), 1);
        assert_eq!(r.members[0].label, "");
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
