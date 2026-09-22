//! Compatibility paths for the existing audio-disabled behavior.

pub(crate) use allmystuff_audio::codec::OpusDecoder;
pub use allmystuff_audio::disabled::AudioBridge;
pub(crate) use allmystuff_audio::disabled::OpusStream;
pub use allmystuff_audio::CaptureSource;
pub(crate) use allmystuff_audio::{OPUS_FRAME_SAMPLES, OPUS_FRAME_US, OPUS_RATE};
