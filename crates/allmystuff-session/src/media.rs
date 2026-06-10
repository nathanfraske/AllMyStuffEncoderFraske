//! The video + input frames carried over active display/input routes —
//! the rest of the media plane next to [`crate::AudioFrame`].
//!
//! The transport is deliberately the "basic known stuff": a display route
//! is an **MJPEG stream** (piKVM's default — each frame a standalone JPEG,
//! so loss costs one frame and seeking/decoding state never exists), and an
//! input route is a stream of small **normalized HID events** (mouse moves
//! in 0..1 so the two ends never negotiate resolutions; keys as the DOM
//! `KeyboardEvent.key` value so layouts resolve on the side that typed).
//!
//! All three frame kinds share the daemon's one media channel. Audio keeps
//! its original untagged shape (a v0.1.0 peer still decodes it); video and
//! input are tagged `"t":"video"` / `"t":"input"`, and [`MediaPayload`]
//! demuxes by that tag — absent tag means audio.

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::AudioFrame;

/// One JPEG-encoded frame of a display route's stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoFrame {
    /// Tag for demuxing off the shared media channel. Always `"video"`.
    pub t: MediaTagVideo,
    /// The route this frame belongs to.
    pub route: String,
    pub seq: u64,
    /// Encoded frame dimensions (after any sender-side downscale).
    pub width: u32,
    pub height: u32,
    /// The capture source's full resolution — what normalized input
    /// coordinates map back onto.
    pub source_width: u32,
    pub source_height: u32,
    /// The JPEG bytes, base64 on the wire (the daemon channel is JSON).
    #[serde(with = "bytes_b64")]
    pub jpeg: Vec<u8>,
}

/// One keyboard / mouse event of an input route's stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputEvent {
    /// Tag for demuxing off the shared media channel. Always `"input"`.
    pub t: MediaTagInput,
    pub route: String,
    pub seq: u64,
    #[serde(flatten)]
    pub action: InputAction,
}

/// What the far keyboard/mouse did. Coordinates are normalized 0..1 over
/// the *source screen* of the paired display route, so neither end needs
/// the other's resolution; the injecting side multiplies by its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputAction {
    MouseMove {
        x: f64,
        y: f64,
    },
    /// `button`: 0 left, 1 middle, 2 right (the DOM convention).
    MouseButton {
        button: u8,
        down: bool,
    },
    /// Scroll in wheel lines (positive = down / right).
    Wheel {
        dx: f64,
        dy: f64,
    },
    /// `key` is the DOM `KeyboardEvent.key` value — a printable character
    /// ("a", "?") or a named key ("Enter", "ArrowLeft", "Shift").
    Key {
        key: String,
        down: bool,
    },
}

/// Everything that can arrive on the media channel, demuxed by the `t`
/// tag (no tag = audio, the original frame shape).
#[derive(Debug, Clone, PartialEq)]
pub enum MediaPayload {
    Audio(AudioFrame),
    Video(VideoFrame),
    Input(InputEvent),
}

impl MediaPayload {
    /// Decode a media-channel payload. `None` for frames we don't
    /// understand (e.g. a newer peer's new kind) — drop, never error.
    pub fn decode(payload: serde_json::Value) -> Option<MediaPayload> {
        match payload.get("t").and_then(|t| t.as_str()) {
            Some("video") => serde_json::from_value(payload)
                .ok()
                .map(MediaPayload::Video),
            Some("input") => serde_json::from_value(payload)
                .ok()
                .map(MediaPayload::Input),
            Some(_) => None,
            None => serde_json::from_value(payload)
                .ok()
                .map(MediaPayload::Audio),
        }
    }

    /// The route id the frame belongs to, whatever its kind.
    pub fn route(&self) -> &str {
        match self {
            MediaPayload::Audio(f) => &f.route,
            MediaPayload::Video(f) => &f.route,
            MediaPayload::Input(f) => &f.route,
        }
    }
}

// Unit-variant tags so the structs serialize with a literal `"t":"…"`
// field (and refuse to deserialize anything else), without a hand-written
// serde impl.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MediaTagVideo {
    #[default]
    #[serde(rename = "video")]
    Video,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MediaTagInput {
    #[default]
    #[serde(rename = "input")]
    Input,
}

impl VideoFrame {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        route: impl Into<String>,
        seq: u64,
        width: u32,
        height: u32,
        source_width: u32,
        source_height: u32,
        jpeg: Vec<u8>,
    ) -> Self {
        VideoFrame {
            t: MediaTagVideo::Video,
            route: route.into(),
            seq,
            width,
            height,
            source_width,
            source_height,
            jpeg,
        }
    }
}

impl InputEvent {
    pub fn new(route: impl Into<String>, seq: u64, action: InputAction) -> Self {
        InputEvent {
            t: MediaTagInput::Input,
            route: route.into(),
            seq,
            action,
        }
    }
}

mod bytes_b64 {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        base64::engine::general_purpose::STANDARD
            .decode(text.as_bytes())
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_frame_round_trips_with_tag_and_b64() {
        let f = VideoFrame::new("route:a→b", 3, 640, 360, 1920, 1080, vec![0xFF, 0xD8, 0xFF]);
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains("\"t\":\"video\""));
        assert!(json.contains("\"jpeg\":\""), "bytes travel as base64");
        let back: VideoFrame = serde_json::from_str(&json).unwrap();
        assert_eq!(f, back);
    }

    #[test]
    fn input_event_round_trips_each_action() {
        let actions = [
            InputAction::MouseMove { x: 0.25, y: 0.75 },
            InputAction::MouseButton {
                button: 2,
                down: true,
            },
            InputAction::Wheel { dx: 0.0, dy: -3.0 },
            InputAction::Key {
                key: "Enter".into(),
                down: false,
            },
        ];
        for action in actions {
            let e = InputEvent::new("r1", 9, action);
            let v = serde_json::to_value(&e).unwrap();
            assert_eq!(v["t"], "input");
            let back: InputEvent = serde_json::from_value(v).unwrap();
            assert_eq!(e, back);
        }
    }

    #[test]
    fn demux_dispatches_on_the_tag_with_audio_as_the_untagged_default() {
        let audio = serde_json::to_value(AudioFrame::new("r", 1, 48_000, 1, vec![1, 2])).unwrap();
        assert!(matches!(
            MediaPayload::decode(audio),
            Some(MediaPayload::Audio(f)) if f.route == "r"
        ));

        let video = serde_json::to_value(VideoFrame::new("r", 1, 8, 8, 8, 8, vec![1])).unwrap();
        assert!(matches!(
            MediaPayload::decode(video),
            Some(MediaPayload::Video(_))
        ));

        let input = serde_json::to_value(InputEvent::new(
            "r",
            1,
            InputAction::Key {
                key: "a".into(),
                down: true,
            },
        ))
        .unwrap();
        assert!(matches!(
            MediaPayload::decode(input),
            Some(MediaPayload::Input(_))
        ));

        // A future kind we don't know is dropped, not an error.
        assert_eq!(
            MediaPayload::decode(serde_json::json!({ "t": "hologram", "route": "r" })),
            None
        );
    }

    #[test]
    fn a_v1_audio_frame_still_decodes_unchanged() {
        // The exact shape a v0.1.0 peer sends — no tag.
        let legacy = serde_json::json!({
            "route": "route:mic→spk", "seq": 4, "sample_rate": 44_100,
            "channels": 1, "pcm": "AAABAA=="
        });
        let p = MediaPayload::decode(legacy).expect("legacy audio decodes");
        assert_eq!(p.route(), "route:mic→spk");
    }
}
