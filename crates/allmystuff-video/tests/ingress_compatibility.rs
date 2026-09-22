//! Frozen node ingress decisions compared through real bounded local channels.
//! BOUND-01 deliberately rejects oversized initial/replacement fragments; that
//! divergence is asserted separately without changing the historical oracle.
//! No executor, daemon, socket, capture device, or global environment is used.

use allmystuff_video::ingress::{Event, Frame, Freshness, Sink, TrySendError};
use tokio::sync::mpsc;

// Supply the original module paths without changing any frozen method body.
#[allow(dead_code)] // Frozen helpers include unrelated insertion/split operations.
#[rustfmt::skip]
#[path = "baseline/metadata.rs"]
mod video_wire;
#[allow(dead_code)]
#[rustfmt::skip]
#[path = "baseline/wire_stub.rs"]
mod video;
#[allow(dead_code)]
#[rustfmt::skip]
#[path = "baseline/codec.rs"]
mod video_decode;
mod video_frame_timing {
    pub use allmystuff_frame_timing::{periodic_sample, AssemblyClock};
}

include!("baseline/inbound_frame.rs");

#[allow(dead_code)] // Includes frozen queue constants and the Unframed variant.
mod original {
    use super::InboundFrame;
    use std::collections::HashMap;
    use std::time::{Duration, Instant};
    use tokio::sync::mpsc;

    include!("baseline/ingress.rs");

    #[derive(Default)]
    pub(super) struct State(InboundVideoFreshness);

    impl State {
        pub(super) fn send(
            &mut self,
            action: super::Action,
            frame: InboundFrame,
            tx: &mpsc::Sender<InboundVideoEvent>,
        ) -> bool {
            match action {
                super::Action::Frame => self.0.forward(frame, tx),
                super::Action::Paced => self.0.forward_paced(frame, tx),
                super::Action::Gap => self.0.forward_transport_discontinuity(frame, tx),
            }
        }

        pub(super) fn pending(&self) -> bool {
            !self.0.paced.is_empty()
        }

        pub(super) fn discard(&mut self, frame: &InboundFrame) {
            self.0.discard_paced_lane(frame);
        }
    }
}

fn old_frame(frame: Frame<u8>) -> InboundFrame {
    InboundFrame {
        kind: frame.context,
        key: frame.key,
        stream: frame.stream,
        rtp_timestamp: frame.rtp_timestamp,
        from: frame.from,
        data: frame.data,
    }
}

fn normalize_frame(frame: InboundFrame) -> Frame<u8> {
    Frame {
        context: frame.kind,
        key: frame.key,
        stream: frame.stream,
        rtp_timestamp: frame.rtp_timestamp,
        from: frame.from,
        data: frame.data,
    }
}

fn normalize(event: original::InboundVideoEvent) -> Event<u8> {
    match event {
        original::InboundVideoEvent::Frame(frame) => Event::Frame(normalize_frame(frame)),
        original::InboundVideoEvent::Unframed(frame) => Event::Unframed(normalize_frame(frame)),
        original::InboundVideoEvent::Discontinuity {
            from,
            stream,
            reason,
            entry,
        } => Event::Discontinuity {
            from,
            stream,
            reason,
            entry: entry.map(normalize_frame),
        },
    }
}

struct Queue<'a>(&'a mpsc::Sender<Event<u8>>);

impl Sink<u8> for Queue<'_> {
    fn try_send(&self, event: Event<u8>) -> Result<(), TrySendError<u8>> {
        match self.0.try_send(event) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(event)) => Err(TrySendError::Full(event)),
            Err(mpsc::error::TrySendError::Closed(event)) => Err(TrySendError::Closed(event)),
        }
    }
}

#[derive(Clone, Copy)]
enum Action {
    Frame,
    Paced,
    Gap,
}

struct Pair {
    original: original::State,
    current: Freshness<u8>,
    old_tx: mpsc::Sender<original::InboundVideoEvent>,
    old_rx: mpsc::Receiver<original::InboundVideoEvent>,
    new_tx: mpsc::Sender<Event<u8>>,
    new_rx: mpsc::Receiver<Event<u8>>,
}

impl Pair {
    fn new(capacity: usize) -> Self {
        let (old_tx, old_rx) = mpsc::channel(capacity);
        let (new_tx, new_rx) = mpsc::channel(capacity);
        Self {
            original: original::State::default(),
            current: Freshness::new(allmystuff_video::framing::paced_au_marker_count),
            old_tx,
            old_rx,
            new_tx,
            new_rx,
        }
    }

