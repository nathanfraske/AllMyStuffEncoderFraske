// Frozen original PCM helpers; see frozen_pcm_manifest.json.
use std::collections::VecDeque;

const TARGET_DEPTH_MS: usize = 80;
/// Depth at which [`AudioBridge::feed`] trims the ring back to
/// [`TARGET_DEPTH_MS`]. The gap between the two is hysteresis: a burst
/// must overshoot meaningfully before a trim (an audible skip) is paid.
const MAX_DEPTH_MS: usize = 200;
fn buffer_playback_samples(
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
fn f32_to_i16(f: f32) -> i16 {
    (f.clamp(-1.0, 1.0) * 32767.0) as i16
}

fn i16_to_f32(s: i16) -> f32 {
    s as f32 / 32768.0
}

/// Average `channels` interleaved samples down to one mono sample each.
fn downmix(samples: &[i16], channels: u16) -> Vec<i16> {
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

// Observation adapters only; original helper bodies above are unchanged.
pub(super) fn from_f32(value: f32) -> i16 {
    f32_to_i16(value)
}
pub(super) fn to_f32(value: i16) -> f32 {
    i16_to_f32(value)
}
pub(super) fn mix(samples: &[i16], channels: u16) -> Vec<i16> {
    downmix(samples, channels)
}
pub(super) fn append(
    ring: &mut VecDeque<i16>,
    samples: impl IntoIterator<Item = i16>,
    rate: u32,
) -> Option<usize> {
    buffer_playback_samples(ring, samples, rate)
}
