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

/// Typed-channel name for **virtual rooms** — the lightweight membership +
/// chat plane of a room (the media itself rides ordinary routes). Only
/// peers that advertise [`FEATURE_ROOMS`] subscribe; an older peer never
/// sees the channel, so the whole plane is additive.
pub const CHANNEL_ROOMS: &str = "allmystuff/rooms/v1";

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

/// Feature tag a node advertises in [`NodeProfile::features`] when it
/// speaks the virtual-rooms plane ([`CHANNEL_ROOMS`]): room invites, join /
/// leave presence, and chat. The room UI badges members without it (an
/// older build) so nobody wonders why a message went unanswered.
pub const FEATURE_ROOMS: &str = "rooms";

/// Feature tag a node advertises in [`NodeProfile::features`] when it can
/// host **sites** — reverse-proxy a TCP service it's listening on (a local
/// web app, a database) over the mesh to a peer, who reaches it through a
/// locally-mapped port. The advertised [`NodeProfile::sites`] are the *only*
/// ports such a host will proxy; a peer only offers a site route to a node
/// that advertises this tag, and an older build never sees the plane.
pub const FEATURE_SITES: &str = "sites";

/// Feature tag a node advertises in [`NodeProfile::features`] when it can
/// *stream* its cameras (a capture backend feeds its video routes). The
/// cameras themselves have always ridden presence as capabilities; this
/// says selecting one will actually produce pixels — a console pointed at
/// an older build's camera explains "update that machine" instead of
/// waiting on a stream that will never start.
pub const FEATURE_CAMERA: &str = "camera";

/// Feature tag a node advertises in [`NodeProfile::features`] when its local
/// daemon provisions the **media lane pool** (myownmesh ≥ 0.2.7): several
/// independent video/audio RTP tracks per peer, so a sender can fan several
/// simultaneous streams to one peer (two screens of one machine) onto
/// separate lanes instead of one. A sender only routes a second+ stream to a
/// lane past 0 when the receiver advertises this; otherwise the extra stream
/// falls back to MJPEG, exactly as before the pool.
pub const FEATURE_MEDIA_LANES: &str = "media-lanes";

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
    /// The **sites** this node exposes — TCP services it's willing to
    /// reverse-proxy over the mesh (see [`SiteAdvert`]). The owner curates
    /// this; it's the exhaustive set a peer may ask the host to proxy, so a
    /// connection to anything *not* listed is refused (the advert is the
    /// allow-list, not just a hint). Additive: absent (an older peer)
    /// decodes as empty, and empty serializes *without* the key, so the
    /// presence shape an older receiver sees is unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sites: Vec<SiteAdvert>,
    /// The AllMyStuff version this node is running — its binary's
    /// `CARGO_PKG_VERSION` (e.g. `"0.1.11"`). It lets a peer notice that one
    /// of its own machines is behind the channel's latest release and offer
    /// to upgrade it ([`AppControl::Upgrade`]). Absent from an older peer
    /// (`default`) decodes as empty — "unknown" — so the upgrade affordance
    /// simply never appears for it; empty serializes *without* the key, so an
    /// older receiver sees exactly the presence shape it always did.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version: String,
}

/// One site a node exposes — a TCP service it's listening on that it's
/// willing to reverse-proxy over the mesh. Rides the presence advert
/// ([`NodeProfile::sites`]); the bytes themselves never touch presence (a
/// connection is a route, tunneled on the media channel). The set is the
/// host's allow-list: it only proxies a port that appears here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteAdvert {
    /// Stable id, mirroring the scan's `ListeningService.id` (`tcp:8080`),
    /// so a mapped site keeps its identity across rescans and restarts.
    pub id: String,
    /// Friendly label — "HTTP", "PostgreSQL", or "Port 8080".
    pub label: String,
    /// The TCP port the host is listening on (and will proxy to on
    /// loopback). The host re-checks an inbound request's port against this
    /// set before connecting, so a peer can't pivot to an unadvertised port.
    pub port: u16,
    /// URL scheme a client reaches it with — "http", "https", "ssh", … — or
    /// empty for a bare TCP service the proxy still tunnels. A web scheme
    /// (`http`/`https`) is what lets the UI offer "open in browser".
    #[serde(default)]
    pub scheme: String,
    /// `true` when the host bound it to loopback only — the prime
    /// reverse-proxy case (not reachable on its LAN, but the mesh carries
    /// it). Cosmetic; the proxy works the same either way.
    #[serde(default)]
    pub loopback: bool,
}