    fn send(&mut self, action: Action, frame: Frame<u8>) -> bool {
        tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {
            let expected = self
                .original
                .send(action, old_frame(frame.clone()), &self.old_tx);
            let queue = Queue(&self.new_tx);
            let actual = match action {
                Action::Frame => self.current.forward(frame, &queue),
                Action::Paced => self.current.forward_paced(frame, &queue),
                Action::Gap => self.current.forward_transport_discontinuity(frame, &queue),
            };
            assert_eq!(actual, expected, "queue feedback return");
            assert_eq!(self.current.has_pending_paced(), self.original.pending());
            assert_eq!(self.new_tx.capacity(), self.old_tx.capacity());
            actual
        })
    }

    fn drain(&mut self) -> Vec<Event<u8>> {
        let mut expected = Vec::new();
        while let Ok(event) = self.old_rx.try_recv() {
            expected.push(normalize(event));
        }
        let mut actual = Vec::new();
        while let Ok(event) = self.new_rx.try_recv() {
            actual.push(event);
        }
        assert_eq!(
            actual, expected,
            "delivered event order and exact envelopes"
        );
        actual
    }

    fn close(&mut self) {
        self.old_rx.close();
        self.new_rx.close();
    }

    fn discard(&mut self, frame: Frame<u8>, borrowed_lane: bool) {
        self.original.discard(&old_frame(frame.clone()));
        if borrowed_lane {
            self.current
                .discard_paced_peer_lane(&frame.from, frame.stream);
        } else {
            self.current.discard_paced_lane(&frame);
        }
        assert_eq!(self.current.has_pending_paced(), self.original.pending());
    }
}

const OVERFLOW: &str = "AMS complete-AU ingress queue overflow";
const TRANSPORT: &str = "MyOwnMesh reported media discontinuity (transport or daemon IPC)";
const MISSING: &str = "paced AU expired or missing end marker";
const BOUNDS: &str = "paced AU exceeded assembly bounds";
const NO_FRAGMENTS: &str = "paced AU marker without fragments";
// Independent contract value: changing the implementation's limit must not
// silently change these boundary fixtures. Run these bounded large cases serially.
const BYTE_LIMIT: usize = 16 * 1024 * 1024;

// The frozen Pair remains strict. Only intentionally changed behavior uses
// this current-only harness; every emitted payload still gets a bound check.
struct Current {
    state: Freshness<u8>,
    tx: mpsc::Sender<Event<u8>>,
    rx: mpsc::Receiver<Event<u8>>,
}

impl Current {
    fn new(capacity: usize) -> Self {
        let (tx, rx) = mpsc::channel(capacity);
        Self {
            state: Freshness::new(allmystuff_video::framing::paced_au_marker_count),
            tx,
            rx,
        }
    }

    fn send(&mut self, action: Action, frame: Frame<u8>) -> bool {
        tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {
            let queue = Queue(&self.tx);
            match action {
                Action::Frame => self.state.forward(frame, &queue),
                Action::Paced => self.state.forward_paced(frame, &queue),
                Action::Gap => self.state.forward_transport_discontinuity(frame, &queue),
            }
        })
    }

    fn drain(&mut self) -> Vec<Event<u8>> {
        let mut events = Vec::new();
        while let Ok(event) = self.rx.try_recv() {
            let payload = match &event {
                Event::Frame(frame) | Event::Unframed(frame) => Some(frame),
                Event::Discontinuity { entry, .. } => entry.as_ref(),
            };
            if let Some(frame) = payload {
                assert!(frame.data.len() <= BYTE_LIMIT, "oversized emitted AU");
            }
            events.push(event);
        }
        events
    }
}

fn frame(from: &str, stream: u8, stamp: u32, key: bool, data: &[u8]) -> Frame<u8> {
    Frame {
        context: 2,
        key,
        stream,
        rtp_timestamp: stamp,
        from: from.into(),
        data: data.into(),
    }
}

fn sized_frame(from: &str, stream: u8, stamp: u32, len: usize) -> Frame<u8> {
    let mut out = frame(from, stream, stamp, true, &[]);
    // Allocate directly in the owned frame, avoiding a second large temporary.
    out.data = vec![9; len];
    out
}

