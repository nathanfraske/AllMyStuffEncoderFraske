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
        /// Which of the remote's monitors the coordinates are normalized
        /// over — the `screen:<id>` capability's id, so control follows
        /// the screen the console is showing. `None` = the primary (and
        /// what an older sender's events decode to).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        screen: Option<u32>,
    },
    /// `button`: 0 left, 1 middle, 2 right (the DOM convention).
    MouseButton { button: u8, down: bool },
    /// Scroll in wheel lines (positive = down / right).
    Wheel { dx: f64, dy: f64 },
    /// `key` is the DOM `KeyboardEvent.key` value — a printable character
    /// ("a", "?") or a named key ("Enter", "ArrowLeft", "Shift").
    Key { key: String, down: bool },
}

/// One event of a terminal route's stream — the byte-level conversation
/// between an xterm.js viewer and the PTY a host spawned for it. Bytes are
/// opaque to the wire (the emulator and the PTY speak VT between
/// themselves); the frame just carries them, plus the two control events a
/// session needs: the viewer resizing its emulator, and the host reporting
/// the shell's end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermFrame {
    /// Tag for demuxing off the shared media channel. Always `"term"`.
    pub t: MediaTagTerm,
    pub route: String,
    pub seq: u64,
    #[serde(flatten)]
    pub event: TermEvent,
}

/// What happened on the terminal route. `Data` flows both ways (keystrokes
/// up, PTY output down); `Resize` only viewer → host; `Exit` only host →
/// viewer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TermEvent {
    /// Raw PTY bytes, base64 on the wire (the daemon channel is JSON).
    Data {
        #[serde(with = "bytes_b64")]
        bytes: Vec<u8>,
    },
    /// The viewer's emulator was resized; the host resizes the PTY so the
    /// shell relays out at the right dimensions.
    Resize { cols: u16, rows: u16 },
    /// The shell ended. `None` = killed / no status to report.
    Exit {
        #[serde(default)]
        code: Option<i32>,
    },
}

impl TermFrame {
    pub fn new(route: impl Into<String>, seq: u64, event: TermEvent) -> Self {
        TermFrame {
            t: MediaTagTerm::Term,
            route: route.into(),
            seq,
            event,
        }
    }

    /// Split `bytes` into channel-sized [`TermEvent::Data`] frames — each
    /// carries at most `max_bytes` so the full JSON message (base64 +
    /// envelope) stays under the transport's ceiling. Sequence numbers
    /// increment per piece starting at `first_seq`; an empty payload still
    /// yields one (empty) frame so a write is never silently dropped.
    pub fn data_frames(
        route: &str,
        first_seq: u64,
        bytes: &[u8],
        max_bytes: usize,
    ) -> Vec<TermFrame> {
        let max = max_bytes.max(1);
        if bytes.len() <= max {
            return vec![TermFrame::new(
                route,
                first_seq,
                TermEvent::Data {
                    bytes: bytes.to_vec(),
                },
            )];
        }
        bytes
            .chunks(max)
            .enumerate()
            .map(|(i, piece)| {
                TermFrame::new(
                    route,
                    first_seq + i as u64,
                    TermEvent::Data {
                        bytes: piece.to_vec(),
                    },
                )
            })
            .collect()
    }
}

/// Everything that can arrive on the media channel, demuxed by the `t`
/// tag (no tag = audio, the original frame shape).
#[derive(Debug, Clone, PartialEq)]
pub enum MediaPayload {
    Audio(AudioFrame),
    Video(VideoFrame),
    Input(InputEvent),
    Terminal(TermFrame),
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
            Some("term") => serde_json::from_value(payload)
                .ok()
                .map(MediaPayload::Terminal),
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
            MediaPayload::Terminal(f) => &f.route,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MediaTagTerm {
    #[default]
    #[serde(rename = "term")]
    Term,
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
            InputAction::MouseMove {
                x: 0.25,
                y: 0.75,
                screen: None,
            },
            InputAction::MouseMove {
                x: 0.5,
                y: 0.5,
                screen: Some(131_073),
            },
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

        let term =
            serde_json::to_value(TermFrame::new("r", 1, TermEvent::Data { bytes: vec![27] }))
                .unwrap();
        assert!(matches!(
            MediaPayload::decode(term),
            Some(MediaPayload::Terminal(_))
        ));

        // A future kind we don't know is dropped, not an error.
        assert_eq!(
            MediaPayload::decode(serde_json::json!({ "t": "hologram", "route": "r" })),
            None
        );
    }

