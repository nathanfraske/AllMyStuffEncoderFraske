//! Route fallback assembly is compared with its original private implementation.
//! BOUND-01 deliberately rejects oversized initial/replacement fragments that
//! the frozen implementation retained; those differences are asserted explicitly.
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

fn current_step(
    pending: &mut HashMap<String, PacedInboundAu>,
    route: &str,
    timestamp: u32,
    key: bool,
    data: Vec<u8>,
) -> (Option<Complete>, bool) {
    tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {
        let (complete, damaged) =
            receive::accept_paced_fragment(pending, route, timestamp, key, data);
        (
            complete.map(|au| Complete {
                timestamp: au.rtp_timestamp,
                key: au.key,
                data: au.data,
                chunks: au.chunks,
                timed: au.timing.is_some(),
            }),
            damaged,
        )
    })
}

// Independent contract value, not imported from the implementation under test.
const ROUTE_BYTE_LIMIT: usize = 16 * 1024 * 1024;

#[test]
fn completion_waits_for_exact_marker_and_ors_fragment_keys() {
    let mut pair = Pair::default();
    assert_eq!(pair.step("r", 9, false, vec![1, 2]), (None, false));
    assert_eq!(pair.step("r", 9, true, vec![3, 4]), (None, false));
    assert_eq!(
        pair.step("r", 9, false, marker(2)),
        completed(9, true, &[1, 2, 3, 4], 2)
    );
    assert_eq!(pair.step("r", 9, true, marker(2)), (None, true));
    assert_eq!(pair.step("r", 10, false, vec![7]), (None, false));
    // A key flag on the closing marker does not mark the completed payload key.
    assert_eq!(
        pair.step("r", 10, true, marker(1)),
        completed(10, false, &[7], 1)
    );
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
    assert_eq!(
        pair.step("r", 0, false, marker(1)),
        completed(0, false, &[2], 1)
    );
}

#[test]
fn route_keys_are_exact_and_empty_fragments_still_count() {
    let mut pair = Pair::default();
    assert_eq!(pair.step("peer-abc12", 3, false, vec![]), (None, false));
    assert_eq!(pair.step("peer", 4, false, vec![4]), (None, false));
    assert_eq!(pair.step("peer-abc12", 3, false, vec![3]), (None, false));
    assert_eq!(
        pair.step("peer", 4, false, marker(1)),
        completed(4, false, &[4], 1)
    );
    assert_eq!(
        pair.step("peer-abc12", 3, false, marker(2)),
        completed(3, false, &[3], 2)
    );
}

#[test]
fn marker_lookalike_is_payload_until_the_exact_closer() {
    let mut pair = Pair::default();
    let mut ordinary = marker(1);
    ordinary[25] ^= 1;
    assert_eq!(pair.step("r", 1, false, ordinary.clone()), (None, false));
    assert_eq!(
        pair.step("r", 1, false, marker(1)),
        completed(1, false, &ordinary, 1)
    );
}

#[test]
fn route_chunk_ceiling_is_inclusive_then_recovery_discards_overflow() {
    let mut pair = Pair::default();
    for _ in 0..2048 {
        assert_eq!(pair.step("at-limit", 1, false, vec![]), (None, false));
        assert_eq!(pair.step("over-limit", 1, false, vec![]), (None, false));
    }
    assert_eq!(
        pair.step("at-limit", 1, false, marker(2048)),
        completed(1, false, &[], 2048)
    );
    assert_eq!(pair.step("over-limit", 1, false, vec![]), (None, true));
    assert_eq!(
        pair.step("over-limit", 1, false, marker(2048)),
        (None, true)
    );
}

