//! Private state setup and observation only; shared by old and extracted code.
//! Native start paths are reachable only after asserting a duplicate map entry.

use super::super::{FeedState, MapState, StopObservation, WindowState};
use super::*;
use std::sync::mpsc::{self, Receiver};

pub(crate) struct Model {
    bridge: Arc<AudioBridge>,
}

impl Model {
    pub(crate) fn new() -> Self {
        Self {
            bridge: Arc::new(AudioBridge::new()),
        }
    }

    pub(crate) fn capture(&self, id: &str) {
        assert!(!self.bridge.captures.lock().contains_key(id));
        self.bridge.captures.lock().insert(
            id.into(),
            RouteAudio {
                stop: Arc::new(AtomicBool::new(false)),
                thread: None,
                playback: None,
            },
        );
    }

    pub(crate) fn playback(&self, id: &str, rate: u32, backed: bool) {
        assert!(!self.bridge.playbacks.lock().contains_key(id));
        self.bridge.playbacks.lock().insert(
            id.into(),
            RouteAudio {
                stop: Arc::new(AtomicBool::new(false)),
                thread: None,
                playback: backed.then(|| Playback {
                    ring: Arc::new(Mutex::new(VecDeque::new())),
                    out_rate: Arc::new(AtomicU32::new(rate)),
                    fed: Arc::new(AtomicU64::new(0)),
                    stats: Mutex::new(LevelStats::new()),
                }),
            },
        );
    }

    pub(crate) fn set_rate(&self, id: &str, rate: u32) {
        self.bridge.playbacks.lock()[id]
            .playback
            .as_ref()
            .unwrap()
            .out_rate
            .store(rate, Ordering::Relaxed);
    }

    pub(crate) fn set_ring(&self, id: &str, samples: &[i16]) {
        *self.bridge.playbacks.lock()[id]
            .playback
            .as_ref()
            .unwrap()
            .ring
            .lock() = samples.iter().copied().collect();
    }

    pub(crate) fn age_feed_stats(&self, id: &str) {
        self.bridge.playbacks.lock()[id]
            .playback
            .as_ref()
            .unwrap()
            .stats
            .lock()
            .since = Instant::now() - Duration::from_secs(6);
    }

    pub(crate) fn feed(&self, id: &str, frame: &AudioFrame) {
        self.bridge.feed(id, frame);
    }

    pub(crate) fn feed_state(&self, id: &str) -> FeedState {
        let map = self.bridge.playbacks.lock();
        let pb = map[id].playback.as_ref().unwrap();
        let stats = pb.stats.lock();
        let ring = pb.ring.lock().iter().copied().collect();
        FeedState {
            ring,
            rate: pb.out_rate.load(Ordering::Relaxed),
            fed: pb.fed.load(Ordering::Relaxed),
            frames: stats.frames,
            peak: stats.peak,
            warned: stats.warned_silent,
        }
    }

    pub(crate) fn maps(&self) -> MapState {
        let mut captures: Vec<_> = self.bridge.captures.lock().keys().cloned().collect();
        let mut playbacks: Vec<_> = self.bridge.playbacks.lock().keys().cloned().collect();
        captures.sort();
        playbacks.sort();
        MapState {
            captures,
            playbacks,
        }
    }

    pub(crate) fn running(&self, id: &str) -> bool {
        self.bridge.is_running(id)
    }
    pub(crate) fn stop(&self, id: &str) {
        self.bridge.stop(id);
    }
    pub(crate) fn stop_all(&self) {
        self.bridge.stop_all();
    }

    pub(crate) fn duplicate_starts(&self, id: &str) {
        // Required before touching these methods: neither native worker branch runs.
        assert!(self.bridge.captures.lock().contains_key(id));
        assert!(self.bridge.playbacks.lock().contains_key(id));
        self.bridge
            .start_capture(id.into(), CaptureSource::Mic, |_, _| {
                panic!("duplicate callback")
            });
        self.bridge.start_playback(id.into());
    }

    pub(crate) fn workers(
        &self,
        id: &str,
    ) -> (Receiver<StopObservation>, Receiver<StopObservation>) {
        assert!(!self.running(id));
        let capture_stop = Arc::new(AtomicBool::new(false));
        let playback_stop = Arc::new(AtomicBool::new(false));
        let mut receivers = Vec::new();
        for (capture, stop, other_stop) in [
            (true, capture_stop.clone(), playback_stop.clone()),
            (false, playback_stop, capture_stop),
        ] {
            let bridge = self.bridge.clone();
            let id_owned = id.to_owned();
            let thread_stop = stop.clone();
            let (tx, rx) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(5);
                while !thread_stop.load(Ordering::SeqCst) && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(1));
                }
                // Only try_lock: stop_all intentionally joins under its map lock.
                let captures = bridge.captures.try_lock();
                let playbacks = bridge.playbacks.try_lock();
                let event = StopObservation {
                    stopped: thread_stop.load(Ordering::SeqCst),
                    other_stopped: other_stop.load(Ordering::SeqCst),
                    capture_present: captures.as_ref().map(|map| map.contains_key(&id_owned)),
                    playback_present: playbacks.as_ref().map(|map| map.contains_key(&id_owned)),
                };
                let _ = tx.send(event);
            });
            let record = RouteAudio {
                stop,
                thread: Some(worker),
                playback: None,
            };
            let displaced = if capture {
                self.bridge.captures.lock().insert(id.into(), record)
            } else {
                self.bridge.playbacks.lock().insert(id.into(), record)
            };
            assert!(displaced.is_none());
            receivers.push(rx);
        }
        (receivers.remove(0), receivers.remove(0))
    }
}

