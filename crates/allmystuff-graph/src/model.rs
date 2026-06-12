//! The vocabulary of the graph: nodes, capabilities, the routes that
//! wire capabilities together, and the relationships + grants that
//! decide who's allowed to do what.
//!
//! Everything here is plain data with stable string ids, so the whole
//! model round-trips through JSON to the Svelte front-end and (eventually)
//! across the mesh to a peer.

use serde::{Deserialize, Serialize};

// ---- ids --------------------------------------------------------------
//
// Transparent string newtypes — they serialise as bare strings (clean
// TypeScript interop) but stay distinct in Rust so a NodeId can't be
// passed where a CapabilityId is wanted.

macro_rules! string_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_string())
            }
        }
        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

string_id!(
    /// A machine on the mesh. Mirrors a MyOwnMesh device id; the local
    /// machine uses the reserved id [`NodeId::THIS`].
    NodeId
);
string_id!(
    /// A routable endpoint on a node — a physical device (this mic, that
    /// display) or a synthetic machine capability (this computer's screen,
    /// keyboard control). Namespaced by node so the same inventory id on
    /// two machines stays distinct.
    CapabilityId
);
string_id!(
    /// A human you share with. Distinct from a node — one person may bring
    /// several machines.
    PersonId
);

impl NodeId {
    /// The local machine. Routes and groups anchored here are "your side."
    pub const THIS: &'static str = "this";

    pub fn this() -> Self {
        NodeId(Self::THIS.to_string())
    }

    pub fn is_this(&self) -> bool {
        self.0 == Self::THIS
    }
}

// ---- media + flow -----------------------------------------------------

/// What kind of signal flows over a route. The graph only connects
/// endpoints of compatible media (with `Generic` as a wildcard escape
/// hatch for app-defined payloads).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    /// Microphones, speakers, system audio.
    Audio,
    /// Camera frames and screen-share streams.
    Video,
    /// A whole desktop as a remote display (the RDC case) — distinct from
    /// `Video` so "use my monitor as a second screen for that PC" doesn't
    /// get cross-wired with "show me your webcam."
    Display,
    /// Keyboard / mouse / controller events.
    Input,
    /// A shared folder or volume.
    Storage,
    /// App-defined payload — matches anything.
    Generic,
}

impl MediaKind {
    /// Two media are route-compatible when they're equal, or when either
    /// side is the `Generic` wildcard.
    pub fn compatible(self, other: MediaKind) -> bool {
        self == other || self == MediaKind::Generic || other == MediaKind::Generic
    }

    pub fn label(self) -> &'static str {
        match self {
            MediaKind::Audio => "audio",
            MediaKind::Video => "video",
            MediaKind::Display => "display",
            MediaKind::Input => "input",
            MediaKind::Storage => "storage",
            MediaKind::Generic => "data",
        }
    }
}

/// Which way a capability can move its media.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Flow {
    /// Produces the signal — a mic, a camera, a screen.
    Source,
    /// Consumes it — a speaker, a monitor, a "control this PC" endpoint.
    Sink,
    /// Both — a headset, a duplex audio device, a shared folder.
    Duplex,
}

impl Flow {
    pub fn can_source(self) -> bool {
        matches!(self, Flow::Source | Flow::Duplex)
    }
    pub fn can_sink(self) -> bool {
        matches!(self, Flow::Sink | Flow::Duplex)
    }
}

// ---- capability -------------------------------------------------------

/// One routable thing on one node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    pub id: CapabilityId,
    pub node: NodeId,
    /// Friendly name — "Built-in display", "ReSpeaker 4 Mic Array".
    pub label: String,
    pub media: MediaKind,
    pub flow: Flow,
    /// Origin hint for the UI's icon + grouping — "microphone", "display",
    /// "camera", "screen", "control". Free-form; not load-bearing.
    #[serde(default)]
    pub origin: String,
    /// `true` when this is the node's **current default** for its device
    /// category — the mic the machine captures from, the display it drives
    /// first, and so on. The UI badges it and routing prefers it when
    /// auto-picking an endpoint, so "connect audio to that machine" lands
    /// on the device it actually uses. `#[serde(default)]` so presence from
    /// an older peer (no field) still decodes.
    #[serde(default)]
    pub default: bool,
}