fn delta(stamp: u32) -> Frame<u8> {
    frame("peer", 1, stamp, false, &[0, 0, 1, 0x41, stamp as u8])
}

fn gradual(stamp: u32) -> Frame<u8> {
    let mut out = delta(stamp);
    video_wire::insert_au_identity_marker(
        &mut out.data,
        video_wire::AuIdentity {
            sequence: u64::from(stamp),
            recovery: video_wire::AuRecovery::Gradual,
        },
        false,
    );
    out
}

fn closing(from: &str, stream: u8, stamp: u32, count: usize) -> Frame<u8> {
    // Build through the frozen marker implementation, not the extracted one.
    frame(from, stream, stamp, false, &video::paced_au_marker(count))
}

fn gap(from: &str, stream: u8, reason: &'static str, entry: Option<Frame<u8>>) -> Event<u8> {
    Event::Discontinuity {
        from: from.into(),
        stream,
        reason,
        entry,
    }
}

#[test]
fn four_slots_overflow_without_replay_and_recover_only_after_real_admission() {
    assert_eq!(original::MEDIA_VIDEO_QUEUE_CAPACITY, 4);
    assert_eq!(original::MEDIA_AUDIO_QUEUE_CAPACITY, 8);
    let mut pair = Pair::new(4);
    for stamp in 0..4 {
        assert!(pair.send(Action::Frame, delta(stamp)));
    }
    assert!(pair.send(Action::Frame, delta(4))); // First loss fences this lane.
    let clean = frame("peer", 1, 5, true, &[9]);
    assert!(pair.send(Action::Frame, clean.clone())); // Full again; no recovery.
    assert_eq!(
        pair.drain(),
        (0..4).map(|n| Event::Frame(delta(n))).collect::<Vec<_>>()
    );
    assert!(pair.send(Action::Frame, delta(6)));
    assert_eq!(pair.drain(), vec![gap("peer", 1, OVERFLOW, None)]);
    assert!(pair.send(Action::Frame, delta(7))); // Marker delivered, delta suppressed.
    assert!(pair.drain().is_empty());
    assert!(pair.send(Action::Frame, clean.clone()));
    assert_eq!(pair.drain(), vec![Event::Frame(clean)]);
    assert!(pair.send(Action::Frame, delta(8)));
    assert_eq!(pair.drain(), vec![Event::Frame(delta(8))]);
}

#[test]
fn closure_is_observed_at_send_not_before_suppressed_delta_or_queued_marker() {
    let mut pair = Pair::new(1);
    assert!(pair.send(Action::Gap, delta(0)));
    assert_eq!(pair.drain(), vec![gap("peer", 1, TRANSPORT, None)]);
    pair.close();
    assert!(pair.send(Action::Frame, delta(1))); // No send occurs in this branch.
    assert!(pair.send(Action::Gap, delta(2))); // Marker was already queued.
    assert!(!pair.send(Action::Frame, frame("peer", 1, 3, true, &[])));
    assert!(pair.drain().is_empty());
    let mut fresh = Pair::new(1);
    fresh.close();
    assert!(!fresh.send(Action::Frame, delta(0)));
    assert!(!fresh.send(Action::Gap, delta(1)));
}

#[test]
fn clean_entry_can_be_flagged_or_classified_and_is_carried_with_first_gap() {
    for (key, data) in [
        (true, &[][..]),
        (false, &[0, 0, 1, 0x67][..]),
        (false, &[0, 0, 1, 0x40][..]),
        (false, &[0x08][..]),
    ] {
        let mut pair = Pair::new(1);
        assert!(pair.send(Action::Frame, delta(0)));
        assert!(pair.send(Action::Frame, delta(1)));
        pair.drain();
        let clean = frame("peer", 1, 2, key, data);
        assert!(pair.send(Action::Frame, clean.clone()));
        assert_eq!(pair.drain(), vec![gap("peer", 1, OVERFLOW, Some(clean))]);
        assert!(pair.send(Action::Frame, delta(3)));
        assert_eq!(pair.drain(), vec![Event::Frame(delta(3))]);
    }
}

