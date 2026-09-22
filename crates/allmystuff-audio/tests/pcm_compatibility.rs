//! Independent literal vectors and the original PCM helpers, with no devices.

use allmystuff_audio::pcm;
use serde_json::Value;
use std::collections::VecDeque;

#[rustfmt::skip]
#[path = "support/frozen_pcm.rs"]
mod original;

fn samples(value: &Value) -> Vec<i16> {
    value
        .as_array()
        .expect("sample array")
        .iter()
        .map(|value| i16::try_from(value.as_i64().expect("signed sample")).expect("i16 sample"))
        .collect()
}

fn sample_vectors() -> Value {
    serde_json::from_str(include_str!("baseline/sample_vectors.json")).expect("frozen vectors")
}

#[test]
fn capture_conversion_matches_independent_float_bit_literals() {
    for row in sample_vectors()["f32_to_i16"].as_array().unwrap() {
        let bits = u32::from_str_radix(row["bits"].as_str().unwrap(), 16).unwrap();
        let input = f32::from_bits(bits);
        let expected = row["expected"].as_i64().unwrap() as i16;
        assert_eq!(original::from_f32(input), expected, "original {bits:08x}");
        assert_eq!(pcm::f32_to_i16(input), expected, "extracted {bits:08x}");
    }
}

#[test]
fn playback_conversion_preserves_the_different_denominator() {
    for row in sample_vectors()["i16_to_f32"].as_array().unwrap() {
        let input = row["sample"].as_i64().unwrap() as i16;
        let expected = u32::from_str_radix(row["expected_bits"].as_str().unwrap(), 16).unwrap();
        assert_eq!(original::to_f32(input).to_bits(), expected);
        assert_eq!(pcm::i16_to_f32(input).to_bits(), expected);
    }
    assert_eq!(pcm::f32_to_i16(pcm::i16_to_f32(i16::MIN)), -32767);
}

#[test]
fn downmix_preserves_zero_channels_partial_groups_and_signed_truncation() {
    for row in sample_vectors()["downmix"].as_array().unwrap() {
        let input = samples(&row["input"]);
        let expected = samples(&row["expected"]);
        let channels = row["channels"].as_u64().unwrap() as u16;
        assert_eq!(original::mix(&input, channels), expected);
        assert_eq!(pcm::downmix(&input, channels), expected);
    }
}

#[test]
fn downmix_accepts_the_full_channel_bound_without_narrow_accumulation() {
    for sample in [i16::MIN, i16::MAX] {
        let mut input = vec![sample; u16::MAX as usize];
        input.extend_from_slice(&[7, -2]);
        let expected = vec![sample, 2];
        assert_eq!(original::mix(&input, u16::MAX), expected);
        assert_eq!(pcm::downmix(&input, u16::MAX), expected);
    }
}

#[test]
fn resampler_matches_independent_literal_vectors() {
    let rows: Value = serde_json::from_str(include_str!("baseline/resample_vectors.json")).unwrap();
    for row in rows.as_array().unwrap() {
        let input = samples(&row["input"]);
        let expected = samples(&row["expected"]);
        let from = row["from"].as_u64().unwrap() as u32;
        let to = row["to"].as_u64().unwrap() as u32;
        let name = row["name"].as_str().unwrap();
        assert_eq!(
            original::resample_linear(&input, from, to),
            expected,
            "original {name}"
        );
        assert_eq!(
            pcm::resample_linear(&input, from, to),
            expected,
            "extracted {name}"
        );
    }
}

#[test]
fn fractional_resampling_remains_independent_for_each_capture_buffer() {
    let together = pcm::resample_linear(&[-100, 100], 3, 4);
    let mut apart = pcm::resample_linear(&[-100], 3, 4);
    apart.extend(pcm::resample_linear(&[100], 3, 4));
    assert_eq!(together, vec![-100, 50]);
    assert_eq!(apart, vec![-100, 100]);
    assert_eq!(together, original::resample_linear(&[-100, 100], 3, 4));
    let mut old_apart = original::resample_linear(&[-100], 3, 4);
    old_apart.extend(original::resample_linear(&[100], 3, 4));
    assert_eq!(apart, old_apart);
}

#[test]
fn resampler_matches_frozen_helpers_across_rates_lengths_and_extrema() {
    let rates = [
        (0, 48000),
        (48000, 0),
        (44100, 44100),
        (44100, 48000),
        (48000, 44100),
        (16000, 48000),
        (48000, 16000),
        (8000, 12000),
        (32000, 24000),
        (u32::MAX, u32::MAX - 1),
    ];
    for len in [0usize, 1, 2, 3, 4, 17, 257] {
        let input: Vec<i16> = (0..len)
            .map(|index| match index % 4 {
                0 => i16::MIN,
                1 => i16::MAX,
                2 => 0,
                _ => ((index * 997 + 333) % 65536) as i16,
            })
            .collect();
        for (from, to) in rates {
            assert_eq!(
                pcm::resample_linear(&input, from, to),
                original::resample_linear(&input, from, to),
                "len={len}, rate={from}->{to}"
            );
        }
    }
}

#[test]
fn playback_ring_preserves_strict_threshold_tail_and_low_rate_literals() {
    let rows: Value = serde_json::from_str(include_str!("baseline/buffer_vectors.json")).unwrap();
    for row in rows.as_array().unwrap() {
        let initial = if let Some(range) = row.get("initial_range") {
            let start = range[0].as_i64().unwrap();
            let count = range[1].as_u64().unwrap();
            (0..count)
                .map(|offset| (start + offset as i64) as i16)
                .collect()
        } else {
            samples(&row["initial"])
        };
        let mut old_ring = VecDeque::from(initial);
        let mut new_ring = old_ring.clone();
        let append = samples(&row["append"]);
        let rate = row["rate"].as_u64().unwrap() as u32;
        let expected = row["held_ms"].as_u64().map(|value| value as usize);
        assert_eq!(
            original::append(&mut old_ring, append.clone(), rate),
            expected
        );
        assert_eq!(
            pcm::buffer_playback_samples(&mut new_ring, append, rate),
            expected
        );
        assert_eq!(new_ring, old_ring, "{}", row["name"].as_str().unwrap());
        assert_eq!(new_ring.len(), row["len"].as_u64().unwrap() as usize);
        assert_eq!(
            new_ring.front().copied(),
            row["first"].as_i64().map(|value| value as i16)
        );
        assert_eq!(
            new_ring.back().copied(),
            row["last"].as_i64().map(|value| value as i16)
        );
    }
}

#[test]
fn repeated_bursts_preserve_hysteresis_and_report_the_pretrim_depth() {
    let mut old_ring = VecDeque::new();
    let mut new_ring = VecDeque::new();
    let expected = [None, None, Some(300), None, Some(280)];
    for held in expected {
        assert_eq!(original::append(&mut old_ring, vec![1; 4800], 48000), held);
        assert_eq!(
            pcm::buffer_playback_samples(&mut new_ring, vec![1; 4800], 48000),
            held
        );
        assert_eq!(new_ring, old_ring);
    }
    assert_eq!(new_ring, VecDeque::from(vec![1; 3840]));
}
