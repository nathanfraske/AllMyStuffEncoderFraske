//! Hardware-free bridge, fake-worker lifecycle and native callback-body checks.
//! Never discover devices, open CPAL streams, load Pulse, or start native workers.

use crate::StatsPolicy;
use allmystuff_session::AudioFrame;
use std::cell::Cell;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::Receiver;
use std::time::Duration;

#[allow(dead_code)]
#[rustfmt::skip]
#[path = "frozen_io.rs"]
mod original;

#[path = "extracted_io.rs"]
mod extracted;

thread_local! {
    static POLICY: Cell<(bool, usize)> = const { Cell::new((false, 0)) };
}

struct TestStats;
impl StatsPolicy for TestStats {
    fn stats_to_info() -> bool {
        POLICY.with(|p| {
            let (value, calls) = p.get();
            p.set((value, calls + 1));
            value
        })
    }
}

fn oracle_stats_to_info() -> bool { TestStats::stats_to_info() }

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct FeedState {
    ring: Vec<i16>, rate: u32, fed: u64, frames: u32, peak: i16, warned: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MapState { captures: Vec<String>, playbacks: Vec<String> }

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct WindowState { silent: bool, lines: Vec<String>, frames: u32, peak: i16, warned: bool }

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct StopObservation {
    stopped: bool,
    other_stopped: bool,
    // None means the map was locked during join; Some is the observed membership.
    capture_present: Option<bool>,
    playback_present: Option<bool>,
}

struct Pair { old: original::harness::Model, new: extracted::harness::Model }
impl Pair {
    fn new() -> Self { Self { old: original::harness::Model::new(), new: extracted::harness::Model::new() } }
    fn playback(&self, id: &str, rate: u32, backed: bool) {
        self.old.playback(id, rate, backed); self.new.playback(id, rate, backed);
    }
    fn capture(&self, id: &str) { self.old.capture(id); self.new.capture(id); }
    fn feed(&self, id: &str, frame: &AudioFrame) { self.old.feed(id, frame); self.new.feed(id, frame); }
    fn state(&self, id: &str) -> FeedState {
        let old = self.old.feed_state(id); let new = self.new.feed_state(id);
        assert_eq!(new, old); new
    }
}

#[test]
fn feed_missing_or_unbacked_routes_preserves_maps_and_does_nothing() {
    let pair = Pair::new();
    pair.capture("capture"); pair.playback("unbacked", 48000, false);
    let frame = AudioFrame::new("capture", 3, 48000, 1, vec![9]);
    for id in ["", "missing", "capture", "unbacked"] { pair.feed(id, &frame); }
    assert_eq!(pair.new.maps(), pair.old.maps());
    assert_eq!(pair.new.maps(), MapState { captures: vec!["capture".into()], playbacks: vec!["unbacked".into()] });
}

#[test]
fn feed_uses_argument_route_and_counts_empty_frames_before_processing() {
    let pair = Pair::new(); pair.playback("r", 48000, true);
    pair.feed("r", &AudioFrame::new("different", u64::MAX, 0, 0, vec![3, -4]));
    pair.feed("r", &AudioFrame::new("", 0, 48000, 2, Vec::new()));
    assert_eq!(pair.state("r"), FeedState {
        ring: vec![3, -4], rate: 48000, fed: 2, frames: 2, peak: 4, warned: false,
    });
}

#[test]
fn feed_observes_device_rate_changes_and_original_downmix_resample_order() {
    let pair = Pair::new(); pair.playback("r", 48000, true);
    pair.feed("r", &AudioFrame::new("r", 0, 24000, 2, vec![0, 20, 20, 40, 50]));
    assert_eq!(pair.state("r").ring, [10, 20, 30, 40, 50, 50]);
    pair.old.set_rate("r", 24000); pair.new.set_rate("r", 24000);
    pair.feed("r", &AudioFrame::new("r", 0, 48000, 1, vec![100, 200, 300, 400]));
    pair.feed("r", &AudioFrame::new("r", 0, 0, 1, vec![500, 600]));
    assert_eq!(pair.state("r").ring, [10, 20, 30, 40, 50, 50, 100, 300, 500, 600]);
}

#[test]
fn feed_keeps_strict_trim_threshold_and_can_trim_on_empty_input() {
    let pair = Pair::new(); pair.playback("r", 48000, true);
    let initial: Vec<i16> = (0..9600).collect();
    pair.feed("r", &AudioFrame::new("r", 0, 48000, 1, initial));
    assert_eq!(pair.state("r").ring.len(), 9600);
    pair.feed("r", &AudioFrame::new("r", 1, 48000, 1, vec![9600]));
    let state = pair.state("r");
    assert_eq!(state.ring.len(), 3840); assert_eq!(state.ring[0], 5761);
    pair.old.set_ring("r", &[1, 2]); pair.new.set_ring("r", &[1, 2]);
    pair.old.set_rate("r", 999); pair.new.set_rate("r", 999);
    pair.feed("r", &AudioFrame::new("r", 2, 0, 1, Vec::new()));
    assert!(pair.state("r").ring.is_empty());
}

#[test]
fn duplicate_start_guards_keep_seeded_capture_and_playback_resources() {
    let pair = Pair::new(); pair.capture("r"); pair.playback("r", 44100, true);
    pair.feed("r", &AudioFrame::new("r", 0, 0, 1, vec![7]));
    let before = pair.state("r");
    pair.old.duplicate_starts("r"); pair.new.duplicate_starts("r");
    assert_eq!(pair.state("r"), before);
    assert_eq!(pair.new.maps(), pair.old.maps());
    assert_eq!(pair.new.maps(), MapState { captures: vec!["r".into()], playbacks: vec!["r".into()] });
}

#[test]
fn running_is_map_presence_even_without_a_live_worker() {
    let pair = Pair::new(); pair.capture("capture"); pair.playback("playback", 0, false);
    for id in ["capture", "playback"] {
        assert!(pair.old.running(id)); assert!(pair.new.running(id));
    }
    assert!(!pair.old.running("missing")); assert!(!pair.new.running("missing"));
    pair.old.stop("capture"); pair.new.stop("capture");
    assert!(!pair.old.running("capture")); assert!(!pair.new.running("capture"));
    assert!(pair.old.running("playback")); assert!(pair.new.running("playback"));
}

fn stopped(rx: Receiver<StopObservation>) -> StopObservation {
    let state = rx.recv_timeout(Duration::from_secs(6)).expect("fake worker observation");
    assert!(state.stopped, "fake worker reached its deadline without the stop signal");
    state
}

#[test]
fn stop_removes_both_maps_then_joins_capture_before_playback_off_lock() {
    let pair = Pair::new();
    let (old_capture, old_playback) = pair.old.workers("r");
    let (new_capture, new_playback) = pair.new.workers("r");
    pair.old.stop("r"); pair.new.stop("r");
    let old = [stopped(old_capture), stopped(old_playback)];
    let new = [stopped(new_capture), stopped(new_playback)];
    assert_eq!(new, old);
    assert_eq!(new, [
        StopObservation { stopped: true, other_stopped: false, capture_present: Some(false), playback_present: Some(false) },
        StopObservation { stopped: true, other_stopped: true, capture_present: Some(false), playback_present: Some(false) },
    ]);
    assert_eq!(pair.new.maps(), MapState { captures: vec![], playbacks: vec![] });
}

#[test]
fn stop_all_preserves_capture_then_playback_joins_under_their_map_locks() {
    let pair = Pair::new();
    let (old_capture, old_playback) = pair.old.workers("r");
    let (new_capture, new_playback) = pair.new.workers("r");
    pair.old.stop_all(); pair.new.stop_all();
    let old = [stopped(old_capture), stopped(old_playback)];
    let new = [stopped(new_capture), stopped(new_playback)];
    assert_eq!(new, old);
    assert_eq!(new, [
        StopObservation { stopped: true, other_stopped: false, capture_present: None, playback_present: Some(true) },
        StopObservation { stopped: true, other_stopped: true, capture_present: Some(false), playback_present: None },
    ]);
}

#[test]
fn route_drop_sets_stop_and_ignores_the_joined_workers_panic() {
    assert!(original::harness::route_drop_with_panicking_worker());
    assert!(extracted::harness::route_drop_with_panicking_worker());
}

#[test]
fn level_window_saturates_the_signed_minimum_and_resets_only_window_fields() {
    let mut old = original::harness::Window::new();
    let mut new = extracted::harness::Window::new();
    old.warned(); new.warned();
    let first = new.note(&[i16::MIN, 2]); assert_eq!(first, old.note(&[i16::MIN, 2]));
    assert_eq!(first, WindowState { silent: false, lines: vec![], frames: 1, peak: 32767, warned: true });
    old.age(); new.age();
    let next = new.note(&[]); assert_eq!(next, old.note(&[]));
    assert_eq!(next, WindowState { silent: false, lines: vec!["0 buffers/s · peak 100%".into()], frames: 0, peak: 0, warned: true });
}

#[test]
fn level_window_counts_empty_buffers_and_reports_silence_without_marking_warning() {
    let mut old = original::harness::Window::new();
    let mut new = extracted::harness::Window::new();
    assert_eq!(new.note(&[]), old.note(&[]));
    old.age(); new.age();
    let state = new.note(&[0]); assert_eq!(state, old.note(&[0]));
    assert_eq!(state, WindowState { silent: true, lines: vec!["0 buffers/s · peak 0%".into()], frames: 0, peak: 0, warned: false });
}

#[test]
fn output_callback_pops_once_per_frame_including_partial_interleaved_tail() {
    let input = [10, -20, 30, 40];
    for channels in [1, 2, 3, 8] {
        for len in [0, 1, 2, 5, 9, 40] {
            assert_eq!(extracted::harness::output_i16(&input, channels, len), original::harness::output_i16(&input, channels, len));
        }
    }
    assert_eq!(extracted::harness::output_i16(&input, 2, 5), (vec![10, 10, -20, -20, 30], vec![40]));
}

#[test]
fn output_callback_preserves_float_unsigned_and_underrun_silence_values() {
    let input = [i16::MIN, 0, i16::MAX];
    assert_eq!(extracted::harness::output_f32(&input, 2, 8), original::harness::output_f32(&input, 2, 8));
    assert_eq!(extracted::harness::output_u16(&input, 2, 8), original::harness::output_u16(&input, 2, 8));
    assert_eq!(extracted::harness::output_f32(&input, 2, 8).0, [-1.0, -1.0, 0.0, 0.0, 32767.0/32768.0, 32767.0/32768.0, 0.0, 0.0]);
    assert_eq!(extracted::harness::output_u16(&input, 2, 8).0, [0, 0, 32768, 32768, 65535, 65535, 32768, 32768]);
}

#[test]
fn output_callback_preserves_zero_channel_panic_even_for_empty_output() {
    for len in [0, 1] {
        assert!(catch_unwind(AssertUnwindSafe(|| original::harness::output_i16(&[1], 0, len))).is_err());
        assert!(catch_unwind(AssertUnwindSafe(|| extracted::harness::output_i16(&[1], 0, len))).is_err());
    }
}

#[test]
fn metered_capture_forwards_exact_samples_rates_and_empty_buffers() {
    for system in [false, true] {
        let new = extracted::harness::metered_forward(system);
        assert_eq!(new, original::harness::metered_forward(system));
        assert_eq!(new, vec![(vec![i16::MIN, 0, i16::MAX], 0), (vec![], 44100), (vec![7, -8], 96000)]);
    }
}

#[test]
fn metered_capture_releases_statistics_lock_before_user_callback() {
    assert!(original::harness::metered_callbacks_can_overlap());
    assert!(extracted::harness::metered_callbacks_can_overlap());
}

#[test]
fn statistics_policy_is_lazy_at_the_original_emission_points() {
    POLICY.with(|p| p.set((true, 0)));
    let pair = Pair::new(); pair.playback("r", 48000, true); pair.capture("r");
    pair.old.duplicate_starts("r"); pair.new.duplicate_starts("r");
    pair.feed("r", &AudioFrame::new("r", 0, 48000, 1, vec![1]));
    POLICY.with(|p| assert_eq!(p.get(), (true, 0)));
    pair.old.age_feed_stats("r"); pair.new.age_feed_stats("r");
    pair.feed("r", &AudioFrame::new("r", 1, 48000, 1, vec![2]));
    POLICY.with(|p| assert_eq!(p.get(), (true, 2)));
    original::harness::log_line(); extracted::harness::log_line();
    POLICY.with(|p| assert_eq!(p.get(), (true, 4)));
}

#[test]
fn static_statistics_policy_does_not_add_owned_send_sync_or_default_bounds() {
    struct Marker { _not_send: std::rc::Rc<()> }
    impl StatsPolicy for Marker { fn stats_to_info() -> bool { false } }
    fn send_sync<T: Send + Sync>(_: &T) {}
    let bridge = super::AudioBridge::<Marker>::default();
    send_sync(&bridge);
    assert!(!bridge.is_running("r"));
    assert!(!crate::DebugStats::stats_to_info());
}