#[test]
fn gradual_frames_remain_inline_until_admitted_then_normal_forwarding_resumes() {
    let mut pair = Pair::new(1);
    assert!(pair.send(Action::Frame, delta(0)));
    assert!(pair.send(Action::Frame, gradual(1))); // Identity read before full result.
    assert!(pair.send(Action::Frame, gradual(2))); // Retain recovery across full.
    pair.drain();
    assert!(pair.send(Action::Frame, gradual(3)));
    assert_eq!(
        pair.drain(),
        vec![gap("peer", 1, OVERFLOW, Some(gradual(3)))]
    );
    assert!(pair.send(Action::Frame, delta(4)));
    assert_eq!(pair.drain(), vec![Event::Frame(delta(4))]);
    assert!(pair.send(Action::Gap, delta(5)));
    assert_eq!(pair.drain(), vec![gap("peer", 1, TRANSPORT, None)]);
    assert!(pair.send(Action::Frame, delta(6))); // Gradual gap success removed fence.
    assert_eq!(pair.drain(), vec![Event::Frame(delta(6))]);
}

#[test]
fn identity_changes_recovery_mode_even_while_the_lane_is_fenced() {
    let mut pair = Pair::new(1);
    assert!(pair.send(Action::Gap, delta(0)));
    pair.drain();
    assert!(pair.send(Action::Frame, gradual(1)));
    assert_eq!(
        pair.drain(),
        vec![gap("peer", 1, TRANSPORT, Some(gradual(1)))]
    );
    assert!(pair.send(Action::Frame, delta(2)));
    assert!(pair.send(Action::Frame, gradual(3))); // Full with remembered Gradual.
    pair.drain();
    let mut reset = delta(4);
    video_wire::insert_au_identity_marker(
        &mut reset.data,
        video_wire::AuIdentity {
            sequence: 4,
            recovery: video_wire::AuRecovery::Reset,
        },
        false,
    );
    assert!(pair.send(Action::Frame, reset));
    assert_eq!(pair.drain(), vec![gap("peer", 1, OVERFLOW, None)]);
    assert!(pair.send(Action::Frame, delta(5)));
    assert!(pair.drain().is_empty());
}

#[test]
fn only_five_ascii_alphanumeric_suffixes_canonicalize_and_streams_stay_separate() {
    let mut pair = Pair::new(1);
    let blocked = frame("peer-Ab123", 2, 0, false, &[1]);
    assert!(pair.send(Action::Frame, blocked.clone()));
    assert!(pair.send(Action::Frame, blocked));
    pair.drain();
    let same = frame("peer-Z9x8Q", 2, 1, false, &[1]);
    assert!(pair.send(Action::Frame, same));
    assert_eq!(pair.drain(), vec![gap("peer-Z9x8Q", 2, OVERFLOW, None)]);
    for (from, stream) in [
        ("peer-Xabcd", 3),
        ("peer-a_123", 2),
        ("peer-1234", 2),
        ("peer-ab-XYZ12", 2),
        ("Peer-Ab123", 2),
        ("peer-é123", 2),
    ] {
        let independent = frame(from, stream, 2, false, &[1]);
        assert!(pair.send(Action::Frame, independent.clone()));
        assert_eq!(pair.drain(), vec![Event::Frame(independent)]);
    }
    assert!(pair.send(Action::Frame, frame("peer", 2, 3, false, &[1])));
    assert!(pair.drain().is_empty());
}

#[test]
fn paced_fragments_keep_first_envelope_or_key_and_close_as_one_queue_item() {
    let mut pair = Pair::new(4);
    let mut first = frame("peer-Ab123", 2, 90_000, false, &[1, 2]);
    first.context = 77; // Opaque kind is retained from the first fragment.
    assert!(pair.send(Action::Paced, first.clone()));
    let mut second = frame("peer-Xy987", 2, 90_000, true, &[3]);
    second.context = 88;
    assert!(pair.send(Action::Paced, second));
    assert!(pair.drain().is_empty());
    assert!(pair.send(Action::Paced, closing("peer", 2, 90_000, 2)));
    first.key = true;
    first.data.push(3);
    assert_eq!(pair.drain(), vec![Event::Frame(first)]);
    assert!(!pair.current.has_pending_paced());
}

#[test]
fn missing_or_mismatched_closing_markers_preserve_exact_gap_reasons() {
    for (fragment, stamp, count, reason) in [
        (false, 100, 0, "paced AU marker without fragments"),
        (true, 101, 1, "paced AU timestamp/count mismatch"),
        (true, 100, 0, "paced AU timestamp/count mismatch"),
        (true, 100, 2, "paced AU timestamp/count mismatch"),
    ] {
        let mut pair = Pair::new(4);
        if fragment {
            assert!(pair.send(Action::Paced, frame("peer", 1, 100, true, &[1])));
        }
        assert!(pair.send(Action::Paced, closing("peer", 1, stamp, count)));
        assert_eq!(pair.drain(), vec![gap("peer", 1, reason, None)]);
        assert!(!pair.current.has_pending_paced());
    }
}

