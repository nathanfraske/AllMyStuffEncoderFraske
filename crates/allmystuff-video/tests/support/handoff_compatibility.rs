//! Included inside handoff so memory limits can be tested without large buffers.
//! The oracle is the frozen node implementation with access helpers appended.

use std::time::{Duration, Instant};

use serde_json::Value;

use super::{Enqueue, Packet, VideoHandoff};
use crate::test_support::LegacyHandoffPolicy;

#[rustfmt::skip]
#[path = "handoff_oracle.rs"]
mod original;

type Current = VideoHandoff<LegacyHandoffPolicy>;

#[derive(Default)]
struct Pair {
    current: Current,
    original: original::VideoHandoff,
}

impl Pair {
    fn limited(max_age: Duration, max_bytes: usize) -> Self {
        Self {
            current: Current {
                max_age,
                max_bytes,
                ..Current::default()
            },
            original: original::with_limits(max_age, max_bytes),
        }
    }

    fn state(&self) {
        assert_eq!(
            (
                self.current.len(),
                self.current.bytes,
                self.current.awaiting_key,
                self.current.convergence_requested,
            ),
            original::state(&self.original)
        );
        assert_eq!(self.current.is_empty(), original::state(&self.original).0 == 0);
    }

    fn push(&mut self, data: Vec<u8>, now: Instant, gradual: bool) -> Enqueue {
        let old = self.original.push_h264(data.clone(), now, gradual);
        let expected = match old {
            original::Enqueue::Enqueued { skipped } => Enqueue::Enqueued { skipped },
            original::Enqueue::AwaitingKey { started } => Enqueue::AwaitingKey { started },
            original::Enqueue::Converging { skipped, started } => {
                Enqueue::Converging { skipped, started }
            }
        };
        let actual = self.current.push_h264(data, now, gradual);
        assert_eq!(actual, expected);
        self.state();
        actual
    }

    fn replace(&mut self, data: Vec<u8>) {
        self.original.replace(data.clone());
        self.current.replace(data);
        self.state();
    }

    fn take(&mut self) -> Vec<u8> {
        let old = self.original.take_batch();
        let actual = self.current.take_batch();
        assert_eq!(actual, old);
        self.state();
        actual
    }
}

