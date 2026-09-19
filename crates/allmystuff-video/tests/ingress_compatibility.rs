//! Frozen node ingress decisions compared through real bounded local channels.
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
        assert_eq!(actual, expected, "delivered event order and exact envelopes");
        actual
    }

    fn close(&mut self) {
        self.old_rx.close();
        self.new_rx.close();
    }

    fn discard(&mut self, frame: Frame<u8>, borrowed_lane: bool) {
        self.original.discard(&old_frame(frame.clone()));
        if borrowed_lane {
            self.current.discard_paced_peer_lane(&frame.from, frame.stream);
        } else {
            self.current.discard_paced_lane(&frame);
        }
        assert_eq!(self.current.has_pending_paced(), self.original.pending());
    }
}

const OVERFLOW: &str = "AMS complete-AU ingress queue overflow";
const TRANSPORT: &str = "MyOwnMesh reported media discontinuity (transport or daemon IPC)";
const MISSING: &str = "paced AU expired or missing end marker";

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
    assert_eq!(pair.drain(), vec![gap("peer", 1, OVERFLOW, Some(gradual(3)))]);
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
    assert_eq!(pair.drain(), vec![gap("peer", 1, TRANSPORT, Some(gradual(1)))]);
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
    assert_eq!(pair.drain(), vec![gap("peer", 1, MISSING, Some(replacement))]);
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
fn byte_bound_applies_to_append_but_first_fragment_remains_unchecked() {
    const LIMIT: usize = 16 * 1024 * 1024;
    for extra in [0, 1] {
        let mut pair = Pair::new(4);
        assert!(pair.send(
            Action::Paced,
            frame("peer", 1, 100, true, &vec![9; LIMIT + extra])
        ));
        assert!(pair.send(Action::Paced, closing("peer", 1, 100, 1)));
        let delivered = pair.drain();
        let [Event::Frame(complete)] = delivered.as_slice() else {
            panic!("complete first fragment");
        };
        assert_eq!(complete.data.len(), LIMIT + extra);
    }
    let mut pair = Pair::new(4);
    assert!(pair.send(Action::Paced, frame("peer", 1, 100, true, &vec![9; LIMIT])));
    assert!(pair.send(Action::Paced, frame("peer", 1, 100, false, &[])));
    assert!(pair.drain().is_empty()); // Equal total bytes remains accepted.
    assert!(pair.send(Action::Paced, frame("peer", 1, 100, false, &[1])));
    assert_eq!(
        pair.drain(),
        vec![gap("peer", 1, "paced AU exceeded assembly bounds", None)]
    );
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