#[test]
fn replacement_survives_full_feedback_and_first_loss_reason_is_retained() {
    let mut pair = Pair::new(1);
    assert!(pair.send(Action::Frame, frame("other", 1, 0, false, &[1])));
    assert!(pair.send(Action::Paced, frame("peer", 1, 100, true, &[2])));
    let replacement = frame("peer", 1, 101, true, &[3]);
    assert!(pair.send(Action::Paced, replacement.clone())); // Gap cannot yet enter queue.
                                                            // Drops pending while retaining the first loss reason.
    assert!(pair.send(Action::Gap, frame("peer", 1, 102, false, &[])));
    pair.drain();
    assert!(pair.send(Action::Paced, replacement.clone()));
    assert!(pair.send(Action::Paced, closing("peer", 1, 101, 1)));
    assert_eq!(
        pair.drain(),
        vec![gap("peer", 1, MISSING, Some(replacement))]
    );
}

#[test]
fn transport_gap_and_both_borrowed_discard_forms_remove_the_partial_lane() {
    let mut pair = Pair::new(4);
    assert!(pair.send(Action::Paced, frame("peer-Ab123", 1, 1, false, &[1])));
    assert!(pair.send(Action::Gap, delta(2)));
    assert!(!pair.current.has_pending_paced());
    assert_eq!(pair.drain(), vec![gap("peer", 1, TRANSPORT, None)]);
    assert!(pair.send(Action::Paced, closing("peer", 1, 1, 1)));
    assert!(pair.drain().is_empty()); // Existing queued marker coalesces the later reason.
    for borrowed in [false, true] {
        assert!(pair.send(Action::Paced, frame("different-Ab123", 2, 3, true, &[1])));
        pair.discard(frame("different", 3, 3, false, &[]), borrowed);
        assert!(pair.current.has_pending_paced()); // Stream is part of the key.
        pair.discard(frame("different-Z9x8Q", 2, 3, false, &[]), borrowed);
        assert!(!pair.current.has_pending_paced());
    }
}

#[test]
fn chunk_limit_is_inclusive_and_empty_fragments_still_count() {
    for chunks in [2048, 2049] {
        let mut pair = Pair::new(4);
        for _ in 0..chunks {
            assert!(pair.send(Action::Paced, frame("peer", 1, 100, true, &[])));
        }
        if chunks == 2048 {
            assert!(pair.send(Action::Paced, closing("peer", 1, 100, chunks)));
            assert_eq!(
                pair.drain(),
                vec![Event::Frame(frame("peer", 1, 100, true, &[]))]
            );
        } else {
            assert_eq!(
                pair.drain(),
                vec![gap("peer", 1, "paced AU exceeded assembly bounds", None)]
            );
            assert!(!pair.current.has_pending_paced());
        }
    }
}

#[test]
fn byte_bound_is_inclusive_for_initial_and_timestamp_replacement() {
    for replacement in [false, true] {
        let mut pair = Pair::new(4);
        if replacement {
            assert!(pair.send(Action::Paced, frame("peer", 1, 99, true, &[1])));
        }
        assert!(pair.send(
            Action::Paced,
            sized_frame("peer", 1, 100, BYTE_LIMIT)
        ));
        assert!(pair.current.has_pending_paced());
        let expected = if replacement {
            vec![gap("peer", 1, MISSING, None)]
        } else {
            vec![]
        };
        assert_eq!(pair.drain(), expected);
        assert!(pair.send(Action::Paced, closing("peer", 1, 100, 1)));
        let delivered = pair.drain();
        let [Event::Frame(complete)] = delivered.as_slice() else {
            panic!("one exact-limit AU");
        };
        assert_eq!(complete.data.len(), BYTE_LIMIT);
        assert!(complete.data.iter().all(|byte| *byte == 9));
        assert!(!pair.current.has_pending_paced());
    }
}