impl Capability {
    pub fn new(
        node: impl Into<NodeId>,
        id: impl Into<CapabilityId>,
        label: impl Into<String>,
        media: MediaKind,
        flow: Flow,
        origin: impl Into<String>,
    ) -> Self {
        Capability {
            id: id.into(),
            node: node.into(),
            label: label.into(),
            media,
            flow,
            origin: origin.into(),
            default: false,
        }
    }

    /// Builder: flag this capability as its category's current default.
    pub fn as_default(mut self, default: bool) -> Self {
        self.default = default;
        self
    }
}

// ---- people, relationships, grants -----------------------------------

/// A human on the other end of a *shared* relationship.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    pub id: PersonId,
    pub name: String,
}

/// What a shared person's device is allowed to do, from your point of
/// view. `Provide` lets their device be a *source* in a route with you
/// (they send you their camera); `Consume` lets it be a *sink* (you cast
/// your screen to them); `Both` is a duplex grant (a shared headset).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantRole {
    Provide,
    Consume,
    Both,
}

impl GrantRole {
    pub fn allows_source(self) -> bool {
        matches!(self, GrantRole::Provide | GrantRole::Both)
    }
    pub fn allows_sink(self) -> bool {
        matches!(self, GrantRole::Consume | GrantRole::Both)
    }
}

/// A single scoped authorization on a share — "Alex may receive my
/// audio." Narrow by capability to pin it to one device ("…my *living
/// room speaker*, nothing else").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub id: String,
    pub media: MediaKind,
    pub role: GrantRole,
    /// `None` = any capability of this media on the relevant node. `Some`
    /// pins the grant to exactly one capability.
    #[serde(default)]
    pub capability: Option<CapabilityId>,
    /// Human-readable summary for the share sheet ("Receive your screen").
    #[serde(default)]
    pub label: String,
}

impl Grant {
    /// Does this grant authorize a shared endpoint to act in `role` for
    /// `media` on `capability`?
    pub fn permits(&self, media: MediaKind, role: GrantRole, capability: &CapabilityId) -> bool {
        if !self.media.compatible(media) {
            return false;
        }
        if let Some(c) = &self.capability {
            if c != capability {
                return false;
            }
        }
        match role {
            GrantRole::Provide => self.role.allows_source(),
            GrantRole::Consume => self.role.allows_sink(),
            GrantRole::Both => self.role == GrantRole::Both,
        }
    }
}

/// The share envelope for a node owned by someone else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Share {
    pub person: Person,
    #[serde(default)]
    pub grants: Vec<Grant>,
}

/// How a node relates to you.
///
/// This is the crux of AllMyStuff's model: the **mesh** below us proves
/// *who* a peer is (MyOwnMesh's ed25519 handshake). AllMyStuff never asks
/// the user about keys — it asks the only question a person actually
/// thinks about: **is this mine, or am I sharing with someone?**
///
///  * [`Relationship::Mine`] — a device you own or manage. Part of your
///    personal fleet; everything you own can talk to everything else you
///    own without further ceremony.
///  * [`Relationship::Shared`] — someone else, connected for *specific
///    purposes*. Nothing flows until you grant it, and each grant is
///    scoped to a direction, a media, and (optionally) one device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Relationship {
    Mine,
    Shared(Share),
}

impl Relationship {
    pub fn is_mine(&self) -> bool {
        matches!(self, Relationship::Mine)
    }
    pub fn share(&self) -> Option<&Share> {
        match self {
            Relationship::Shared(s) => Some(s),
            Relationship::Mine => None,
        }
    }
}

// ---- node -------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// The local machine running this app.
    This,
    /// Any other machine on the mesh.
    Machine,
}

/// A node on the graph — a machine, with the relationship that governs
/// what it may do with your stuff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeshNode {
    pub id: NodeId,
    pub label: String,
    pub kind: NodeKind,
    pub relationship: Relationship,
    #[serde(default)]
    pub online: bool,
}

impl MeshNode {
    /// Convenience constructor for the local machine.
    pub fn this(label: impl Into<String>) -> Self {
        MeshNode {
            id: NodeId::this(),
            label: label.into(),
            kind: NodeKind::This,
            relationship: Relationship::Mine,
            online: true,
        }
    }
}

// ---- route ------------------------------------------------------------

/// A live connection between two capabilities: `from` (the source side)
/// feeds `to` (the sink side). Built and validated through
/// [`crate::Catalog::propose_route`] — never construct one by hand and
/// trust it, since the catalog is what enforces media/flow/authorization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Route {
    pub id: String,
    pub from: CapabilityId,
    pub to: CapabilityId,
    pub media: MediaKind,
}