#[test]
fn route_byte_ceiling_is_inclusive_for_first_and_replacement_fragments() {
    for replaces_pending in [false, true] {
        let mut pair = Pair::default();
        if replaces_pending {
            assert_eq!(pair.step("r", 1, true, vec![9]), (None, false));
        }
        assert_eq!(
            pair.step("r", 2, false, vec![7; ROUTE_BYTE_LIMIT]),
            (None, replaces_pending)
        );
        // Empty continuations still count, without making an exact-limit AU
        // oversized. The replaced unit's key bit must not leak into this one.
        assert_eq!(pair.step("r", 2, false, vec![]), (None, false));
        let (complete, damaged) = pair.step("r", 2, true, marker(2));
        assert!(!damaged);
        let complete = complete.unwrap();
        assert_eq!(complete.timestamp, 2);
        assert!(!complete.key);
        assert_eq!(complete.chunks, 2);
        assert_eq!(complete.data.len(), ROUTE_BYTE_LIMIT);
        assert!(complete.data.iter().all(|&byte| byte == 7));
        assert!(pair.current.is_empty());
    }
}

#[test]
fn route_byte_ceiling_is_inclusive_across_continuations() {
    let mut pair = Pair::default();
    assert_eq!(
        pair.step("r", 1, false, vec![7; ROUTE_BYTE_LIMIT - 1]),
        (None, false)
    );
    assert_eq!(pair.step("r", 1, true, vec![8]), (None, false));
    let (complete, damaged) = pair.step("r", 1, false, marker(2));
    assert!(!damaged);
    let complete = complete.unwrap();
    assert_eq!(complete.timestamp, 1);
    assert!(complete.key);
    assert_eq!(complete.chunks, 2);
    assert_eq!(complete.data.len(), ROUTE_BYTE_LIMIT);
    assert!(complete.data[..ROUTE_BYTE_LIMIT - 1]
        .iter()
        .all(|&byte| byte == 7));
    assert_eq!(complete.data[ROUTE_BYTE_LIMIT - 1], 8);
    assert!(pair.current.is_empty());
}

#[test]
fn continuation_byte_overflow_clears_pending_and_recovers_after_rejected_closers() {
    // Cover a sum that exceeds the limit by one although each fragment fits,
    // and a small pending unit followed by an individually oversized fragment.
    // Both same-timestamp rejections agree with the frozen append behavior.
    for (initial_len, continuation_len) in [(ROUTE_BYTE_LIMIT - 1, 2), (1, ROUTE_BYTE_LIMIT + 1)] {
        let mut pair = Pair::default();
        assert_eq!(
            pair.step("r", 1, false, vec![7; initial_len]),
            (None, false)
        );
        assert_eq!(
            pair.step("r", 1, true, vec![8; continuation_len]),
            (None, true)
        );
        assert!(pair.current.is_empty());
        for count in [1, 2] {
            assert_eq!(pair.step("r", 1, false, marker(count)), (None, true));
            assert!(pair.current.is_empty());
        }
        assert_eq!(pair.step("r", 2, false, vec![4, 5]), (None, false));
        assert_eq!(
            pair.step("r", 2, true, marker(1)),
            completed(2, false, &[4, 5], 1)
        );
    }
}

#[test]
fn oversized_first_fragment_is_rejected_instead_of_the_historical_exception() {
    let mut pair = Pair::default();
    // Intentional BOUND-01 divergence: the original admitted a single large
    // first fragment and could deliver it when its one-fragment marker arrived.
    assert_eq!(
        pair.original
            .accept("r", 3, true, vec![6; ROUTE_BYTE_LIMIT + 1]),
        (None, false)
    );
    assert_eq!(
        current_step(
            &mut pair.current,
            "r",
            3,
            true,
            vec![6; ROUTE_BYTE_LIMIT + 1],
        ),
        (None, true)
    );
    assert_eq!(pair.original.keys(), ["r".to_string()]);
    assert!(pair.current.is_empty());
    assert_eq!(
        current_step(&mut pair.current, "r", 3, false, marker(1)),
        (None, true)
    );
    let (complete, damaged) = pair.original.accept("r", 3, false, marker(1));
    assert!(!damaged);
    let complete = complete.unwrap();
    assert_eq!(complete.timestamp, 3);
    assert_eq!(complete.data.len(), ROUTE_BYTE_LIMIT + 1);
    assert!(complete.data.iter().all(|&byte| byte == 6));
    assert!(complete.key);
    assert_eq!(complete.chunks, 1);
    drop(complete);
    // Closing the frozen oversized AU restores comparable empty state.
    assert_eq!(pair.step("r", 3, false, marker(1)), (None, true));
    assert_eq!(pair.step("r", 4, false, vec![1, 2]), (None, false));
    assert_eq!(
        pair.step("r", 4, true, marker(1)),
        completed(4, false, &[1, 2], 1)
    );
}