#[test]
fn byte_bound_is_inclusive_for_cumulative_continuations() {
    for extra in [0, 1] {
        let mut pair = Pair::new(4);
        for _ in 0..2 {
            assert!(pair.send(
                Action::Paced,
                sized_frame("peer", 1, 100, BYTE_LIMIT / 2)
            ));
        }
        assert!(pair.send(Action::Paced, frame("peer", 1, 100, false, &[])));
        assert!(pair.current.has_pending_paced());
        assert!(pair.drain().is_empty()); // Equal bytes, including an empty append.
        if extra == 0 {
            assert!(pair.send(Action::Paced, closing("peer", 1, 100, 3)));
            let delivered = pair.drain();
            let [Event::Frame(complete)] = delivered.as_slice() else {
                panic!("one exact-limit assembled AU");
            };
            assert_eq!(complete.data.len(), BYTE_LIMIT);
            assert!(complete.data.iter().all(|byte| *byte == 9));
        } else {
            assert!(pair.send(Action::Paced, frame("peer", 1, 100, false, &[1])));
            assert_eq!(pair.drain(), vec![gap("peer", 1, BOUNDS, None)]);
            assert!(!pair.current.has_pending_paced());
            assert!(pair.send(Action::Paced, closing("peer", 1, 100, 4)));
            assert!(pair.drain().is_empty());
        }
        assert!(!pair.current.has_pending_paced());
    }
}

#[test]
fn oversized_continuation_discards_pending_and_preserves_historical_feedback() {
    let mut pair = Pair::new(4);
    assert!(pair.send(Action::Paced, frame("peer", 1, 100, true, &[1])));
    assert!(pair.send(
        Action::Paced,
        sized_frame("peer", 1, 100, BYTE_LIMIT + 1)
    ));
    assert!(!pair.current.has_pending_paced());
    assert_eq!(pair.drain(), vec![gap("peer", 1, BOUNDS, None)]);
    assert!(pair.send(Action::Paced, closing("peer", 1, 100, 2)));
    assert!(pair.drain().is_empty());
    let clean = frame("peer", 1, 101, true, &[7]);
    assert!(pair.send(Action::Paced, clean.clone()));
    assert!(pair.send(Action::Paced, closing("peer", 1, 101, 1)));
    assert_eq!(pair.drain(), vec![Event::Frame(clean)]);
}

#[test]
fn oversized_initial_and_replacement_intentionally_diverge_from_frozen_behavior() {
    for replacement in [false, true] {
        // Retain explicit evidence of the historical exception, including the
        // oversized emitted AU. Do not alter the frozen implementation to pass.
        {
            let mut old = original::State::default();
            let (tx, mut rx) = mpsc::channel(4);
            if replacement {
                assert!(old.send(
                    Action::Paced,
                    old_frame(frame("peer", 1, 99, true, &[1])),
                    &tx
                ));
            }
            assert!(old.send(
                Action::Paced,
                old_frame(sized_frame("peer", 1, 100, BYTE_LIMIT + 1)),
                &tx
            ));
            assert!(old.pending(), "historical unchecked fragment retained");
            if replacement {
                assert_eq!(
                    normalize(rx.try_recv().unwrap()),
                    gap("peer", 1, MISSING, None)
                );
            }
            assert!(rx.try_recv().is_err());
            assert!(old.send(Action::Paced, old_frame(closing("peer", 1, 100, 1)), &tx));
            let Event::Frame(complete) = normalize(rx.try_recv().unwrap()) else {
                panic!("historical unchecked first fragment was emitted");
            };
            assert_eq!(complete.data.len(), BYTE_LIMIT + 1);
            assert!(!old.pending());
            assert!(rx.try_recv().is_err());
        } // Release the old oversized buffer before allocating the new input.

        let mut current = Current::new(4);
        if replacement {
            assert!(current.send(Action::Paced, frame("peer", 1, 99, true, &[1])));
        }
        assert!(current.send(
            Action::Paced,
            sized_frame("peer", 1, 100, BYTE_LIMIT + 1)
        ));
        assert!(!current.state.has_pending_paced(), "rejected input retained");
        assert_eq!(current.drain(), vec![gap("peer", 1, BOUNDS, None)]);
        // Neither the old pending timestamp nor the rejected input may close.
        for stamp in [99, 100] {
            assert!(current.send(Action::Paced, closing("peer", 1, stamp, 1)));
            assert!(!current.state.has_pending_paced());
            assert!(current.drain().is_empty());
        }
    }
}

