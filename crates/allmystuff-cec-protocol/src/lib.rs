//! # allmystuff-cec-protocol
//!
//! The wire contract and shared constants for **CEC Support** — Critical
//! Error Computing's one-tap remote help desk. It lives in the AllMyStuff
//! workspace because both sides speak it: the technician side (an AllMyStuff
//! install joined to a customer's support mesh) and the standalone CEC Support
//! client app, which reuses this crate through the shared node engine.
//!
//! CEC Support rides on the [MyOwnMesh](https://github.com/mrjeeves/MyOwnMesh)
//! peer-to-peer substrate, but with two deliberate twists that make it behave
//! like AnyDesk rather than an ordinary always-on mesh:
//!
//! 1. **A per-customer "Silent" mesh, named after the number.** Running the
//!    CEC Support app gives the customer a short [`SupportId`](ids::SupportId)
//!    number and joins a MyOwnMesh network of type `Silent` whose `network_id`
//!    is *derived from that number* ([`network_id_for_number`]). A `Silent`
//!    mesh auto-dials nobody and never gossips a roster — the machine is merely
//!    *discoverable*, and only inside its own number-derived room. A technician
//!    can't even signal a customer without being told the number, so the number
//!    is the out-of-band discovery credential; the room is, in effect, secret
//!    to that customer.
//! 2. **Deliberate dial, then approve.** The technician is told the number,
//!    derives the same `network_id`, joins that Silent mesh, finds the one
//!    customer there, and *explicitly* dials them. The customer then approves
//!    with one of three choices — [`ApprovalScope`] `Once` / `ThreeHours` /
//!    `Forever` — and can revoke at any time. Approval state lives in
//!    `allmystuff-cec-consent`; this crate defines only the *messages* and the
//!    *scope*.
//!
//! ## Isolation from other MyOwnMesh ecosystems
//!
//! CEC Support forks MyOwnMesh's signing tags and home dir so its signatures
//! never cross-verify against an AllMyStuff / MyOwnMesh / MyOwnLLM mesh and its
//! identity + state never collide with an existing install. It deliberately does
//! *not* fork the signaling app-id: each support session is already isolated by
//! its per-number `network_id` (`cec-<number>`), which seeds a distinct room
//! handle, so technician and customer meet on the default app-id with no env
//! override.
//!
//! - [`CEC_SIGN_DOMAIN_TAG`] / [`CEC_SIGN_DOMAIN_TAG_STATE`] — domain-separated
//!   signing tags.
//! - [`CEC_HOME_ENV`] — a CEC-specific home dir (`MYOWNMESH_HOME` override) so
//!   identity + state never collide with an existing AllMyStuff install.

pub mod ids;
pub mod media;
pub mod wire;

pub use ids::{
    format_support_id, network_id_for_device, network_id_for_number, support_id_from_device,
    SupportId, SUPPORT_ID_LEN,
};
pub use media::{
    decode_media_frame, encode_media_frame, MediaFrame, MEDIA_KIND_AUDIO, MEDIA_KIND_VIDEO,
};
pub use wire::{AppControl, ApprovalScope, ConnectControl, ControlMessage, Role, SupportPresence};

/// Prefix for a customer's per-number `network_id`. The full id is
/// [`network_id_for_number`]; e.g. number `123456789` → `"cec-123456789"`. Every
/// CEC mesh id starts with this so they're easy to recognise and never collide
/// with a customer's own AllMyStuff fleet networks.
pub const CEC_NETWORK_PREFIX: &str = "cec-";

/// Domain-separation tag for the per-peer ed25519 auth handshake. Forked from
/// `myownmesh-mesh-auth-v1:` so a signature obtained on a CEC mesh cannot be
/// replayed on any other MyOwnMesh network, and vice-versa.
pub const CEC_SIGN_DOMAIN_TAG: &str = "cec-support-mesh-auth-v1:";

/// Domain-separation tag for signed network-state transitions.
pub const CEC_SIGN_DOMAIN_TAG_STATE: &str = "cec-support-network-state-v1:";

/// Env var naming the CEC Support home dir (a `MYOWNMESH_HOME` override), so
/// identity + rosters live under their own tree and never collide with an
/// existing AllMyStuff / MyOwnMesh install on the same machine.
pub const CEC_HOME_ENV: &str = "CEC_SUPPORT_HOME";

/// Wire-protocol version. Stays at 1 across additive changes; every message
/// enum carries an `Unknown` catch-all so an older peer never fails a decode.
pub const PROTOCOL_VERSION: u32 = 1;

/// Typed-channel name carrying [`SupportPresence`] beacons.
pub const CHANNEL_PRESENCE: &str = "cec.presence";
/// Typed-channel name carrying point-to-point [`ControlMessage`]s
/// (connect-request / approve / deny / end, and app control).
pub const CHANNEL_CONTROL: &str = "cec.control";
/// Channel name carrying the binary media plane (screen frames / input).
pub const CHANNEL_MEDIA: &str = "cec.media";

/// Capability tag advertised by every CEC Support node so peers can tell CEC
/// traffic from anything else sharing the substrate.
pub const CAP_TAG: &str = "cec-support";
/// Capability tag identifying a customer (help-seeker) node.
pub const ROLE_CLIENT_TAG: &str = "cec-client";
/// Capability tag identifying a technician (help-desk) node.
pub const ROLE_TECH_TAG: &str = "cec-tech";

/// Seconds in the "Auto-Approve for 3 hours" window.
pub const THREE_HOURS_SECS: u64 = 3 * 60 * 60;

/// This build's version string (`CARGO_PKG_VERSION`).
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
