//! The audio frame carried over an active audio route.
//!
//! A route's media plane is a stream of [`AudioFrame`]s sent on the mesh's
//! typed channel. PCM samples are packed little-endian and base64'd so the
//! frame fits the daemon's JSON channel payload without ballooning into a
//! multi-thousand-element number array.

use base64::Engine as _;
use serde::{Deserialize, Serialize};

/// One buffer of interleaved 16-bit PCM, tagged with its route + a
/// monotonic sequence number so the sink can detect drops/reorders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioFrame {
    /// The route this frame belongs to (so a node carrying several audio
    /// routes can demux them off one channel).
    pub route: String,
    pub seq: u64,
    pub sample_rate: u32,
    pub channels: u16,
    /// Sender-side media clock in microseconds.
    ///
    /// This is optional so nodes using the original audio-frame schema keep
    /// interoperating: old senders omit it and old recorded JSON still
    /// deserializes.  Receivers use it only to distinguish network jitter
    /// from the sender's capture cadence; it is not a wall-clock timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_timestamp_us: Option<u64>,
    /// Interleaved i16 samples, base64 of little-endian bytes on the wire.
    #[serde(with = "pcm_b64")]
    pub pcm: Vec<i16>,
}

impl AudioFrame {
    pub fn new(
        route: impl Into<String>,
        seq: u64,
        sample_rate: u32,
        channels: u16,
        pcm: Vec<i16>,
    ) -> Self {
        AudioFrame {
            route: route.into(),
            seq,
            sample_rate,
            channels,
            media_timestamp_us: None,
            pcm,
        }
    }

    /// Attach the sender's monotonic media time without changing the legacy
    /// [`AudioFrame::new`] call shape.
    pub fn with_media_timestamp(mut self, media_timestamp_us: u64) -> Self {
        self.media_timestamp_us = Some(media_timestamp_us);
        self
    }

    /// Construct a frame carrying a sender media timestamp.
    pub fn new_timestamped(
        route: impl Into<String>,
        seq: u64,
        sample_rate: u32,
        channels: u16,
        media_timestamp_us: u64,
        pcm: Vec<i16>,
    ) -> Self {
        Self::new(route, seq, sample_rate, channels, pcm).with_media_timestamp(media_timestamp_us)
    }

    /// Frames per second worth of audio in this buffer (per channel).
    pub fn frame_count(&self) -> usize {
        self.pcm.len() / self.channels.max(1) as usize
    }
}

mod pcm_b64 {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(pcm: &[i16], s: S) -> Result<S::Ok, S::Error> {
        let mut bytes = Vec::with_capacity(pcm.len() * 2);
        for sample in pcm {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<i16>, D::Error> {
        let text = String::deserialize(d)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(text.as_bytes())
            .map_err(serde::de::Error::custom)?;
        Ok(bytes
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trips_through_json() {
        let f = AudioFrame::new(
            "route:a→b",
            7,
            48_000,
            2,
            vec![0, 1, -1, 32767, -32768, 100],
        );
        let json = serde_json::to_string(&f).unwrap();
        // PCM must travel as a base64 string, not a number array.
        assert!(json.contains("\"pcm\":\""));
        let back: AudioFrame = serde_json::from_str(&json).unwrap();
        assert_eq!(f, back);
        assert_eq!(back.frame_count(), 3); // 6 samples / 2 channels
    }

    #[test]
    fn legacy_json_without_media_timestamp_stays_compatible() {
        let json = r#"{"route":"r","seq":3,"sample_rate":48000,"channels":2,"pcm":"AAABAA=="}"#;
        let frame: AudioFrame = serde_json::from_str(json).unwrap();
        assert_eq!(frame.media_timestamp_us, None);

        let encoded = serde_json::to_string(&frame).unwrap();
        assert!(!encoded.contains("media_timestamp_us"));
    }

    #[test]
    fn media_timestamp_round_trips_when_present() {
        let frame = AudioFrame::new_timestamped("r", 4, 48_000, 2, 123_456, vec![1, -1]);
        let json = serde_json::to_string(&frame).unwrap();
        assert!(json.contains("\"media_timestamp_us\":123456"));
        assert_eq!(
            serde_json::from_str::<AudioFrame>(&json)
                .unwrap()
                .media_timestamp_us,
            Some(123_456)
        );
    }
}