#[test]
fn oversized_replacement_clears_the_route_instead_of_retaining_a_new_train() {
    // Exercise closers for both the rejected replacement and the displaced AU.
    for closing_timestamp in [0, u32::MAX] {
        let mut pair = Pair::default();
        assert_eq!(pair.step("r", u32::MAX, true, vec![9]), (None, false));
        let old = pair
            .original
            .accept("r", 0, false, vec![6; ROUTE_BYTE_LIMIT + 1]);
        let current = current_step(
            &mut pair.current,
            "r",
            0,
            false,
            vec![6; ROUTE_BYTE_LIMIT + 1],
        );
        // Both signal displaced-unit damage, but only the historical version
        // retains the oversized replacement. Compare that difference directly.
        assert_eq!(old, (None, true));
        assert_eq!(current, (None, true));
        assert_eq!(pair.original.keys(), ["r".to_string()]);
        assert!(pair.current.is_empty());
        assert_eq!(
            current_step(
                &mut pair.current,
                "r",
                closing_timestamp,
                true,
                marker(1),
            ),
            (None, true)
        );
        let old = pair.original.accept("r", closing_timestamp, true, marker(1));
        if closing_timestamp == 0 {
            let (complete, damaged) = old;
            assert!(!damaged);
            let complete = complete.unwrap();
            assert_eq!(complete.timestamp, 0);
            assert!(!complete.key);
            assert_eq!(complete.chunks, 1);
            assert_eq!(complete.data.len(), ROUTE_BYTE_LIMIT + 1);
            assert!(complete.data.iter().all(|&byte| byte == 6));
        } else {
            assert_eq!(old, (None, true));
        }
        assert_eq!(pair.step("r", 0, false, marker(1)), (None, true));
        assert_eq!(pair.step("r", 1, false, vec![2, 3]), (None, false));
        assert_eq!(
            pair.step("r", 1, true, marker(1)),
            completed(1, false, &[2, 3], 1)
        );
    }
}

#[test]
fn rejected_oversized_route_leaves_other_routes_live_and_allows_later_recovery() {
    for replaces_pending in [false, true] {
        let mut current = HashMap::new();
        assert_eq!(
            current_step(&mut current, "peer", 90, false, vec![4]),
            (None, false)
        );
        if replaces_pending {
            assert_eq!(
                current_step(&mut current, "peer-abc12", 1, true, vec![9]),
                (None, false)
            );
        }
        assert_eq!(
            current_step(
                &mut current,
                "peer-abc12",
                2,
                true,
                vec![6; ROUTE_BYTE_LIMIT + 1],
            ),
            (None, true)
        );
        assert_eq!(current.len(), 1);
        assert!(current.contains_key("peer"));
        assert!(!current.contains_key("peer-abc12"));
        // The other route progresses before any marker/recovery on the bad
        // route. Similar route strings must not acquire peer canonicalization.
        assert_eq!(
            current_step(&mut current, "peer", 90, true, vec![5]),
            (None, false)
        );
        assert_eq!(
            current_step(&mut current, "peer", 90, false, marker(2)),
            completed(90, true, &[4, 5], 2)
        );
        for stamp in [1, 2] {
            assert_eq!(
                current_step(&mut current, "peer-abc12", stamp, false, marker(1)),
                (None, true)
            );
            assert!(current.is_empty());
        }
        assert_eq!(
            current_step(&mut current, "peer-abc12", 3, false, vec![1]),
            (None, false)
        );
        assert_eq!(
            current_step(&mut current, "peer-abc12", 3, true, marker(1)),
            completed(3, false, &[1], 1)
        );
        assert!(current.is_empty());
    }
}