impl SiteAdvert {
    /// `true` for a web service the UI can "open in browser" — its scheme is
    /// `http` or `https`.
    pub fn is_web(&self) -> bool {
        self.scheme == "http" || self.scheme == "https"
    }
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
    /// The fleet's display name — whatever its owner answers to ("Casey").
    /// Cosmetic and gossiped with the roster, converging by the same
    /// version. Empty = unnamed, and an empty name is skipped on the wire
    /// so an older peer sees exactly the roster shape it always did.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Bumped on every membership change so peers converge on the newest copy.
    #[serde(default)]
    pub version: u64,
    /// Every device the owner has claimed (and the owner itself).
    #[serde(default)]
    pub members: Vec<OwnedMember>,
}

/// One file a member offers into a room's **Shared Files** area, as the
/// uploader states it to the host. `token` is an opaque fetch handle the
/// uploader minted (it round-trips back as the files plane's `Fetch`
/// request); the bytes never touch the host — a downloader pulls them
/// straight from the uploader over a `:shared` route, gated on the token
/// and the room's member set. `size` is the file's byte length, for the
/// progress bar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedFileMeta {
    pub token: String,
    pub name: String,
    #[serde(default)]
    pub size: u64,
}

/// One entry of the host's aggregated Shared Files list — a
/// [`SharedFileMeta`] tagged with the uploader so a downloader knows whom
/// to fetch the bytes from. The host hosts the *list*; the uploader hosts
/// the *bytes* (and only while it's online).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedEntry {
    /// The uploader (canonical node id) — whom to open the `:shared` route to.
    pub from: NodeId,
    pub token: String,
    pub name: String,
    #[serde(default)]
    pub size: u64,
}

/// One message of the virtual-rooms plane, carried on [`CHANNEL_ROOMS`].
/// A room itself is a lightweight, user-minted thing — a stable id, a
/// cosmetic name, and a member list — and every message restates the id
/// and name so a receiver can render a room it has never heard of (an
/// invite that raced a chat line, a member that reinstalled). The media
/// of a room (mic, screen share, …) is **not** here: those are ordinary
/// routes, proposed and authorized exactly like any other connection.
///
/// A room is **hosted by its maker**: the id is minted under the maker's
/// canonical device id (`room:{owner}:{nonce}`), and the room's control
/// plane — its roster and name (the [`RoomEvent::Invite`] replacement) and
/// its end of life ([`RoomEvent::Close`]) — is honoured only from that
/// host (the mesh authenticates senders, so this is a real check, not a
/// label). Members talk to each other directly for everything streamed
/// (join/leave presence, chat, the media routes) — nothing flows *through*
/// the host — and a room is stream-only: nothing is stored, and any future
/// history would live with the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomMessage {
    /// The room's stable id (minted by its creator).
    pub room: String,
    /// The room's display name, restated on every message.
    #[serde(default)]
    pub name: String,
    /// What happened.
    #[serde(flatten)]
    pub event: RoomEvent,
}

/// How a room admits a machine that asks to join ([`RoomEvent::Knock`])
/// without holding an invite. The host states it on every
/// [`RoomEvent::Invite`]; only the host ever enforces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RoomAccess {
    /// Anyone on a shared mesh who knocks with the room's id is admitted
    /// automatically — the id *is* the invite.
    Open,
    /// The host admits each knock by hand. The default, and what an older
    /// host's invites (no field on the wire) read as.
    #[default]
    Invite,
}

