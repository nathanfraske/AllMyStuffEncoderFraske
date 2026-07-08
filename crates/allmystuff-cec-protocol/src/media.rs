//! The binary media-frame codec for the screen/audio plane.
//!
//! Screen frames (H.264 access units, or MJPEG on the fallback path) and audio
//! (Opus) travel as length-prefixed binary frames rather than JSON, so a
//! high-bitrate screen stream pays no base64/JSON tax. This mirrors the
//! `[u32 len][body]` framing AllMyStuff's node uses over its media pipes; CEC
//! Support reuses the exact shape so the media node can be shared.
//!
//! Frame layout (little-endian):
//!
//! ```text
//! offset  size  field
//! 0       1     kind        MEDIA_KIND_VIDEO | MEDIA_KIND_AUDIO
//! 1       1     stream      sub-stream / monitor index
//! 2       1     key         1 = keyframe (video), else 0
//! 3       4     timestamp   media timestamp (µs for audio, RTP ts for video)
//! 7       4     len         payload length
//! 11      len   payload     codec bytes
//! ```

/// Video (screen) frame kind.
pub const MEDIA_KIND_VIDEO: u8 = 0;
/// Audio frame kind.
pub const MEDIA_KIND_AUDIO: u8 = 1;

const HEADER_LEN: usize = 11;

/// A decoded media frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaFrame {
    /// [`MEDIA_KIND_VIDEO`] or [`MEDIA_KIND_AUDIO`].
    pub kind: u8,
    /// Sub-stream / monitor index.
    pub stream: u8,
    /// Whether this is a keyframe (video).
    pub key: bool,
    /// Media timestamp (codec-defined).
    pub timestamp: u32,
    /// Codec payload bytes.
    pub data: Vec<u8>,
}

/// Encode a media frame into a self-describing byte buffer (see module docs).
pub fn encode_media_frame(kind: u8, stream: u8, key: bool, timestamp: u32, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + data.len());
    out.push(kind);
    out.push(stream);
    out.push(u8::from(key));
    out.extend_from_slice(&timestamp.to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
    out
}

/// Decode a frame produced by [`encode_media_frame`]. Returns `None` if the
/// buffer is truncated or the declared length overruns — a corrupt or partial
/// frame is dropped, never a panic.
pub fn decode_media_frame(body: &[u8]) -> Option<MediaFrame> {
    if body.len() < HEADER_LEN {
        return None;
    }
    let kind = body[0];
    let stream = body[1];
    let key = body[2] != 0;
    let timestamp = u32::from_le_bytes(body[3..7].try_into().ok()?);
    let len = u32::from_le_bytes(body[7..11].try_into().ok()?) as usize;
    let data = body.get(HEADER_LEN..HEADER_LEN + len)?.to_vec();
    Some(MediaFrame {
        kind,
        stream,
        key,
        timestamp,
        data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let frame = MediaFrame {
            kind: MEDIA_KIND_VIDEO,
            stream: 2,
            key: true,
            timestamp: 123_456,
            data: vec![9, 8, 7, 6, 5],
        };
        let bytes = encode_media_frame(
            frame.kind,
            frame.stream,
            frame.key,
            frame.timestamp,
            &frame.data,
        );
        assert_eq!(decode_media_frame(&bytes), Some(frame));
    }

    #[test]
    fn empty_payload_round_trips() {
        let bytes = encode_media_frame(MEDIA_KIND_AUDIO, 0, false, 0, &[]);
        let back = decode_media_frame(&bytes).unwrap();
        assert_eq!(back.kind, MEDIA_KIND_AUDIO);
        assert!(back.data.is_empty());
    }

    #[test]
    fn truncated_frames_return_none() {
        assert_eq!(decode_media_frame(&[0, 1, 2]), None);
        // Header claims 10 bytes but none follow.
        let mut bytes = encode_media_frame(MEDIA_KIND_VIDEO, 0, true, 1, &[1, 2, 3]);
        bytes.truncate(HEADER_LEN + 1);
        assert_eq!(decode_media_frame(&bytes), None);
    }
}
