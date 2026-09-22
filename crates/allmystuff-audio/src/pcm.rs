//! The existing sample conversion, resampling and playback-buffer rules.

use std::collections::VecDeque;

/// Playout depth the inbound ring aims for — enough to ride normal
/// network jitter without underruns, small enough that audio stays in
/// step with the video stream (which drops stale frames and runs ~a
/// frame or two behind live). Lip-sync offsets under ~100 ms read as
/// "in sync".
pub const TARGET_DEPTH_MS: usize = 80;
/// Depth at which the playback bridge trims the ring back to
/// [`TARGET_DEPTH_MS`]. The gap between the two is hysteresis: a burst
/// must overshoot meaningfully before a trim (an audible skip) is paid.
pub const MAX_DEPTH_MS: usize = 200;

/// Append one decoded buffer and keep playback close to the live edge.
///
/// The ring is a jitter buffer, not a reservoir: whatever piles up beyond
/// `MAX_DEPTH_MS` becomes permanent lag behind video (which drops stale
/// frames), so trim the oldest samples back to `TARGET_DEPTH_MS`. Keeping this
/// operation independent of the OS output stream also lets its boundary
/// behavior be tested without opening a real audio device.
pub fn buffer_playback_samples(
    ring: &mut VecDeque<i16>,
    samples: impl IntoIterator<Item = i16>,
    out_rate: u32,
) -> Option<usize> {
    ring.extend(samples);
    let samples_per_ms = out_rate as usize / 1000;
    let max = samples_per_ms * MAX_DEPTH_MS;
    let target = samples_per_ms * TARGET_DEPTH_MS;
    if ring.len() <= max.max(1) {
        return None;
    }
    let held_ms = ring.len() / samples_per_ms.max(1);
    let excess = ring.len().saturating_sub(target);
    ring.drain(..excess);
    Some(held_ms)
}

pub fn f32_to_i16(f: f32) -> i16 {
    (f.clamp(-1.0, 1.0) * 32767.0) as i16
}

pub fn i16_to_f32(s: i16) -> f32 {
    s as f32 / 32768.0
}

/// Average `channels` interleaved samples down to one mono sample each.
pub fn downmix(samples: &[i16], channels: u16) -> Vec<i16> {
    let ch = channels.max(1) as usize;
    if ch == 1 {
        return samples.to_vec();
    }
    samples
        .chunks(ch)
        .map(|c| (c.iter().map(|&s| s as i32).sum::<i32>() / c.len() as i32) as i16)
        .collect()
}

/// Linear-interpolating resampler from `from_rate` to `to_rate` (mono).
pub fn resample_linear(samples: &[i16], from_rate: u32, to_rate: u32) -> Vec<i16> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmix_stereo_to_mono_averages() {
        assert_eq!(downmix(&[10, 20, 30, 40], 2), vec![15, 35]);
        assert_eq!(downmix(&[1, 2, 3], 1), vec![1, 2, 3]);
    }

    #[test]
    fn resample_upsamples_length() {
        let up = resample_linear(&[0, 100, 200, 300], 24_000, 48_000);
        assert_eq!(up.len(), 8);
        assert_eq!(up[0], 0);
        // Same rate is a passthrough.
        assert_eq!(resample_linear(&[1, 2, 3], 48_000, 48_000), vec![1, 2, 3]);
    }

    #[test]
    fn sample_conversions_round_trip_near_unity() {
        assert_eq!(f32_to_i16(0.0), 0);
        assert!(f32_to_i16(1.0) >= 32760);
        assert!((i16_to_f32(32767) - 1.0).abs() < 0.01);
    }

    #[test]
    fn playback_ring_trims_back_to_the_target_depth() {
        // The ring is a jitter buffer: anything beyond MAX_DEPTH_MS is
        // cut back to TARGET_DEPTH_MS (oldest samples dropped) so audio
        // can't fall permanently behind the video stream. Exercise the
        // buffer operation directly: opening a native output device would
        // test CPAL/WASAPI instead and make this depend on CI hardware.
        let mut ring = VecDeque::new();
        // Default device rate 48 kHz → target 3840 samples, max 9600.
        for _ in 0..5 {
            buffer_playback_samples(&mut ring, vec![1i16; 4800], 48_000); // 100 ms
        }
        // 500 ms went in; the trims must have pulled it back to target.
        assert_eq!(ring.len(), (48_000 / 1000) * TARGET_DEPTH_MS);
    }
}
