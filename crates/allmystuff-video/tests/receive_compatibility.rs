//! Route fallback assembly is compared with its original private implementation.
//! This deliberately does not substitute for canonical-peer ingress policy.

use std::collections::HashMap;

use allmystuff_video::receive::{self, PacedInboundAu};

// The original assembly refers to these node paths. Only its imports are
// supplied here; the frozen source is included without rewriting its body.
mod video_frame_timing {
    pub use allmystuff_frame_timing::AssemblyClock;
}

#[allow(dead_code)] // The frozen module also contains the unrelated splitter.
#[rustfmt::skip]
#[path = "baseline/wire_stub.rs"]
mod video;

#[derive(Debug, PartialEq, Eq)]
struct Complete {
    timestamp: u32,
    key: bool,
    data: Vec<u8>,
    chunks: usize,
    timed: bool,
}

mod original {
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    include!("baseline/route_assembly.rs");

    #[derive(Default)]
    pub(super) struct State(HashMap<String, PacedInboundAu>);

    impl State {
        pub(super) fn accept(
            &mut self,
            route: &str,
            timestamp: u32,
            key: bool,
            data: Vec<u8>,
        ) -> (Option<super::Complete>, bool) {
            let (complete, damaged) =
                accept_paced_fragment(&mut self.0, route, timestamp, key, data);
            (
                complete.map(|au| super::Complete {
                    timestamp: au.rtp_timestamp,
                    key: au.key,
                    data: au.data,
                    chunks: au.chunks,
                    timed: au.timing.is_some(),
                }),
                damaged,
            )
        }

        pub(super) fn keys(&self) -> Vec<String> {
            let mut keys: Vec<_> = self.0.keys().cloned().collect();
            keys.sort();
            keys
        }
    }
}

#[derive(Default)]
struct Pair {
    original: original::State,
    current: HashMap<String, PacedInboundAu>,
}

impl Pair {
    fn step(
        &mut self,
        route: &str,
        timestamp: u32,
        key: bool,
        data: Vec<u8>,
    ) -> (Option<Complete>, bool) {
        tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {
            let old = self.original.accept(route, timestamp, key, data.clone());
            let (complete, damaged) =
                receive::accept_paced_fragment(&mut self.current, route, timestamp, key, data);
            let actual = (
                complete.map(|au| Complete {
                    timestamp: au.rtp_timestamp,
                    key: au.key,
                    data: au.data,
                    chunks: au.chunks,
                    timed: au.timing.is_some(),
                }),
                damaged,
            );
            assert_eq!(actual, old);
            let mut keys: Vec<_> = self.current.keys().cloned().collect();
            keys.sort();
            assert_eq!(keys, self.original.keys());
            actual
        })
    }
}

fn marker(count: u16) -> Vec<u8> {
    // Literal original wire format, independent of the new marker builder.
    let mut bytes = b"\0\0\0\x01\x06\x05\x12AMS-PACED-AU-V1!\0\0\x80".to_vec();
    bytes[23..25].copy_from_slice(&count.to_le_bytes());
    bytes
}

fn completed(timestamp: u32, key: bool, data: &[u8], chunks: usize) -> (Option<Complete>, bool) {
    (
        Some(Complete {
            timestamp,
            key,
            data: data.to_vec(),
            chunks,
            timed: false,
        }),
        false,
    )
}

#[test]
fn completion_waits_for_exact_marker_and_ors_fragment_keys() {
    let mut pair = Pair::default();
    assert_eq!(pair.step("r", 9, false, vec![1, 2]), (None, false));
    assert_eq!(pair.step("r", 9, true, vec![3, 4]), (None, false));
    assert_eq!(pair.step("r", 9, false, marker(2)), completed(9, true, &[1, 2, 3, 4], 2));
    assert_eq!(pair.step("r", 9, true, marker(2)), (None, true));
    assert_eq!(pair.step("r", 10, false, vec![7]), (None, false));
    // A key flag on the closing marker does not mark the completed payload key.
    assert_eq!(pair.step("r", 10, true, marker(1)), completed(10, false, &[7], 1));
}

