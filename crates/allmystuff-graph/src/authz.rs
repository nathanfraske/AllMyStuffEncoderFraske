//! Authorization — the layer that turns "is this mine or am I sharing?"
//! into a yes/no on every individual connection.
//!
//! The rule is short: a route is allowed if **every endpoint that sits on
//! a node you don't own is covered by a grant**. Your own devices need no
//! grant to talk to each other; a shared person's device needs a grant for
//! the exact role it's about to play (source vs sink), media, and —
//! optionally — the exact capability.

use serde::{Deserialize, Serialize};

use crate::model::{CapabilityId, GrantRole, MediaKind, NodeId, PersonId};

/// A connection was refused because a shared person lacks the grant it
/// would need. Carries enough to render a friendly message *and* to offer
/// the one-tap fix (see [`GrantRequest`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{person_name} isn't allowed to {action} yet")]
pub struct Denied {
    pub node: NodeId,
    pub person: PersonId,
    pub person_name: String,
    pub media: MediaKind,
    pub role: GrantRole,
    pub capability: CapabilityId,
    /// Human phrasing of what was attempted — "send you their camera".
    pub action: String,
}

/// The grant that *would* authorize a denied route — handed back so the
/// UI can show a "Let {person} {action}?" button that adds exactly this
/// and nothing more. Least-privilege by construction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantRequest {
    pub node: NodeId,
    pub person: PersonId,
    pub person_name: String,
    pub media: MediaKind,
    pub role: GrantRole,
    pub capability: Option<CapabilityId>,
    /// Suggested grant label — "Receive your screen".
    pub description: String,
}

/// Phrase what a shared endpoint is about to do, from the owner's point of
/// view, for messages and grant labels.
///
///  * `Provide` — their device is the *source*: they send you something.
///  * `Consume` — their device is the *sink*: they receive something of
///    yours.
pub fn describe_action(media: MediaKind, role: GrantRole) -> String {
    match role {
        GrantRole::Provide => format!("send you their {}", media.label()),
        GrantRole::Consume => format!("receive your {}", media.label()),
        GrantRole::Both => format!("exchange {} with you", media.label()),
    }
}

/// Short label for a grant chip / share-sheet row.
pub fn describe_grant(media: MediaKind, role: GrantRole) -> String {
    let m = media.label();
    match role {
        GrantRole::Provide => format!("Send their {m}"),
        GrantRole::Consume => format!("Receive your {m}"),
        GrantRole::Both => format!("Exchange {m}"),
    }
}
