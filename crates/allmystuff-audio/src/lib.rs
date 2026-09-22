//! Shared PCM processing, optional Opus state, and optional audio devices.
//!
//! The default build contains only deterministic PCM helpers. `codec` adds
//! the fixed mono Opus stream and the existing disabled I/O surface. `audio-io`
//! adds CPAL capture/playback and the platform loopback implementations.
//! Callers retain routing, permission checks, transport and runtime ownership.

pub mod pcm;

#[cfg(feature = "codec")]
pub mod codec;
#[cfg(feature = "codec")]
pub mod disabled;
#[cfg(feature = "codec")]
pub use allmystuff_session::AudioFrame;

#[cfg(feature = "audio-io")]
pub mod io;

/// What a sourcing audio route records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureSource {
    /// The default input device — a scanned microphone capability.
    Mic,
    /// This machine's own playback (the synthetic `system-audio`
    /// capability): the loopback of the default output.
    System,
}

/// The audio lane's clock: Opus always runs a 48 kHz RTP clock.
pub const OPUS_RATE: u32 = 48_000;
/// 20 ms at 48 kHz — the canonical Opus frame.
pub const OPUS_FRAME_SAMPLES: usize = 960;
/// The same 20 ms, as the lane's RTP pacing value.
pub const OPUS_FRAME_US: u64 = 20_000;

/// Decide whether a periodic audio statistics line is logged at info.
///
/// Called at the existing statistics emission point, never during bridge
/// construction. The node supplies its shared lazy video statistics policy.
pub trait StatsPolicy {
    fn stats_to_info() -> bool;
}

/// Keep periodic statistics at debug unless the caller supplies a policy.
pub struct DebugStats;

impl StatsPolicy for DebugStats {
    fn stats_to_info() -> bool {
        false
    }
}