#[test]
fn missing_wrong_count_and_wrong_timestamp_markers_discard_pending() {
    for closing in [marker(0), marker(2)] {
        let mut pair = Pair::default();
        assert_eq!(pair.step("r", 1, true, closing.clone()), (None, true));
        assert_eq!(pair.step("r", 1, true, vec![9]), (None, false));
        assert_eq!(pair.step("r", 1, true, closing), (None, true));
        assert_eq!(pair.step("r", 1, true, marker(1)), (None, true));
    }
    let mut pair = Pair::default();
    assert_eq!(pair.step("r", 1, false, vec![1]), (None, false));
    assert_eq!(pair.step("r", 2, false, marker(1)), (None, true));
    assert_eq!(pair.step("r", 1, false, marker(1)), (None, true));
}

#[test]
fn new_timestamp_reports_damage_but_retains_new_train() {
    let mut pair = Pair::default();
    assert_eq!(pair.step("r", u32::MAX, true, vec![1]), (None, false));
    assert_eq!(pair.step("r", 0, false, vec![2]), (None, true));
    assert_eq!(pair.step("r", 0, false, marker(1)), completed(0, false, &[2], 1));
}

#[test]
fn route_keys_are_exact_and_empty_fragments_still_count() {
    let mut pair = Pair::default();
    assert_eq!(pair.step("peer-abc12", 3, false, vec![]), (None, false));
    assert_eq!(pair.step("peer", 4, false, vec![4]), (None, false));
    assert_eq!(pair.step("peer-abc12", 3, false, vec![3]), (None, false));
    assert_eq!(pair.step("peer", 4, false, marker(1)), completed(4, false, &[4], 1));
    assert_eq!(pair.step("peer-abc12", 3, false, marker(2)), completed(3, false, &[3], 2));
}

#[test]
fn marker_lookalike_is_payload_until_the_exact_closer() {
    let mut pair = Pair::default();
    let mut ordinary = marker(1);
    ordinary[25] ^= 1;
    assert_eq!(pair.step("r", 1, false, ordinary.clone()), (None, false));
    assert_eq!(pair.step("r", 1, false, marker(1)), completed(1, false, &ordinary, 1));
}

#[test]
fn route_chunk_ceiling_is_inclusive_then_recovery_discards_overflow() {
    let mut pair = Pair::default();
    for _ in 0..2048 {
        assert_eq!(pair.step("at-limit", 1, false, vec![]), (None, false));
        assert_eq!(pair.step("over-limit", 1, false, vec![]), (None, false));
    }
    assert_eq!(pair.step("at-limit", 1, false, marker(2048)), completed(1, false, &[], 2048));
    assert_eq!(pair.step("over-limit", 1, false, vec![]), (None, true));
    assert_eq!(pair.step("over-limit", 1, false, marker(2048)), (None, true));
}

#[test]
fn route_byte_ceiling_and_existing_first_fragment_exception_are_preserved() {
    const LIMIT: usize = 16 * 1024 * 1024;
    let mut pair = Pair::default();
    assert_eq!(pair.step("exact", 1, false, vec![7; LIMIT]), (None, false));
    assert_eq!(pair.step("exact", 1, false, vec![]), (None, false));
    let (complete, damaged) = pair.step("exact", 1, false, marker(2));
    assert!(!damaged);
    let complete = complete.unwrap();
    assert_eq!(complete.chunks, 2);
    assert_eq!(complete.data.len(), LIMIT);
    assert!(complete.data.iter().all(|&byte| byte == 7));
    drop(complete);

    assert_eq!(pair.step("append", 2, false, vec![8; LIMIT]), (None, false));
    assert_eq!(pair.step("append", 2, false, vec![9]), (None, true));
    assert_eq!(pair.step("append", 2, false, marker(1)), (None, true));

    // The original first insertion is not checked against the byte ceiling.
    assert_eq!(pair.step("first", 3, true, vec![6; LIMIT + 1]), (None, false));
    let (complete, damaged) = pair.step("first", 3, false, marker(1));
    assert!(!damaged);
    let complete = complete.unwrap();
    assert_eq!(complete.data.len(), LIMIT + 1);
    assert!(complete.key);
    assert_eq!(complete.chunks, 1);
}
