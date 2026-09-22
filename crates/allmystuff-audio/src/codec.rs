//! Fixed 48 kHz mono Opus state for the existing audio lane.
//!
//! This module never opens an audio device. Decoder ownership and route
//! lifetime remain with the caller; packet errors leave the decoder in place.

use crate::pcm::resample_linear;
use crate::{OPUS_FRAME_SAMPLES, OPUS_RATE};

/// Chops a capture stream (any rate, mono, buffers at the device's whim)
/// into the canonical 20 ms frames the mesh's Opus lane carries, encoding
/// each. Opus only accepts exact frame sizes, cpal promises nothing about
/// buffer cadence — this is the shim between them.
pub struct OpusStream {
    enc: opus::Encoder,
    /// 48 kHz mono samples awaiting a full frame.
    buf: Vec<i16>,
}

impl OpusStream {
    pub fn new() -> Result<Self, String> {
        let mut enc = opus::Encoder::new(OPUS_RATE, opus::Channels::Mono, opus::Application::Audio)
            .map_err(|e| e.to_string())?;
        // System audio is music as often as speech — give the codec room
        // (96 kbps mono is transparent for almost everything; still ~10×
        // smaller than the PCM frames the channel fallback ships).
        enc.set_bitrate(opus::Bitrate::Bits(96_000))
            .map_err(|e| e.to_string())?;
        Ok(OpusStream {
            enc,
            buf: Vec::with_capacity(OPUS_FRAME_SAMPLES * 2),
        })
    }

    /// Push one captured buffer; `emit` is called once per completed
    /// 20 ms frame with the encoded packet.
    pub fn push(&mut self, pcm: &[i16], rate: u32, mut emit: impl FnMut(Vec<u8>)) {
        let resampled;
        let samples = if rate == OPUS_RATE || rate == 0 {
            pcm
        } else {
            resampled = resample_linear(pcm, rate, OPUS_RATE);
            &resampled[..]
        };
        self.buf.extend_from_slice(samples);
        let mut off = 0;
        while self.buf.len() - off >= OPUS_FRAME_SAMPLES {
            let frame = &self.buf[off..off + OPUS_FRAME_SAMPLES];
            match self.enc.encode_vec(frame, 4000) {
                Ok(pkt) => emit(pkt),
                // A failed frame costs 20 ms of sound, never the stream.
                Err(e) => tracing::debug!(target: "allmystuff_node::audio", "opus encode failed: {e}"),
            }
            off += OPUS_FRAME_SAMPLES;
        }
        self.buf.drain(..off);
    }
}

/// One route's stateful 48 kHz mono decoder.
///
/// The caller supplies its existing output allocation and truncates it to
/// the returned count. Decoding keeps the original non-FEC behavior, including
/// the codec's handling of empty packets and errors.
pub struct OpusDecoder {
    decoder: opus::Decoder,
}

impl OpusDecoder {
    pub fn new() -> Result<Self, opus::Error> {
        opus::Decoder::new(OPUS_RATE, opus::Channels::Mono).map(|decoder| Self { decoder })
    }

    pub fn decode(&mut self, data: &[u8], pcm: &mut [i16]) -> Result<usize, opus::Error> {
        self.decoder.decode(data, pcm, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opus_stream_frames_and_survives_a_roundtrip() {
        // Arbitrary capture cadence in (here: 30 ms buffers at 24 kHz),
        // canonical 20 ms Opus packets out — and what was encoded
        // decodes again, which is the whole contract with the far side.
        let mut stream = OpusStream::new().expect("encoder");
        let mut packets = Vec::new();
        // 3 × 30 ms at 24 kHz = 90 ms → resampled to 48 kHz → 4 full
        // frames (80 ms) plus a 10 ms remainder held in the buffer.
        let buf: Vec<i16> = (0..720).map(|i| ((i * 37) % 1000) as i16).collect();
        for _ in 0..3 {
            stream.push(&buf, 24_000, |pkt| packets.push(pkt));
        }
        assert_eq!(packets.len(), 4, "90 ms yields 4 complete frames");
        assert!(
            stream.buf.len() < OPUS_FRAME_SAMPLES,
            "remainder stays buffered"
        );

        let mut dec = opus::Decoder::new(OPUS_RATE, opus::Channels::Mono).expect("decoder");
        let mut out = vec![0i16; OPUS_FRAME_SAMPLES * 6];
        let n = dec.decode(&packets[0], &mut out, false).expect("decode");
        assert_eq!(n, OPUS_FRAME_SAMPLES, "one packet = one 20 ms frame");
    }
}