/// The events of a room's membership + chat plane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RoomEvent {
    /// "You're in this room." Sent by the **host** on creation and on
    /// every roster or name change, carrying the full member list —
    /// replacement semantics, so removals propagate (a member that's no
    /// longer listed drops the room). Receivers ignore invites for a
    /// known room from anyone but its host. Once received, the room stays
    /// on the member's device like a roster slot — it's listed (and can be
    /// rejoined) until the host removes them or closes the room.
    Invite {
        members: Vec<NodeId>,
        /// How the room treats knocks. Absent on an older host's invites
        /// (`default`), which reads as invite-only — never more open.
        #[serde(default)]
        access: RoomAccess,
    },
    /// The sender opened the room — they're present in the call now.
    Join,
    /// The sender left the room (hung up).
    Leave,
    /// A chat line from the sender.
    Chat { text: String },
    /// The **host** closed the room for everyone — members drop it.
    /// (From anyone else it's ignored. An older build doesn't know the
    /// kind and drops the whole message: it just keeps a dead room
    /// listed until the user forgets it locally.)
    Close,
    /// "May I join?" — sent **to the host** by a machine holding the
    /// room's id but no invite (the id was shared out-of-band and pasted
    /// into the rooms UI). An [`RoomAccess::Open`] host admits at once by
    /// re-stating the roster (an [`RoomEvent::Invite`] listing the
    /// knocker); an invite-only host surfaces it for a human to admit or
    /// deny. An older host drops the whole message — the knock simply
    /// goes unanswered, like knocking on a door nobody's behind.
    Knock,
    /// The host's "no" to a knock, so the asker isn't left waiting.
    Deny,
    /// A member tells the **host** the files it's currently offering into
    /// the room's Shared Files area — replacement semantics (the member's
    /// full current list each time). The host aggregates every member's
    /// list and restates the whole as [`RoomEvent::Shares`]. The bytes
    /// never travel through the host: a downloader fetches them straight
    /// from the uploader over a `:shared` route. Sent member→host; ignored
    /// by anyone that isn't the room's host. An older host drops the whole
    /// message — its members simply see no shared files.
    ShareList { files: Vec<SharedFileMeta> },
    /// The **host's** authoritative Shared Files list for the room: every
    /// online member's offerings, each tagged with the uploader so a
    /// downloader knows whom to fetch from. Restated on every change and to
    /// each new joiner, exactly like the roster ([`RoomEvent::Invite`]);
    /// members ignore it from anyone but the host. Replacement semantics —
    /// an uploader that's gone offline simply drops off the next list.
    Shares { files: Vec<SharedEntry> },
}

/// Point-to-point control traffic. Tagged on `t` so route, share,
/// ownership, site management, and app-level commands share one channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ControlMessage {
    Route(RouteControl),
    Share(ShareControl),
    Ownership(OwnershipControl),
    Site(SiteControl),
    App(AppControl),
}

/// One listening service on a machine, as reported to a co-owned fleet
/// member managing it remotely ([`SiteControl::Sites`]). Mirrors the scan's
/// `ListeningService` without the protocol crate depending on the inventory
/// crate — the backend fills it from a scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteService {
    pub id: String,
    pub name: String,
    pub port: u16,
    #[serde(default)]
    pub scheme: String,
    #[serde(default)]
    pub loopback: bool,
    #[serde(default)]
    pub process: String,
    /// The page `<title>` the probe fetched (http), a default-name hint.
    #[serde(default)]
    pub title: String,
}

