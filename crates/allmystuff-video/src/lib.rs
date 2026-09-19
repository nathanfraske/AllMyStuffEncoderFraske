//! Reusable video framing, classification and receive policy.
//!
//! The default feature set has no capture device, native codec, runtime or
//! transport dependency. Applications own routing and IPC envelopes.

pub mod codec;
pub mod framing;
pub mod handoff;
pub mod receive;

pub use allmystuff_frame_timing as timing;
pub use allmystuff_video_metadata as metadata;
pub use allmystuff_video_pacing as pacing;

#[cfg(test)]
#[path = "../tests/support/legacy_output.rs"]
mod test_support;
