//! Hardware-free comparisons against the frozen encoder and original decoder calls.

use super::{OpusDecoder, OpusStream};
use std::panic::{catch_unwind, AssertUnwindSafe};

#[allow(dead_code)]
#[rustfmt::skip]
#[path = "frozen_encoder.rs"]
mod original;

fn signal(start: usize, len: usize) -> Vec<i16> {
    (start..start + len)
        .map(|i| (((i * 997 + 17) % 60001) as i32 - 30000) as i16)
        .collect()
}

struct Pair {
    old: original::OpusStream,
    new: OpusStream,
}

impl Pair {
    fn new() -> Self {
        Self {
            old: original::OpusStream::new().expect("original encoder"),
            new: OpusStream::new().expect("extracted encoder"),
        }
    }

    fn push(&mut self, pcm: &[i16], rate: u32) -> Vec<Vec<u8>> {
        let mut old_packets = Vec::new();
        let mut new_packets = Vec::new();
        self.old.push(pcm, rate, |packet| old_packets.push(packet));
        self.new.push(pcm, rate, |packet| new_packets.push(packet));
        assert_eq!(new_packets, old_packets);
        assert_eq!(self.new.buf, original::remaining(&self.old));
        new_packets
    }
}

#[test]
fn arbitrary_capture_cadence_matches_frozen_packet_counts_and_remainders() {
    let vectors: serde_json::Value =
        serde_json::from_str(include_str!("../baseline/contract_vectors.json")).unwrap();
    for row in vectors["opus_sequences"].as_array().unwrap() {
        let mut pair = Pair::new();
        let mut offset = 0;
        for (index, count) in row["push_samples"].as_array().unwrap().iter().enumerate() {
            let count = count.as_u64().unwrap() as usize;
            let packets = pair.push(&signal(offset, count), row["rate"].as_u64().unwrap() as u32);
            assert_eq!(packets.len(), row["emitted_per_push"][index].as_u64().unwrap() as usize);
            assert_eq!(pair.new.buf.len(), row["remaining_per_push"][index].as_u64().unwrap() as usize);
            offset += count;
        }
    }
}

#[test]
fn zero_capture_rate_matches_the_existing_48khz_passthrough() {
    let mut zero = Pair::new();
    let mut fixed = Pair::new();
    for (start, len) in [(0, 13), (13, 1907), (1920, 1), (1921, 959)] {
        let pcm = signal(start, len);
        assert_eq!(zero.push(&pcm, 0), fixed.push(&pcm, 48000));
        assert_eq!(zero.new.buf, fixed.new.buf);
    }
}

#[test]
fn mixed_rate_pushes_keep_original_state_and_stateless_resampling() {
    let mut pair = Pair::new();
    for (index, (rate, len)) in [
        (44100, 881), (24000, 721), (48000, 19), (32000, 1001),
        (0, 0), (96000, 1931), (8000, 161), (48000, 3000),
    ].into_iter().enumerate() {
        pair.push(&signal(index * 1000, len), rate);
    }
    let mut split = Pair::new();
    split.push(&[-100], 32000);
    split.push(&[100], 32000);
    assert_eq!(split.new.buf, [-100, 100]);
    let mut combined = Pair::new();
    combined.push(&[-100, 100], 32000);
    assert_eq!(combined.new.buf, [-100, 33, 100]);
}

#[test]
fn synchronous_emit_panic_keeps_original_input_and_advanced_codec_state() {
    let mut pair = Pair::new();
    let pcm = signal(0, 1920);
    let mut old_packets = Vec::new();
    let mut new_packets = Vec::new();
    let old_panic = catch_unwind(AssertUnwindSafe(|| {
        pair.old.push(&pcm, 48000, |packet| {
            old_packets.push(packet);
            if old_packets.len() == 2 { panic!("second original emit"); }
        });
    }));
    let new_panic = catch_unwind(AssertUnwindSafe(|| {
        pair.new.push(&pcm, 48000, |packet| {
            new_packets.push(packet);
            if new_packets.len() == 2 { panic!("second extracted emit"); }
        });
    }));
    assert!(old_panic.is_err() && new_panic.is_err());
    assert_eq!(new_packets.len(), 2);
    assert_eq!(new_packets, old_packets);
    assert_eq!(original::remaining(&pair.old), pcm);
    assert_eq!(pair.new.buf, pcm);
    assert_eq!(pair.push(&[], 48000).len(), 2);
    assert!(pair.new.buf.is_empty());
    assert_eq!(pair.push(&signal(1920, 960), 48000).len(), 1);
}

