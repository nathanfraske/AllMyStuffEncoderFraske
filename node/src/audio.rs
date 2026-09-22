//! Compatibility paths for the shared audio implementation.

pub use allmystuff_audio::CaptureSource;
pub(crate) use allmystuff_audio::codec::{OpusDecoder, OpusStream};
pub(crate) use allmystuff_audio::{OPUS_FRAME_SAMPLES, OPUS_FRAME_US, OPUS_RATE};

pub type AudioBridge = allmystuff_audio::io::AudioBridge<NodeStats>;

/// Retain the node's existing shared, lazily evaluated video statistics dial.
pub struct NodeStats;

impl allmystuff_audio::StatsPolicy for NodeStats {
    fn stats_to_info() -> bool {
        crate::video::stats_to_info()
    }
}