fn hex(value: &str) -> Vec<u8> {
    assert_eq!(value.len() % 2, 0);
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn literal_outcome(step: &Value) -> Enqueue {
    let skipped = || usize::try_from(step["skipped"].as_u64().unwrap()).unwrap();
    match step["outcome"].as_str().unwrap() {
        "Enqueued" => Enqueue::Enqueued { skipped: skipped() },
        "AwaitingKey" => Enqueue::AwaitingKey {
            started: step["started"].as_bool().unwrap(),
        },
        "Converging" => Enqueue::Converging {
            skipped: skipped(),
            started: step["started"].as_bool().unwrap(),
        },
        unknown => panic!("unknown literal outcome {unknown}"),
    }
}

#[test]
fn source_derived_traces_preserve_results_batches_and_recovery_state() {
    let all: Value = serde_json::from_str(include_str!("../baseline/handoff_vectors.json")).unwrap();
    assert_eq!(Current::default().max_age, Duration::from_millis(200));
    assert_eq!(Current::default().max_bytes, 64 * 1024 * 1024);
    for trace in all["traces"].as_array().unwrap() {
        let start = Instant::now();
        let mut pair = Pair::default();
        for step in trace["steps"].as_array().unwrap() {
            let data = hex(step["hex"].as_str().unwrap());
            match step["op"].as_str().unwrap() {
                "push" => {
                    let now = start + Duration::from_millis(step["at_ms"].as_u64().unwrap());
                    let outcome = pair.push(data, now, step["gradual"].as_bool().unwrap());
                    assert_eq!(outcome, literal_outcome(step), "{}", trace["name"]);
                }
                "replace" => pair.replace(data),
                "take" => assert_eq!(pair.take(), data, "{}", trace["name"]),
                unknown => panic!("unknown trace operation {unknown}"),
            }
        }
    }
}

#[test]
fn packet_accounting_keeps_original_metadata_and_length_prefix_charge() {
    let now = Instant::now();
    for len in [0, 1, 2, 3, 1024] {
        let packet = Packet {
            data: vec![0; len],
            at: now,
        };
        let expected = len + 4 + 2 * std::mem::size_of::<Packet>();
        assert_eq!(packet.charge::<LegacyHandoffPolicy>(), expected);
        assert_eq!(original::packet_charge(len, now), expected);
    }
}

#[test]
fn memory_limit_accepts_exact_budget_then_fences_whole_reference_chain() {
    let now = Instant::now();
    let limit = original::packet_charge(3, now) * 2;
    let mut pair = Pair::limited(Duration::from_secs(1), limit);
    assert_eq!(pair.push(vec![2, 1, 1], now, false), Enqueue::Enqueued { skipped: 0 });
    assert_eq!(pair.push(vec![2, 0, 2], now, false), Enqueue::Enqueued { skipped: 0 });
    assert_eq!(pair.current.bytes, limit);
    assert_eq!(pair.push(vec![2, 0, 3], now, false), Enqueue::AwaitingKey { started: true });
    assert!(pair.take().is_empty());
    assert_eq!(pair.push(vec![2, 0, 4], now, false), Enqueue::AwaitingKey { started: false });
    assert_eq!(pair.push(vec![2, 1, 5], now, false), Enqueue::Enqueued { skipped: 0 });
    assert_eq!(pair.take(), hex("03000000020105"));
}

#[test]
fn memory_trimming_keeps_latest_whole_key_suffix_only_when_it_fits() {
    let now = Instant::now();
    let limit = original::packet_charge(3, now) * 3;
    let mut pair = Pair::limited(Duration::from_secs(1), limit);
    pair.push(vec![2, 0, 1], now, false);
    pair.push(vec![2, 1, 2], now, false);
    pair.push(vec![2, 0, 3], now, false);
    assert_eq!(pair.push(vec![2, 0, 4], now, false), Enqueue::Enqueued { skipped: 1 });
    assert_eq!(pair.current.bytes, limit);
    assert_eq!(pair.take(), hex("030000000201020300000002000303000000020004"));
}

#[test]
fn oversize_key_cannot_release_fence_but_replace_retains_its_separate_policy() {
    let now = Instant::now();
    let limit = original::packet_charge(3, now);
    let mut pair = Pair::limited(Duration::from_secs(1), limit);
    let mut large = vec![9; limit + 1];
    large[..2].copy_from_slice(&[2, 1]);
    assert_eq!(pair.push(large.clone(), now, false), Enqueue::AwaitingKey { started: true });
    assert_eq!(pair.push(large, now, true), Enqueue::AwaitingKey { started: false });
    assert!(pair.take().is_empty());
    assert!(pair.current.awaiting_key);
    // Original replace accepts a self-contained packet even beyond the cap.
    let replacement = vec![3; limit + 1];
    pair.replace(replacement.clone());
    assert!(pair.current.bytes > limit);
    assert!(!pair.current.awaiting_key);
    let batch = pair.take();
    assert_eq!(&batch[..4], &u32::try_from(replacement.len()).unwrap().to_le_bytes());
    assert_eq!(&batch[4..], replacement.as_slice());
}

#[test]
fn key_recognition_remains_specific_to_node_encoded_packet_prefix() {
    let start = Instant::now();
    let late = start + Duration::from_millis(200);
    let mut pair = Pair::default();
    pair.push(vec![2, 1, 1], start, false);
    assert_eq!(pair.push(vec![2, 0, 2], late, false), Enqueue::AwaitingKey { started: true });
    for data in [vec![], vec![2], vec![2, 2], vec![3, 1], vec![1, 1]] {
        assert_eq!(pair.push(data, late, false), Enqueue::AwaitingKey { started: false });
    }
    assert_eq!(pair.push(vec![2, 1], late, false), Enqueue::Enqueued { skipped: 0 });
    assert_eq!(pair.take(), hex("020000000201"));
}

#[test]
fn gradual_mode_can_leave_reset_fence_without_claiming_an_independent_key() {
    let start = Instant::now();
    let late = start + Duration::from_millis(200);
    let mut pair = Pair::default();
    pair.push(vec![2, 1, 1], start, false);
    assert_eq!(pair.push(vec![2, 0, 2], late, false), Enqueue::AwaitingKey { started: true });
    assert_eq!(
        pair.push(vec![2, 0, 3], late, true),
        Enqueue::Converging { skipped: 0, started: true }
    );
    assert!(!pair.current.awaiting_key);
    assert!(pair.current.convergence_requested);
    assert_eq!(pair.take(), hex("03000000020003"));
    assert!(!pair.current.convergence_requested);
}