    #[test]
    fn term_frame_round_trips_each_event() {
        let events = [
            TermEvent::Data {
                bytes: b"ls -la\r".to_vec(),
            },
            TermEvent::Resize {
                cols: 120,
                rows: 40,
            },
            TermEvent::Exit { code: Some(3) },
            TermEvent::Exit { code: None },
        ];
        for event in events {
            let f = TermFrame::new("route:a:terminal→b:term-view:1", 9, event);
            let v = serde_json::to_value(&f).unwrap();
            assert_eq!(v["t"], "term");
            let back: TermFrame = serde_json::from_value(v).unwrap();
            assert_eq!(f, back);
        }
        // Data bytes travel as base64, never raw.
        let f = TermFrame::new("r", 0, TermEvent::Data { bytes: vec![0xFF] });
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains("\"bytes\":\"/w==\""));
    }

    #[test]
    fn term_frame_with_an_unknown_kind_drops_alone() {
        // A newer peer's new event kind fails *that frame only* — decode
        // returns None and the stream's surviving frames are unaffected.
        assert_eq!(
            MediaPayload::decode(serde_json::json!({
                "t": "term", "route": "r", "seq": 4, "kind": "hologram"
            })),
            None
        );
        let ok = serde_json::to_value(TermFrame::new("r", 5, TermEvent::Data { bytes: vec![1] }))
            .unwrap();
        assert!(matches!(
            MediaPayload::decode(ok),
            Some(MediaPayload::Terminal(f)) if f.seq == 5
        ));
    }

    #[test]
    fn term_data_chunks_split_byte_exact_and_number_sequentially() {
        let bytes: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
        let frames = TermFrame::data_frames("r", 7, &bytes, 16 * 1024);
        assert_eq!(frames.len(), 4);
        let mut rebuilt = Vec::new();
        for (i, f) in frames.iter().enumerate() {
            assert_eq!(f.seq, 7 + i as u64);
            match &f.event {
                TermEvent::Data { bytes } => {
                    assert!(bytes.len() <= 16 * 1024);
                    rebuilt.extend_from_slice(bytes);
                }
                other => panic!("expected Data, got {other:?}"),
            }
        }
        assert_eq!(rebuilt, bytes);

        // A small write passes through as exactly one frame; an empty one
        // still yields a frame rather than vanishing.
        assert_eq!(TermFrame::data_frames("r", 0, b"hi", 16 * 1024).len(), 1);
        assert_eq!(TermFrame::data_frames("r", 0, b"", 16 * 1024).len(), 1);
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
    fn a_screenless_mouse_move_still_decodes() {
        // The exact shape an older sender (no per-screen control) emits.
        let legacy = serde_json::json!({
            "t": "input", "route": "r", "seq": 1,
            "kind": "mouse_move", "x": 0.5, "y": 0.5
        });
        let ev: InputEvent = serde_json::from_value(legacy).expect("legacy decodes");
        assert_eq!(
            ev.action,
            InputAction::MouseMove {
                x: 0.5,
                y: 0.5,
                screen: None
            }
        );
        // And the screenless shape serializes without the field, so an
        // older *receiver* isn't handed a key it never knew.
        let v = serde_json::to_value(&ev).unwrap();
        assert!(v.get("screen").is_none());
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