#[test]
fn oversized_rejection_requires_a_clean_reset_entry_before_paced_deltas_resume() {
    let mut current = Current::new(4);
    assert!(current.send(
        Action::Paced,
        sized_frame("peer", 1, 100, BYTE_LIMIT + 1)
    ));
    assert!(!current.state.has_pending_paced());
    assert_eq!(current.drain(), vec![gap("peer", 1, BOUNDS, None)]);
    assert!(current.send(Action::Paced, closing("peer", 1, 100, 1)));
    assert!(current.drain().is_empty());

    assert!(current.send(Action::Paced, delta(101)));
    assert!(current.send(Action::Paced, closing("peer", 1, 101, 1)));
    assert!(!current.state.has_pending_paced());
    assert!(current.drain().is_empty()); // A dependent delta cannot clear Reset.
    let clean = frame("peer", 1, 102, true, &[7]);
    assert!(current.send(Action::Paced, clean.clone()));
    assert!(current.send(Action::Paced, closing("peer", 1, 102, 1)));
    assert_eq!(current.drain(), vec![Event::Frame(clean)]);
    assert!(current.send(Action::Paced, delta(103)));
    assert!(current.send(Action::Paced, closing("peer", 1, 103, 1)));
    assert_eq!(current.drain(), vec![Event::Frame(delta(103))]);
    assert!(!current.state.has_pending_paced());
}

#[test]
fn oversized_rejection_preserves_remembered_gradual_recovery() {
    let mut current = Current::new(4);
    assert!(current.send(Action::Frame, gradual(0)));
    assert_eq!(current.drain(), vec![Event::Frame(gradual(0))]);
    assert!(current.send(
        Action::Paced,
        sized_frame("peer", 1, 100, BYTE_LIMIT + 1)
    ));
    assert!(!current.state.has_pending_paced());
    assert_eq!(current.drain(), vec![gap("peer", 1, BOUNDS, None)]);
    // Successful Gradual gap delivery clears the fence. The closing marker
    // still has no fragments, so its separate gap is preserved, never an AU.
    assert!(current.send(Action::Paced, closing("peer", 1, 100, 1)));
    assert_eq!(current.drain(), vec![gap("peer", 1, NO_FRAGMENTS, None)]);
    assert!(current.send(Action::Paced, delta(101)));
    assert!(current.send(Action::Paced, closing("peer", 1, 101, 1)));
    assert_eq!(current.drain(), vec![Event::Frame(delta(101))]);
    assert!(!current.state.has_pending_paced());
}

#[test]
fn oversized_rejection_with_full_sink_preserves_the_first_gap_until_admission() {
    for (gradual_mode, replacement) in [(false, false), (false, true), (true, false), (true, true)] {
        let mut current = Current::new(1);
        if gradual_mode {
            assert!(current.send(Action::Frame, gradual(0)));
            assert_eq!(current.drain(), vec![Event::Frame(gradual(0))]);
        }
        let blocker = frame("other", 1, 0, false, &[1]);
        assert!(current.send(Action::Frame, blocker.clone()));
        if replacement {
            assert!(current.send(Action::Paced, frame("peer", 1, 99, true, &[1])));
        }
        assert!(current.send(
            Action::Paced,
            sized_frame("peer", 1, 100, BYTE_LIMIT + 1)
        ));
        assert!(!current.state.has_pending_paced());
        assert_eq!(current.tx.capacity(), 0);
        for stamp in [99, 100] {
            assert!(current.send(Action::Paced, closing("peer", 1, stamp, 1)));
            assert!(!current.state.has_pending_paced());
        }
        // A clean entry also fails admission while full; it must not clear the
        // recovery fence or replace the original bounds reason.
        assert!(current.send(Action::Paced, frame("peer", 1, 101, true, &[7])));
        assert!(current.send(Action::Paced, closing("peer", 1, 101, 1)));
        assert!(!current.state.has_pending_paced());
        assert_eq!(current.drain(), vec![Event::Frame(blocker)]);

        assert!(current.send(Action::Paced, delta(102)));
        assert!(current.send(Action::Paced, closing("peer", 1, 102, 1)));
        let entry = gradual_mode.then(|| delta(102));
        assert_eq!(current.drain(), vec![gap("peer", 1, BOUNDS, entry)]);
        if !gradual_mode {
            assert!(current.send(Action::Paced, delta(103)));
            assert!(current.send(Action::Paced, closing("peer", 1, 103, 1)));
            assert!(current.drain().is_empty());
            let clean = frame("peer", 1, 104, true, &[7]);
            assert!(current.send(Action::Paced, clean.clone()));
            assert!(current.send(Action::Paced, closing("peer", 1, 104, 1)));
            assert_eq!(current.drain(), vec![Event::Frame(clean)]);
        }
        assert!(current.send(Action::Paced, delta(105)));
        assert!(current.send(Action::Paced, closing("peer", 1, 105, 1)));
        assert_eq!(current.drain(), vec![Event::Frame(delta(105))]);
        assert!(!current.state.has_pending_paced());
    }
}

