//! Compatibility paths for the existing audio-disabled behavior.

pub use allmystuff_audio::disabled::AudioBridge;
pub use allmystuff_audio::CaptureSource;
pub(crate) use allmystuff_audio::codec::OpusDecoder;
pub(crate) use allmystuff_audio::disabled::OpusStream;
pub(crate) use allmystuff_audio::{OPUS_FRAME_SAMPLES, OPUS_FRAME_US, OPUS_RATE};