fn packets() -> Vec<Vec<u8>> {
    // Both independent encoders must agree before their packets drive decoding.
    let mut pair = Pair::new();
    let result = pair.push(&signal(0, 960 * 5), 48000);
    assert_eq!(result.len(), 5);
    result
}

struct Decoders {
    old: opus::Decoder,
    new: OpusDecoder,
}

impl Decoders {
    fn new() -> Self {
        Self {
            old: opus::Decoder::new(48000, opus::Channels::Mono).unwrap(),
            new: OpusDecoder::new().unwrap(),
        }
    }

    fn decode(&mut self, packet: &[u8], capacity: usize) -> Result<Vec<i16>, String> {
        let mut old_pcm = vec![12345; capacity];
        let mut new_pcm = old_pcm.clone();
        // This is the original Mesh call, including false FEC and caller allocation.
        let old = self.old.decode(packet, &mut old_pcm, false);
        let new = self.new.decode(packet, &mut new_pcm);
        assert_eq!(new_pcm, old_pcm, "entire caller buffer, including the untouched tail");
        match (old, new) {
            (Ok(old_count), Ok(new_count)) => {
                assert_eq!(new_count, old_count);
                new_pcm.truncate(new_count);
                Ok(new_pcm)
            }
            (Err(old), Err(new)) => {
                assert_eq!(new.code(), old.code());
                assert_eq!(new.to_string(), old.to_string());
                Err(new.to_string())
            }
            _ => panic!("original and extracted decode outcome differ"),
        }
    }
}

#[test]
fn decoder_keeps_state_over_multiple_original_packet_frames() {
    let mut decoders = Decoders::new();
    let mut audible = false;
    for packet in packets() {
        let pcm = decoders.decode(&packet, 5760).unwrap();
        assert_eq!(pcm.len(), 960);
        audible |= pcm.iter().any(|&sample| sample != 0);
    }
    assert!(audible);
}

#[test]
fn empty_packets_preserve_plc_before_between_and_after_valid_frames() {
    let mut decoders = Decoders::new();
    assert_eq!(decoders.decode(&[], 5760).unwrap().len(), 5760);
    for packet in packets() {
        assert_eq!(decoders.decode(&packet, 5760).unwrap().len(), 960);
        assert_eq!(decoders.decode(&[], 960).unwrap().len(), 960);
    }
    assert_eq!(decoders.decode(&[], 1920).unwrap().len(), 1920);
}

#[test]
fn malformed_packet_errors_preserve_next_valid_decoder_behavior() {
    let mut decoders = Decoders::new();
    let packets = packets();
    decoders.decode(&packets[0], 5760).unwrap();
    // Opus framing code 3 without its frame-count byte is incomplete.
    for malformed in [&[0xff][..], &[0x03][..]] {
        assert!(decoders.decode(malformed, 5760).is_err());
        assert_eq!(decoders.decode(&packets[1], 5760).unwrap().len(), 960);
    }
    assert_eq!(decoders.decode(&packets[2], 5760).unwrap().len(), 960);
}

#[test]
fn short_output_errors_preserve_caller_buffer_and_later_decoder_state() {
    let packets = packets();
    let mut decoders = Decoders::new();
    for capacity in [0, 1, 959] {
        assert!(decoders.decode(&packets[0], capacity).is_err());
        assert_eq!(decoders.decode(&packets[1], 5760).unwrap().len(), 960);
    }
}
