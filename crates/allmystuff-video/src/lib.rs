//! Reusable video framing, classification and receive policy.
//!
//! The default feature set has no capture device, native codec, runtime or
//! transport dependency. Applications own routing and IPC envelopes.

pub mod codec;
pub mod framing;
pub mod handoff;
pub mod ingress;
pub mod receive;

pub use allmystuff_frame_timing as timing;
pub use allmystuff_video_metadata as metadata;
pub use allmystuff_video_pacing as pacing;

#[cfg(test)]
#[path = "../tests/support/legacy_output.rs"]
mod test_support;

pub mod host;
#[cfg(feature = "decode")]
pub mod output;
#[cfg(feature = "decode")]
pub mod video_decode;
#[cfg(feature = "decode")]
pub use video_decode as decode;
#[cfg(all(windows, feature = "host"))]
pub mod amf;
#[cfg(feature = "host")]
pub mod camera_capture;
#[cfg(all(windows, feature = "host"))]
pub mod d3d11va;
#[cfg(all(windows, feature = "host"))]
pub mod gpu_pipeline;
#[cfg(feature = "hwenc")]
pub mod hwenc;
#[cfg(all(windows, feature = "host"))]
pub mod mediafoundation;
#[cfg(all(windows, feature = "host"))]
pub mod nvdec;
#[cfg(all(windows, feature = "host"))]
pub mod nvenc;
#[cfg(feature = "decode")]
pub mod os_perf;
#[cfg(feature = "host")]
pub mod video;
#[cfg(all(feature = "decode", not(feature = "host")))]
#[path = "stubs/video.rs"]
pub mod video;
#[cfg(feature = "host")]
mod video_frame_timing;
#[cfg(all(test, feature = "host"))]
mod video_wire;
#[cfg(all(target_os = "macos", feature = "host"))]
pub mod videotoolbox;
#[cfg(feature = "host")]
pub mod wake;
#[cfg(all(target_os = "linux", feature = "host"))]
pub mod wayland_capture;
#[cfg(all(windows, feature = "host"))]
pub mod win_capture;