impl Drop for Model {
    fn drop(&mut self) {
        self.bridge.stop_all();
    }
}

pub(crate) fn route_drop_with_panicking_worker() -> bool {
    let stop = Arc::new(AtomicBool::new(false));
    let worker = std::thread::spawn(|| panic!("test-owned worker panic"));
    let record = RouteAudio {
        stop: stop.clone(),
        thread: Some(worker),
        playback: None,
    };
    drop(record);
    stop.load(Ordering::SeqCst)
}

pub(crate) struct Window(LevelStats);
impl Window {
    pub(crate) fn new() -> Self {
        Self(LevelStats::new())
    }
    pub(crate) fn age(&mut self) {
        self.0.since = Instant::now() - Duration::from_secs(6);
    }
    pub(crate) fn warned(&mut self) {
        self.0.warned_silent = true;
    }
    pub(crate) fn note(&mut self, pcm: &[i16]) -> WindowState {
        let mut lines = Vec::new();
        let silent = self.0.note(pcm, |line| lines.push(line));
        WindowState {
            silent,
            lines,
            frames: self.0.frames,
            peak: self.0.peak,
            warned: self.0.warned_silent,
        }
    }
}

pub(crate) fn output_i16(input: &[i16], channels: usize, len: usize) -> (Vec<i16>, Vec<i16>) {
    let ring = Mutex::new(input.iter().copied().collect());
    let mut data = vec![12345; len];
    observed_fill(&ring, channels, &mut data, |s| s);
    (data, ring.into_inner().into_iter().collect())
}

pub(crate) fn output_f32(input: &[i16], channels: usize, len: usize) -> (Vec<f32>, Vec<i16>) {
    let ring = Mutex::new(input.iter().copied().collect());
    let mut data = vec![12345.0; len];
    // Original F32 conversion; independently covered by literal PCM vectors.
    observed_fill(&ring, channels, &mut data, |s| s as f32 / 32768.0);
    (data, ring.into_inner().into_iter().collect())
}

pub(crate) fn output_u16(input: &[i16], channels: usize, len: usize) -> (Vec<u16>, Vec<i16>) {
    let ring = Mutex::new(input.iter().copied().collect());
    let mut data = vec![12345; len];
    observed_fill(&ring, channels, &mut data, |s: i16| {
        (s as i32 + 32768) as u16
    });
    (data, ring.into_inner().into_iter().collect())
}

pub(crate) fn metered_forward(system: bool) -> Vec<(Vec<i16>, u32)> {
    let frames = Arc::new(Mutex::new(Vec::new()));
    let seen = frames.clone();
    let source = if system {
        CaptureSource::System
    } else {
        CaptureSource::Mic
    };
    let callback = metered("r\0test", source, move |pcm, rate| {
        seen.lock().push((pcm, rate))
    });
    callback(vec![i16::MIN, 0, i16::MAX], 0);
    callback(Vec::new(), 44100);
    callback(vec![7, -8], 96000);
    let result = frames.lock().clone();
    result
}

pub(crate) fn metered_callbacks_can_overlap() -> bool {
    let (began_tx, began_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let success = Arc::new(AtomicBool::new(false));
    let seen = success.clone();
    let callback = Arc::new(metered("r", CaptureSource::Mic, move |pcm, _| {
        if pcm == [1] {
            let _ = began_tx.send(());
            seen.store(
                release_rx
                    .lock()
                    .recv_timeout(Duration::from_secs(5))
                    .is_ok(),
                Ordering::SeqCst,
            );
        } else {
            let _ = release_tx.send(());
        }
    }));
    let first = callback.clone();
    let first_thread = std::thread::spawn(move || first(vec![1], 48000));
    let began = began_rx.recv_timeout(Duration::from_secs(5)).is_ok();
    let second_thread = std::thread::spawn(move || callback(vec![2], 48000));
    first_thread.join().unwrap();
    second_thread.join().unwrap();
    began && success.load(Ordering::SeqCst)
}

pub(crate) fn log_line() {
    stats_log("fixture statistics".into());
}
