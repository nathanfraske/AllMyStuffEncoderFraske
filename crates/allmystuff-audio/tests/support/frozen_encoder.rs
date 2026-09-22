// Frozen encoder and resampler from e6340b31; see frozen_encoder_manifest.json.

// ---- the mesh's Opus audio lane (encode side) --------------------------

/// The audio lane's clock: Opus always runs a 48 kHz RTP clock.
pub(crate) const OPUS_RATE: u32 = 48_000;
/// 20 ms at 48 kHz — the canonical Opus frame.
pub(crate) const OPUS_FRAME_SAMPLES: usize = 960;
/// The same 20 ms, as the lane's RTP pacing value.
pub(crate) const OPUS_FRAME_US: u64 = 20_000;

/// Chops a capture stream (any rate, mono, buffers at the device's whim)
/// into the canonical 20 ms frames the mesh's Opus lane carries, encoding
/// each. Opus only accepts exact frame sizes, cpal promises nothing about
/// buffer cadence — this is the shim between them.
pub(crate) struct OpusStream {
    enc: opus::Encoder,
    /// 48 kHz mono samples awaiting a full frame.
    buf: Vec<i16>,
}

impl OpusStream {
    pub(crate) fn new() -> Result<Self, String> {
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
    pub(crate) fn push(&mut self, pcm: &[i16], rate: u32, mut emit: impl FnMut(Vec<u8>)) {
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
                Err(e) => tracing::debug!("opus encode failed: {e}"),
            }
            off += OPUS_FRAME_SAMPLES;
        }
        self.buf.drain(..off);
    }
}


/// Linear-interpolating resampler from `from_rate` to `to_rate` (mono).
pub(crate) fn resample_linear(samples: &[i16], from_rate: u32, to_rate: u32) -> Vec<i16> {
    if samples.is_empty() || from_rate == 0 || to_rate == 0 || from_rate == to_rate {
        return samples.to_vec();
    }
    let ratio = to_rate as f64 / from_rate as f64;
    let out_len = ((samples.len() as f64) * ratio) as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src = i as f64 / ratio;
        let i0 = src.floor() as usize;
        let frac = src - i0 as f64;
        let a = samples.get(i0).copied().unwrap_or(0) as f64;
        let b = samples.get(i0 + 1).copied().unwrap_or(a as i16) as f64;
        out.push((a + (b - a) * frac) as i16);
    }
    out
}

// Observation only: expose original retained input to the differential fixture.
pub(super) fn remaining(stream: &OpusStream) -> &[i16] {
    &stream.buf
}
