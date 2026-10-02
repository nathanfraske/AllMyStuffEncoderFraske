//! Local MyOwnMesh control transport with explicit endpoints and wire contracts.
//!
//! This crate does not discover a default daemon, launch processes, choose an
//! application upgrade or authorize access to application state. Status is
//! protocol evidence, not authentication or proof of a binary's Git revision.
//! An event capability binds daemon resources; it is not application enrollment
//! or permission to access a user's ledger. Callers retain their product policy.
//!
//! Keep the returned [`EventSession`] alive while using its registrations.
//! Registrations cannot be constructed from a client id or capability string,
//! and become unusable on renewal, socket end, receiver closure or session Drop.

mod client;
pub mod contract;
mod session;
pub mod transport;

pub use allmystuff_protocol::{ClientId, Request, Response};
pub use client::{ControlClient, StatusGuard, StatusProbe};
pub use session::{ConnectionGeneration, EventContract, EventRegistration, EventSession};
pub use transport::{Endpoint, CANDIDATE_EVENT_LINE_LIMIT, CONTROL_TIMEOUT, STATUS_ACK_LIMIT};
