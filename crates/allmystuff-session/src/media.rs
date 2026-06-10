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

/// One JPEG-encoded frame of a display route's stream — or one *chunk* of
/// it: the mesh's data channel caps a message at ~64 KiB (the WebRTC SCTP
/// transport's maximum message size), and a desktop frame routinely beats
/// that, so a large frame travels as several messages sharing a `seq`,
/// reassembled by [`VideoAssembler`] at the sink. Losing any chunk just
/// loses that frame — the next one stands alone, MJPEG's whole virtue.
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
    /// This piece's index within `chunks`. Defaults (0 of 1) mean "the
    /// whole frame in one message" — the common case for small frames.
    #[serde(default)]
    pub chunk: u16,
    #[serde(default = "one_chunk")]
    pub chunks: u16,
    /// The JPEG bytes (of this chunk), base64 on the wire (the daemon
    /// channel is JSON).
    #[serde(with = "bytes_b64")]
    pub jpeg: Vec<u8>,
}

fn one_chunk() -> u16 {
    1
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
            chunk: 0,
            chunks: 1,
            jpeg,
        }
    }

    /// Split this frame into channel-sized pieces: each piece's JPEG slice
    /// is at most `max_jpeg_bytes`, so the full JSON message (base64 +
    /// envelope) stays under the transport's message ceiling. A frame that
    /// already fits comes back whole.
    pub fn into_chunks(self, max_jpeg_bytes: usize) -> Vec<VideoFrame> {
        let max = max_jpeg_bytes.max(1);
        if self.jpeg.len() <= max {
            return vec![self];
        }
        let pieces: Vec<&[u8]> = self.jpeg.chunks(max).collect();
        let total = pieces.len() as u16;
        pieces
            .into_iter()
            .enumerate()
            .map(|(i, piece)| VideoFrame {
                t: MediaTagVideo::Video,
                route: self.route.clone(),
                seq: self.seq,
                width: self.width,
                height: self.height,
                source_width: self.source_width,
                source_height: self.source_height,
                chunk: i as u16,
                chunks: total,
                jpeg: piece.to_vec(),
            })
            .collect()
    }
}

/// Reassembles chunked [`VideoFrame`]s per route. Frames are independent:
/// a newer `seq` discards any half-built older frame (that frame is simply
/// lost, never shown torn), and hostile shapes (absurd chunk counts,
/// out-of-range indices, ballooning totals) are dropped wholesale.
#[derive(Debug, Default)]
pub struct VideoAssembler {
    partial: std::collections::HashMap<String, PartialFrame>,
}

#[derive(Debug)]
struct PartialFrame {
    seq: u64,
    parts: Vec<Option<Vec<u8>>>,
    received: u16,
}

/// Upper bounds a frame may occupy mid-assembly — far above anything the
/// sender produces, low enough that a misbehaving peer can't balloon us.
const MAX_CHUNKS: u16 = 64;
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

impl VideoAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one inbound frame (or chunk). Returns the complete frame once
    /// every piece has arrived — immediately, for unchunked frames.
    pub fn push(&mut self, frame: VideoFrame) -> Option<VideoFrame> {
        if frame.chunks <= 1 {
            self.partial.remove(&frame.route);
            return Some(frame);
        }
        if frame.chunks > MAX_CHUNKS || frame.chunk >= frame.chunks {
            return None;
        }
        let entry = self.partial.entry(frame.route.clone());
        let p = match entry {
            std::collections::hash_map::Entry::Occupied(mut o) => {
                if o.get().seq < frame.seq || o.get().parts.len() != frame.chunks as usize {
                    *o.get_mut() = PartialFrame::new(frame.seq, frame.chunks);
                } else if o.get().seq > frame.seq {
                    return None; // a stale chunk of an abandoned frame
                }
                o.into_mut()
            }
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(PartialFrame::new(frame.seq, frame.chunks))
            }
        };
        let slot = &mut p.parts[frame.chunk as usize];
        if slot.is_none() {
            p.received += 1;
        }
        *slot = Some(frame.jpeg);
        let assembled_len: usize = p.parts.iter().flatten().map(Vec::len).sum();
        if assembled_len > MAX_FRAME_BYTES {
            self.partial.remove(&frame.route);
            return None;
        }
        if p.received < frame.chunks {
            return None;
        }
        let p = self.partial.remove(&frame.route)?;
        let mut jpeg = Vec::with_capacity(assembled_len);
        for part in p.parts.into_iter().flatten() {
            jpeg.extend_from_slice(&part);
        }
        Some(VideoFrame {
            t: MediaTagVideo::Video,
            route: frame.route,
            seq: frame.seq,
            width: frame.width,
            height: frame.height,
            source_width: frame.source_width,
            source_height: frame.source_height,
            chunk: 0,
            chunks: 1,
            jpeg,
        })
    }
}

impl PartialFrame {
    fn new(seq: u64, chunks: u16) -> Self {
        PartialFrame {
            seq,
            parts: vec![None; chunks as usize],
            received: 0,
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
    fn chunking_splits_big_frames_and_reassembles_them_byte_exact() {
        let jpeg: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        let frame = VideoFrame::new("r", 7, 1280, 720, 1920, 1080, jpeg.clone());
        let chunks = frame.into_chunks(40_000);
        assert_eq!(chunks.len(), 3);
        assert!(chunks
            .iter()
            .all(|c| c.jpeg.len() <= 40_000 && c.chunks == 3));

        let mut asm = VideoAssembler::new();
        // Out-of-order arrival still assembles.
        assert!(asm.push(chunks[2].clone()).is_none());
        assert!(asm.push(chunks[0].clone()).is_none());
        let full = asm.push(chunks[1].clone()).expect("complete");
        assert_eq!(full.jpeg, jpeg);
        assert_eq!((full.seq, full.chunk, full.chunks), (7, 0, 1));

        // A small frame passes through untouched and unsplit.
        let small = VideoFrame::new("r", 8, 8, 8, 8, 8, vec![1, 2, 3]);
        assert_eq!(small.clone().into_chunks(40_000).len(), 1);
        assert_eq!(asm.push(small).unwrap().jpeg, vec![1, 2, 3]);
    }

    #[test]
    fn a_newer_frame_discards_a_half_built_older_one() {
        let old = VideoFrame::new("r", 1, 8, 8, 8, 8, vec![0u8; 100]).into_chunks(40);
        let new = VideoFrame::new("r", 2, 8, 8, 8, 8, vec![1u8; 100]).into_chunks(40);
        let mut asm = VideoAssembler::new();
        assert!(asm.push(old[0].clone()).is_none());
        // The newer frame arrives; the old partial is abandoned.
        for c in &new[..new.len() - 1] {
            assert!(asm.push(c.clone()).is_none());
        }
        // A stale chunk of the abandoned frame can't corrupt the new one.
        assert!(asm.push(old[1].clone()).is_none());
        let full = asm
            .push(new[new.len() - 1].clone())
            .expect("new frame completes");
        assert!(full.jpeg.iter().all(|&b| b == 1));
    }

    #[test]
    fn hostile_chunk_shapes_are_dropped() {
        let mut asm = VideoAssembler::new();
        let mut absurd = VideoFrame::new("r", 1, 8, 8, 8, 8, vec![0]);
        absurd.chunks = 60_000;
        absurd.chunk = 5;
        assert!(asm.push(absurd).is_none());

        let mut out_of_range = VideoFrame::new("r", 1, 8, 8, 8, 8, vec![0]);
        out_of_range.chunks = 4;
        out_of_range.chunk = 4; // index == count
        assert!(asm.push(out_of_range).is_none());
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
