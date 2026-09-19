//! Compatibility path for encoded-video metadata shared by host and viewer builds.

pub(crate) use allmystuff_video::metadata::{
    insert_au_identity_marker, peek_au_identity_marker, take_au_identity_marker, AuIdentity,
    AuRecovery,
};