#[test]
fn oversized_rejection_with_closed_sink_drops_pending_and_returns_false() {
    for replacement in [false, true] {
        let mut current = Current::new(1);
        if replacement {
            assert!(current.send(Action::Paced, frame("peer", 1, 99, true, &[1])));
        }
        current.rx.close();
        assert!(!current.send(
            Action::Paced,
            sized_frame("peer", 1, 100, BYTE_LIMIT + 1)
        ));
        assert!(!current.state.has_pending_paced());
        for stamp in [99, 100] {
            assert!(!current.send(Action::Paced, closing("peer", 1, stamp, 1)));
            assert!(!current.state.has_pending_paced());
        }
        assert!(current.send(Action::Paced, frame("peer", 1, 101, true, &[7])));
        assert!(!current.send(Action::Paced, closing("peer", 1, 101, 1)));
        assert!(!current.state.has_pending_paced());
        assert!(current.drain().is_empty());
    }
}

#[test]
fn oversized_replacement_only_discards_its_canonical_peer_and_lane() {
    let mut current = Current::new(4);
    let other_lane = frame("peer-Ab123", 2, 200, true, &[2]);
    let other_peer = frame("other", 1, 300, true, &[3]);
    assert!(current.send(Action::Paced, frame("peer-Ab123", 1, 99, true, &[1])));
    assert!(current.send(Action::Paced, other_lane.clone()));
    assert!(current.send(Action::Paced, other_peer.clone()));
    assert!(current.send(
        Action::Paced,
        sized_frame("peer-Z9x8Q", 1, 100, BYTE_LIMIT + 1)
    ));
    assert_eq!(current.drain(), vec![gap("peer-Z9x8Q", 1, BOUNDS, None)]);
    for stamp in [99, 100] {
        assert!(current.send(Action::Paced, closing("peer", 1, stamp, 1)));
        assert!(current.drain().is_empty());
    }
    assert!(current.state.has_pending_paced()); // Both independent lanes survive.
    assert!(current.send(Action::Paced, closing("peer-Xy987", 2, 200, 1)));
    assert_eq!(current.drain(), vec![Event::Frame(other_lane)]);
    assert!(current.state.has_pending_paced());
    assert!(current.send(Action::Paced, closing("other", 1, 300, 1)));
    assert_eq!(current.drain(), vec![Event::Frame(other_peer)]);
    assert!(!current.state.has_pending_paced());
    let clean = frame("peer", 1, 101, true, &[7]);
    assert!(current.send(Action::Paced, clean.clone()));
    assert!(current.send(Action::Paced, closing("peer", 1, 101, 1)));
    assert_eq!(current.drain(), vec![Event::Frame(clean)]);
    assert!(!current.state.has_pending_paced());
}

#[test]
fn age_check_is_on_data_arrival_while_an_old_matching_marker_still_closes() {
    let mut marker_case = Pair::new(4);
    let mut data_case = Pair::new(4);
    let first = frame("peer", 1, 100, true, &[1]);
    assert!(marker_case.send(Action::Paced, first.clone()));
    assert!(data_case.send(Action::Paced, first.clone()));
    // Manager-owned test execution waits once past the original one-second
    // threshold. This does not assert equal clocks or the exact equality edge.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    assert!(marker_case.send(Action::Paced, closing("peer", 1, 100, 1)));
    assert_eq!(marker_case.drain(), vec![Event::Frame(first)]);
    let next = frame("peer", 1, 100, true, &[2]);
    assert!(data_case.send(Action::Paced, next.clone()));
    assert_eq!(data_case.drain(), vec![gap("peer", 1, MISSING, None)]);
    assert!(data_case.send(Action::Paced, closing("peer", 1, 100, 1)));
    assert_eq!(data_case.drain(), vec![Event::Frame(next)]);
}
