//! # allmystuff-protocol
//!
//! Everything AllMyStuff puts on a wire, in one dependency-light crate:
//!
//!  * [`control`] — a faithful mirror of the MyOwnMesh daemon's
//!    control-socket protocol. AllMyStuff is a *client* of `myownmesh
//!    serve` (the sidecar pattern the whole product family uses); this is
//!    how it asks the daemon for status, joins networks, approves peers,
//!    and pumps the event stream — without compiling the engine.
//!
//!  * [`app`] — AllMyStuff's own peer-to-peer messages (presence, route
//!    setup, share negotiation) that ride *inside* the daemon's typed
//!    channels once two nodes are connected.
//!
//! Keeping both here means the Tauri backend, and any future headless
//! agent, share one source of truth for the bytes — and it all builds and
//! tests with nothing but `serde`.

pub mod app;
pub mod control;

// Re-export the most-used items at the crate root.
pub use app::{
    ControlMessage, InventorySummary, NodeProfile, OwnedMember, OwnedRoster, OwnershipControl,
    RoomAccess, RoomEvent, RoomMessage, RouteControl, ShareControl, APP_ID, CHANNEL_CONTROL,
    CHANNEL_MEDIA, CHANNEL_OWNED, CHANNEL_PRESENCE, CHANNEL_ROOMS, FEATURE_CAMERA, FEATURE_FILES,
    FEATURE_MEDIA_LANES, FEATURE_ROOMS, FEATURE_TERMINAL, PROTOCOL_VERSION,
};
pub use control::{ClientId, Request, Response, ServerOut};
