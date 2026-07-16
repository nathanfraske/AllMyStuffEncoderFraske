//! # allmystuff-mobile-core
//!
//! The **model** of the AllMyStuff mobile app (iOS / Android): what a phone
//! *is* on the graph, and the client half of the wire it would speak. It is
//! deliberately pure Rust — `serde`, `thiserror`, and the three pure
//! AllMyStuff library crates ([`allmystuff_graph`], [`allmystuff_protocol`],
//! [`allmystuff_session`]) — so everything here is verified by `cargo test`,
//! exactly the way the desktop's wire contract is. No webview, no daemon, no
//! native decoders, no network: those live one layer up.
//!
//! ## Why the phone is a *node*, not a thin client
//!
//! On the desktop, the GUI is a thin client of a separate `allmystuff-serve`
//! process it **spawns**, which in turn spawns the `myownmesh` daemon. iOS
//! forbids a sandboxed app from spawning child processes, so that model can't
//! cross over: the phone becomes a first-class mesh peer itself — its own
//! ed25519 identity, direct WebRTC DTLS/SRTP to its peers, signaling only.
//! The "no central server, peer-to-peer, end-to-end encrypted" promise is
//! preserved — see `docs/MOBILE.md` for the full architecture.
//!
//! ## What the app uses, and what stands as the spec
//!
//! The shipped shell (`gui/mobile`) embeds the **whole node engine**
//! (`allmystuff-node`, capture-less) plus the `myownmesh` daemon in-process,
//! so at runtime the wire is spoken by the same engine code as everywhere
//! else. Two of this crate's modules ride in the app itself:
//!
//! * [`caps`] + [`node`] — the phone's capability set and [`NodeProfile`]:
//!   what `scan_self` answers from while the engine boots, and the pinned
//!   truth about a phone's synthetic endpoints (see below).
//!
//! The rest — [`connect`], [`control`], [`transport`], [`media`] — is the
//! tested, executable **spec of the viewer/controller client**: every path
//! written against one seam, [`MeshClient`], which an in-memory fake
//! implements in tests so the logic runs without a radio. It is the starting
//! point if a pure-native (non-Tauri) client is ever built, and the
//! reference for what a conforming viewer/controller must send.
//!
//! ## What a phone is, on the graph
//!
//! A phone is a **viewer / controller**, not a host (see [`MobileScope`]):
//!
//! * it can **receive** a remote machine's screen/camera ([`MediaKind::Video`]
//!   sink) and play its audio, so [`media::VideoSink`] reassembles MJPEG and
//!   the [`media::VideoDecoder`] seam feeds the platform H.264 decoder;
//! * it can **drive** a remote ([`MediaKind::Input`] source) — touch becomes
//!   normalized [`InputAction`]s via [`media::InputEncoder`];
//! * it can open a remote **shell** ([`media::TermPlane`]) and browse remote
//!   **files** ([`media::FileClient`]);
//! * it does **not** host its own screen, a PTY, or input injection. Those are
//!   desktop-only capture concerns with no clean (or, for input, any) mobile
//!   story. Phone-as-a-*source* (camera / mic / screen-share) is a later,
//!   opt-in [`MobileScope::ViewerControllerHost`] decision.
//!
//! ## Layout
//!
//! * [`caps`] — the phone's [`Capability`] set and advertised feature flags.
//! * [`node`] — assemble the phone's [`NodeProfile`] for presence.
//! * [`connect`] — build the [`RouteControl::Offer`] for a screen / camera /
//!   audio / input / terminal / files route, validated through the
//!   receiver-side [`Catalog`] (input is the outbound half of remote control:
//!   the phone's `keyboard-mouse` source → the remote's `control` sink).
//! * [`control`] — the rest of the control surface a viewer/controller phone
//!   drives: fleet-machine admin (upgrade / restart / reboot), KVM curation and
//!   recognition, per-route video negotiation ([`control::tune`],
//!   [`control::refresh_video`], [`control::video_feedback`]), the shared-shell
//!   picker, and fleet-site management.
//! * [`transport`] — the [`MeshClient`] seam and [`transport::classify`], which
//!   turns a raw `(channel, payload)` off the mesh into a typed [`Inbound`].
//! * [`media`] — the per-plane client pipelines (video, input, terminal,
//!   files), all of them pure functions over the [`allmystuff_session`] frame
//!   types.

pub mod caps;
pub mod connect;
pub mod control;
pub mod media;
pub mod node;
pub mod transport;

pub use caps::{mobile_capabilities, mobile_features, MobileScope};
pub use connect::{
    offer_audio, offer_camera, offer_files, offer_input, offer_screen, offer_terminal, teardown,
    ConnectError as OfferError,
};
pub use control::{
    app_restart, app_restart_device, app_upgrade, is_kvm, kvm_attach, kvm_detach, kvm_mesh_add,
    kvm_mesh_remove, kvm_web_site, list_terminal_sessions, profile_request, refresh_video,
    site_list, site_set_exposed, tune, video_feedback,
};
pub use node::{mobile_profile, MobileNodeConfig};
pub use transport::{answer_profile_request, classify, Inbound, MeshClient, MeshError, MeshResult};

/// A convenient single import for the types a mobile front end touches most.
///
/// ```
/// use allmystuff_mobile_core::prelude::*;
///
/// let me = NodeId::from("phone-abc");
/// let caps = mobile_capabilities(&me, MobileScope::ViewerController);
/// assert!(caps.iter().any(|c| c.origin == "viewer"));
/// ```
pub mod prelude {
    pub use crate::caps::{mobile_capabilities, mobile_features, MobileScope};
    pub use crate::connect::{
        offer_audio, offer_camera, offer_files, offer_input, offer_screen, offer_terminal, teardown,
    };
    pub use crate::control::{
        app_restart, app_restart_device, app_upgrade, is_kvm, kvm_attach, kvm_detach, kvm_mesh_add,
        kvm_mesh_remove, kvm_web_site, list_terminal_sessions, profile_request, refresh_video,
        site_list, site_set_exposed, tune, video_feedback,
    };
    pub use crate::media::{
        FileClient, FileReply, InputEncoder, TermPlane, VideoDecoder, VideoSink, VideoUpdate,
    };
    pub use crate::node::{mobile_profile, MobileNodeConfig};
    pub use crate::transport::{answer_profile_request, classify, Inbound, MeshClient};

    pub use allmystuff_graph::{
        Capability, CapabilityId, Catalog, Flow, MediaKind, MeshNode, NodeId, NodeKind,
        Relationship, Route,
    };
    pub use allmystuff_protocol::{
        ControlMessage, NodeProfile, RouteControl, CHANNEL_CONTROL, CHANNEL_MEDIA, CHANNEL_PRESENCE,
    };
    pub use allmystuff_session::{
        FileEvent, InputAction, MediaPayload, TermEvent, VideoFrame, VideoStatusFrame,
    };
}