/// Remotely managing a co-owned machine's sites — what powers the "Its
/// sites" controls in a fleet device's drawer. Authorized exactly like the
/// site proxy and the terminal: only the device's owner or a fleet member is
/// answered (the mesh authenticates the sender), so a stranger can't list or
/// re-expose your services. An older peer that doesn't know the `site` tag
/// drops the whole control message ([`ControlMessage`] fails to decode) —
/// the manager just sees no remote sites, never an error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SiteControl {
    /// "List your sites" — a fleet member asking what this machine is
    /// listening on and what it currently exposes, to manage it.
    List,
    /// The answer: every discovered service, plus the current exposed map
    /// (id → advertised name).
    Sites {
        services: Vec<SiteService>,
        #[serde(default)]
        exposed: std::collections::BTreeMap<String, String>,
    },
    /// "Advertise exactly these" — the new exposed map (id → name) for this
    /// machine to publish. Applied only from the owner/fleet; the machine
    /// persists it and re-broadcasts presence.
    SetExposed {
        #[serde(default)]
        exposed: std::collections::BTreeMap<String, String>,
    },
}

/// App-level commands one of *your own* machines asks another to perform on
/// itself — things outside the route/share/ownership lifecycle. The receiver
/// enforces that the sender is its owner or a fleet co-member before acting
/// (the same rule that gates a terminal or remote-control session), so a
/// stranger on the mesh can never drive it. An older peer doesn't know the
/// `app` tag and drops the whole message, so a command simply goes
/// unanswered there — never misinterpreted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AppControl {
    /// "Update yourself and restart." Sent to a fleet machine running an
    /// AllMyStuff older than the channel's latest release. The receiver runs
    /// its self-updater and, if a newer build was applied, relaunches — its
    /// next presence advert (carrying the new [`NodeProfile::version`]) is
    /// the confirmation, exactly as a claim confirms by re-advertising its
    /// new owner.
    Upgrade,
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
            sites: vec![SiteAdvert {
                id: "tcp:8080".into(),
                label: "HTTP".into(),
                port: 8080,
                scheme: "http".into(),
                loopback: true,
            }],
            version: "0.1.11".into(),
        };
        let s = serde_json::to_string(&p).unwrap();
        let back: NodeProfile = serde_json::from_str(&s).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn presence_sites_accept_skew_both_ways() {
        // An older peer's advert has no `sites` — it decodes as empty
        // rather than failing (the node never vanishes from the graph).
        let json = r#"{
            "protocol": 1, "node": "old", "label": "Old", "hostname": "old",
            "summary": {"os":"linux","cpu":"cpu","ram_bytes":1,"device_count":1}
        }"#;
        let p: NodeProfile = serde_json::from_str(json).unwrap();
        assert!(p.sites.is_empty());

        // No sites serializes *without* the key, so an older receiver sees
        // exactly the presence shape it always did.
        let s = serde_json::to_string(&p).unwrap();
        assert!(!s.contains("sites"));

        // A populated list round-trips, and `is_web` keys on the scheme.
        let p = NodeProfile {
            sites: vec![
                SiteAdvert {
                    id: "tcp:5432".into(),
                    label: "PostgreSQL".into(),
                    port: 5432,
                    scheme: "postgres".into(),
                    loopback: true,
                },
                SiteAdvert {
                    id: "tcp:443".into(),
                    label: "HTTPS".into(),
                    port: 443,
                    scheme: "https".into(),
                    loopback: false,
                },
            ],
            ..p
        };
        let s = serde_json::to_string(&p).unwrap();
        assert!(s.contains("\"sites\""));
        let back: NodeProfile = serde_json::from_str(&s).unwrap();
        assert_eq!(back.sites, p.sites);
        assert!(!back.sites[0].is_web(), "postgres isn't web");
        assert!(back.sites[1].is_web(), "https is web");
    }

    #[test]
    fn presence_version_accepts_skew_both_ways() {
        // An older peer's advert has no `version` — it decodes as empty
        // ("unknown") rather than failing, so the node never vanishes.
        let json = r#"{
            "protocol": 1, "node": "old", "label": "Old", "hostname": "old",
            "summary": {"os":"linux","cpu":"cpu","ram_bytes":1,"device_count":1}
        }"#;
        let p: NodeProfile = serde_json::from_str(json).unwrap();
        assert!(p.version.is_empty());

        // Empty version serializes *without* the key, so an older receiver
        // sees exactly the presence shape it always did.
        let s = serde_json::to_string(&p).unwrap();
        assert!(!s.contains("version"));

        // A populated version round-trips.
        let p = NodeProfile {
            version: "0.2.0".into(),
            ..p
        };
        let s = serde_json::to_string(&p).unwrap();
        assert!(s.contains("\"version\":\"0.2.0\""));
        let back: NodeProfile = serde_json::from_str(&s).unwrap();
        assert_eq!(back.version, "0.2.0");
    }

    #[test]
    fn app_control_upgrade_round_trips() {
        let msg = ControlMessage::App(AppControl::Upgrade);
        let s = serde_json::to_string(&msg).unwrap();
        // Tagged `t: "app"` at the outer level, `kind: "upgrade"` within.
        assert!(s.contains("\"t\":\"app\""));
        assert!(s.contains("\"kind\":\"upgrade\""));
        let back: ControlMessage = serde_json::from_str(&s).unwrap();
        assert_eq!(msg, back);
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
            name: "Casey".into(),
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
        // A member from an older peer may carry just the device id — and no
        // fleet name (the field postdates them).
        let json = r#"{ "key": "k", "members": [{ "device": "d" }] }"#;
        let r: OwnedRoster = serde_json::from_str(json).unwrap();
        assert_eq!(r.version, 0);
        assert_eq!(r.members.len(), 1);
        assert_eq!(r.members[0].label, "");
        assert_eq!(r.name, "");

        // An unnamed fleet serializes *without* the key, so an older
        // receiver sees exactly the roster shape it always did.
        let s = serde_json::to_string(&r).unwrap();
        assert!(!s.contains("name"));
    }

    #[test]
    fn room_messages_round_trip_and_tag() {
        let m = RoomMessage {
            room: "room:abc".into(),
            name: "Movie night".into(),
            event: RoomEvent::Chat {
                text: "hi all".into(),
            },
        };
        let j = serde_json::to_value(&m).unwrap();
        assert_eq!(j["room"], "room:abc");
        assert_eq!(j["name"], "Movie night");
        assert_eq!(j["kind"], "chat");
        assert_eq!(j["text"], "hi all");
        let back: RoomMessage = serde_json::from_str(&j.to_string()).unwrap();
        assert_eq!(m, back);

        let m = RoomMessage {
            room: "room:abc".into(),
            name: "Movie night".into(),
            event: RoomEvent::Invite {
                members: vec!["a".into(), "b".into()],
                access: RoomAccess::Open,
            },
        };
        let j = serde_json::to_value(&m).unwrap();
        assert_eq!(j["access"], "open");
        let back: RoomMessage = serde_json::from_str(&j.to_string()).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn room_invite_without_access_reads_invite_only() {
        // An older host's invite carries no `access` — it must never read
        // as more open than the host meant.
        let json = r#"{ "room": "room:abc", "name": "Movie night",
                        "kind": "invite", "members": ["a"] }"#;
        let m: RoomMessage = serde_json::from_str(json).unwrap();
        assert_eq!(
            m.event,
            RoomEvent::Invite {
                members: vec!["a".into()],
                access: RoomAccess::Invite,
            }
        );
    }

    #[test]
    fn room_knock_and_deny_round_trip() {
        for (event, kind) in [(RoomEvent::Knock, "knock"), (RoomEvent::Deny, "deny")] {
            let m = RoomMessage {
                room: "room:owner:ab12cd34".into(),
                name: String::new(),
                event,
            };
            let j = serde_json::to_value(&m).unwrap();
            assert_eq!(j["kind"], kind);
            let back: RoomMessage = serde_json::from_str(&j.to_string()).unwrap();
            assert_eq!(m, back);
        }
    }

    #[test]
    fn room_shared_files_round_trip_and_tag() {
        // A member's offering to the host.
        let m = RoomMessage {
            room: "room:owner:ab12cd34".into(),
            name: String::new(),
            event: RoomEvent::ShareList {
                files: vec![SharedFileMeta {
                    token: "share_xyz".into(),
                    name: "deck.pdf".into(),
                    size: 4096,
                }],
            },
        };
        let j = serde_json::to_value(&m).unwrap();
        assert_eq!(j["kind"], "share_list");
        assert_eq!(j["files"][0]["token"], "share_xyz");
        let back: RoomMessage = serde_json::from_str(&j.to_string()).unwrap();
        assert_eq!(m, back);

        // The host's aggregated list, tagged with each uploader.
        let m = RoomMessage {
            room: "room:owner:ab12cd34".into(),
            name: "Movie night".into(),
            event: RoomEvent::Shares {
                files: vec![SharedEntry {
                    from: "alex".into(),
                    token: "share_xyz".into(),
                    name: "deck.pdf".into(),
                    size: 4096,
                }],
            },
        };
        let j = serde_json::to_value(&m).unwrap();
        assert_eq!(j["kind"], "shares");
        assert_eq!(j["files"][0]["from"], "alex");
        let back: RoomMessage = serde_json::from_str(&j.to_string()).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn room_close_round_trips() {
        let m = RoomMessage {
            room: "room:owner:ab12cd34".into(),
            name: "Movie night".into(),
            event: RoomEvent::Close,
        };
        let j = serde_json::to_value(&m).unwrap();
        assert_eq!(j["kind"], "close");
        let back: RoomMessage = serde_json::from_str(&j.to_string()).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn room_message_tolerates_a_minimal_advert() {
        // A join from a build that skipped the name still decodes.
        let json = r#"{ "room": "room:abc", "kind": "join" }"#;
        let m: RoomMessage = serde_json::from_str(json).unwrap();
        assert_eq!(m.room, "room:abc");
        assert_eq!(m.name, "");
        assert_eq!(m.event, RoomEvent::Join);
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
    fn site_control_round_trips_and_tags() {
        // The "list your sites" request.
        let m = ControlMessage::Site(SiteControl::List);
        let j = serde_json::to_value(&m).unwrap();
        assert_eq!(j["t"], "site");
        assert_eq!(j["kind"], "list");
        assert_eq!(serde_json::from_value::<ControlMessage>(j).unwrap(), m);

        // The reply, carrying the full service list + the exposed map.
        let m = ControlMessage::Site(SiteControl::Sites {
            services: vec![SiteService {
                id: "tcp:3000".into(),
                name: "HTTP".into(),
                port: 3000,
                scheme: "http".into(),
                loopback: true,
                process: "grafana".into(),
                title: "My Grafana".into(),
            }],
            exposed: std::collections::BTreeMap::from([("tcp:3000".into(), "My Grafana".into())]),
        });
        let j = serde_json::to_value(&m).unwrap();
        assert_eq!(j["kind"], "sites");
        assert_eq!(j["services"][0]["port"], 3000);
        assert_eq!(j["exposed"]["tcp:3000"], "My Grafana");
        assert_eq!(serde_json::from_value::<ControlMessage>(j).unwrap(), m);

        // The "advertise exactly these" command.
        let m = ControlMessage::Site(SiteControl::SetExposed {
            exposed: std::collections::BTreeMap::from([("tcp:8080".into(), "App".into())]),
        });
        let j = serde_json::to_value(&m).unwrap();
        assert_eq!(j["kind"], "set_exposed");
        assert_eq!(serde_json::from_value::<ControlMessage>(j).unwrap(), m);
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
