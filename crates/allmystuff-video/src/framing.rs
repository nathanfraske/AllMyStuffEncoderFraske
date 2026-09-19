//! Application access-unit closing markers and the two existing slice walks.
//!
//! Host and capture-less walks intentionally retain their different treatment
//! of malformed overlapping prefixes. Select the caller's existing policy.

pub mod host;
pub mod stub;

pub use host::split_annexb_paced as split_annexb_paced_host;
pub use host::{paced_au_marker, paced_au_marker_count};
pub use stub::split_annexb_paced as split_annexb_paced_stub;

/// Existing target size for one paced slice chunk.
pub const PACE_SLICE_BYTES: usize = 24 * 1024;
