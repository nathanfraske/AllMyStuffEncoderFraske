//! The audio media plane: capture from a mic and play to speakers with
//! `cpal`, so an active audio route actually moves sound across the mesh.
//!
//! cpal's `Stream` is `!Send`, so each route runs on its own dedicated
//! thread that builds the stream, keeps it alive, and drops it on stop. The
//! mesh layer never touches a stream directly — it calls [`AudioBridge`]:
//!
//!  * **source side** — [`AudioBridge::start_capture`] grabs the default
//!    input, down-mixes to mono i16, and hands each buffer to a callback the
//!    mesh forwards as an [`AudioFrame`].
//!  * **sink side** — [`AudioBridge::start_playback`] opens the default
//!    output and drains a ring buffer that [`AudioBridge::feed`] fills from
//!    inbound frames (linear-resampled to the device rate).
//!
//! Because "routes active, no sound" is invisible from the route handshake,
//! both ends are deliberately loud about the things that go silently wrong
//! in the field: each stream logs **which device** it opened (the default
//! device may not be the one the user is thinking of), a capture that
//! produces nothing but zeros for its first seconds names the OS
//! microphone permission (macOS TCC and Windows privacy both deliver
//! silence rather than an error), and a playback that never gets fed says
//! so once instead of playing silence forever. The flowing-state counters
//! (`audio out/in` lines) ride the same `ALLMYSTUFF_VIDEO_STATS` dial-in
//! switch the video pipeline uses, at debug otherwise.
//!
//! v1 simplifications, called out honestly: it uses the *default* input /
//! output device (mapping a specific scanned device to a cpal device by
//! name is a follow-up), and transports mono. Both are noted in the README.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;

use allmystuff_session::AudioFrame;

/// One running route's audio resources (a capture or a playback thread).
struct RouteAudio {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// Present for playback routes: the ring the output stream drains and
    /// the device's output sample rate that `feed` resamples to.
    playback: Option<Playback>,
}

struct Playback {
    ring: Arc<Mutex<VecDeque<i16>>>,
    out_rate: Arc<AtomicU32>,
    /// Frames `feed` has accepted — the playback thread warns once when
    /// this is still zero seconds after the route went live.
    fed: Arc<AtomicU64>,
    /// Receive-side dial-in counters.
    stats: Mutex<LevelStats>,
}

impl Drop for RouteAudio {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Windowed frames/peak counters shared by the capture and feed sides —
/// the audio equivalent of the video pipeline's dial-in lines.
struct LevelStats {
    since: Instant,
    frames: u32,
    peak: i16,
    /// Whether the all-zeros warning fired (once per stream).
    warned_silent: bool,
}

impl LevelStats {
    fn new() -> Self {
        LevelStats {
            since: Instant::now(),
            frames: 0,
            peak: 0,
            warned_silent: false,
        }
    }

    /// Count one buffer; every ~5 s emit the dial-in line via `log` and
    /// return whether the window was pure silence (for the caller's
    /// one-shot permission warning).
    fn note(&mut self, pcm: &[i16], log: impl FnOnce(String)) -> bool {
        const EVERY: Duration = Duration::from_secs(5);
        self.frames += 1;
        let peak = pcm.iter().map(|s| s.saturating_abs()).max().unwrap_or(0);
        self.peak = self.peak.max(peak);
        if self.since.elapsed() < EVERY {
            return false;
        }
        let secs = self.since.elapsed().as_secs_f64();
        log(format!(
            "{:.0} buffers/s · peak {:.0}%",
            self.frames as f64 / secs,
            self.peak as f64 * 100.0 / i16::MAX as f64,
        ));
        let silent = self.peak == 0;
        self.since = Instant::now();
        self.frames = 0;
        self.peak = 0;
        silent
    }
}

#[derive(Default)]
pub struct AudioBridge {
    // Capture and playback live in separate maps so a loopback route (this
    // machine's mic to its own speakers) can run both under one route id —
    // one map made the second insert silently stop the first's thread.
    captures: Mutex<HashMap<String, RouteAudio>>,
    playbacks: Mutex<HashMap<String, RouteAudio>>,
}

impl AudioBridge {
    pub fn new() -> Self {
        Self::default()
    }

    /// Begin capturing the default input for `route_id`. `on_frame(mono_pcm,
    /// sample_rate)` is called for every captured buffer; the mesh wraps it
    /// into an [`AudioFrame`] and sends it to the peer.
    pub fn start_capture<F>(&self, route_id: String, on_frame: F)
    where
        F: Fn(Vec<i16>, u32) + Send + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let id = route_id.clone();
        let thread = std::thread::spawn(move || {
            if let Err(e) = run_capture(&stop_thread, &id, on_frame) {
                tracing::warn!("audio capture for {id} stopped: {e}");
            }
        });
        self.captures.lock().insert(
            route_id,
            RouteAudio {
                stop,
                thread: Some(thread),
                playback: None,
            },
        );
    }

    /// Begin playing inbound audio for `route_id` on the default output.
    pub fn start_playback(&self, route_id: String) {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let ring = Arc::new(Mutex::new(VecDeque::<i16>::new()));
        let ring_thread = ring.clone();
        // 48 kHz is the near-universal default; the thread overwrites it
        // with the real device rate once the stream is built.
        let out_rate = Arc::new(AtomicU32::new(48_000));
        let out_rate_thread = out_rate.clone();
        let fed = Arc::new(AtomicU64::new(0));
        let fed_thread = fed.clone();
        let id = route_id.clone();
        let thread = std::thread::spawn(move || {
            if let Err(e) =
                run_playback(&stop_thread, &id, ring_thread, out_rate_thread, fed_thread)
            {
                tracing::warn!("audio playback for {id} stopped: {e}");
            }
        });
        self.playbacks.lock().insert(
            route_id,
            RouteAudio {
                stop,
                thread: Some(thread),
                playback: Some(Playback {
                    ring,
                    out_rate,
                    fed,
                    stats: Mutex::new(LevelStats::new()),
                }),
            },
        );
    }

    /// Push an inbound frame into a playback route's ring (mono, resampled
    /// to the device rate). No-op if the route isn't a playback route.
    pub fn feed(&self, route_id: &str, frame: &AudioFrame) {
        let routes = self.playbacks.lock();
        let Some(pb) = routes.get(route_id).and_then(|r| r.playback.as_ref()) else {
            return;
        };
        if pb.fed.fetch_add(1, Ordering::Relaxed) == 0 {
            tracing::info!(
                "first audio frame for {route_id} ({} Hz, {} ch)",
                frame.sample_rate,
                frame.channels
            );
        }
        let mono = downmix(&frame.pcm, frame.channels);
        {
            let mut stats = pb.stats.lock();
            let id = route_id.to_string();
            stats.note(&mono, move |line| {
                stats_log(format!("audio in {id}: {line}"))
            });
        }
        let out_rate = pb.out_rate.load(Ordering::Relaxed);
        let samples = if frame.sample_rate == out_rate || frame.sample_rate == 0 {
            mono
        } else {
            resample_linear(&mono, frame.sample_rate, out_rate)
        };
        let mut ring = pb.ring.lock();
        ring.extend(samples);
        // Bound latency to ~1s; drop the oldest if we fall behind.
        let cap = out_rate as usize;
        while ring.len() > cap {
            ring.pop_front();
        }
    }

    pub fn stop(&self, route_id: &str) {
        // Drop runs the thread join + stream teardown.
        self.captures.lock().remove(route_id);
        self.playbacks.lock().remove(route_id);
    }

    #[allow(dead_code)]
    pub fn stop_all(&self) {
        self.captures.lock().clear();
        self.playbacks.lock().clear();
    }

    #[allow(dead_code)]
    pub fn is_running(&self, route_id: &str) -> bool {
        self.captures.lock().contains_key(route_id) || self.playbacks.lock().contains_key(route_id)
    }
}

/// Route a dial-in line through the same switch as the video pipeline's:
/// info while `ALLMYSTUFF_VIDEO_STATS` is set, debug otherwise.
fn stats_log(line: String) {
    if crate::video::stats_to_info() {
        tracing::info!("{line}");
    } else {
        tracing::debug!("{line}");
    }
}

// ---- capture ----------------------------------------------------------

fn run_capture<F>(stop: &AtomicBool, route_id: &str, on_frame: F) -> Result<(), String>
where
    F: Fn(Vec<i16>, u32) + Send + 'static,
{
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| "no default input device".to_string())?;
    let supported = device.default_input_config().map_err(|e| e.to_string())?;
    let sample_rate = supported.sample_rate().0;
    let channels = supported.channels();
    let config: cpal::StreamConfig = supported.config();
    let fmt = supported.sample_format();
    // Name the device: "no sound" is regularly "the default input isn't
    // the mic you think it is".
    tracing::info!(
        "audio capture for {route_id}: device \"{}\", {sample_rate} Hz, {channels} ch, {fmt:?}",
        device.name().unwrap_or_else(|_| "unknown".into()),
    );
    let err = |e| tracing::warn!("input stream error: {e}");

    // Wrap the consumer with the level meter. A capture whose first
    // five-second window is *pure zeros* is almost always the OS denying
    // the microphone (macOS TCC / Windows privacy deliver silence, not an
    // error) — say so once, loudly, instead of streaming silence.
    let on_frame = {
        let stats = Mutex::new(LevelStats::new());
        let id = route_id.to_string();
        move |pcm: Vec<i16>, rate: u32| {
            {
                let mut stats = stats.lock();
                let line_id = id.clone();
                let silent = stats.note(&pcm, move |line| {
                    stats_log(format!("audio out {line_id}: {line}"));
                });
                if silent && !stats.warned_silent {
                    stats.warned_silent = true;
                    tracing::warn!(
                        "audio capture for {id} is producing pure silence — check the OS \
                         microphone permission for this app (and the default input device)"
                    );
                }
            }
            on_frame(pcm, rate);
        }
    };

    // `on_frame` is moved into whichever arm runs (match arms are mutually
    // exclusive, so a single move per arm is fine). `Fn` is callable
    // directly — no `Arc` wrapper.
    let stream = match fmt {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config,
            move |data: &[f32], _: &_| {
                let pcm: Vec<i16> = downmix(
                    &data.iter().map(|&f| f32_to_i16(f)).collect::<Vec<_>>(),
                    channels,
                );
                on_frame(pcm, sample_rate);
            },
            err,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config,
            move |data: &[i16], _: &_| on_frame(downmix(data, channels), sample_rate),
            err,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_input_stream(
            &config,
            move |data: &[u16], _: &_| {
                let pcm: Vec<i16> = downmix(
                    &data
                        .iter()
                        .map(|&u| (u as i32 - 32768) as i16)
                        .collect::<Vec<_>>(),
                    channels,
                );
                on_frame(pcm, sample_rate);
            },
            err,
            None,
        ),
        other => return Err(format!("unsupported input sample format: {other:?}")),
    }
    .map_err(|e| e.to_string())?;

    stream.play().map_err(|e| e.to_string())?;
    park_until_stopped(stop);
    Ok(())
}

// ---- playback ---------------------------------------------------------

fn run_playback(
    stop: &AtomicBool,
    route_id: &str,
    ring: Arc<Mutex<VecDeque<i16>>>,
    out_rate: Arc<AtomicU32>,
    fed: Arc<AtomicU64>,
) -> Result<(), String> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| "no default output device".to_string())?;
    let supported = device.default_output_config().map_err(|e| e.to_string())?;
    out_rate.store(supported.sample_rate().0, Ordering::Relaxed);
    let channels = supported.channels() as usize;
    let config: cpal::StreamConfig = supported.config();
    let fmt = supported.sample_format();
    tracing::info!(
        "audio playback for {route_id}: device \"{}\", {} Hz, {channels} ch, {fmt:?}",
        device.name().unwrap_or_else(|_| "unknown".into()),
        supported.sample_rate().0,
    );
    let err = |e| tracing::warn!("output stream error: {e}");

    // Each output frame is `channels` interleaved samples; we hold mono in
    // the ring and write the same sample to every channel.
    macro_rules! fill {
        ($data:expr, $conv:expr) => {{
            let mut guard = ring.lock();
            for frame in $data.chunks_mut(channels) {
                let s = guard.pop_front().unwrap_or(0);
                for slot in frame.iter_mut() {
                    *slot = $conv(s);
                }
            }
        }};
    }

    let stream = match fmt {
        cpal::SampleFormat::F32 => device.build_output_stream(
            &config,
            move |data: &mut [f32], _: &_| fill!(data, i16_to_f32),
            err,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_output_stream(
            &config,
            move |data: &mut [i16], _: &_| fill!(data, |s| s),
            err,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_output_stream(
            &config,
            move |data: &mut [u16], _: &_| fill!(data, |s: i16| (s as i32 + 32768) as u16),
            err,
            None,
        ),
        other => return Err(format!("unsupported output sample format: {other:?}")),
    }
    .map_err(|e| e.to_string())?;

    stream.play().map_err(|e| e.to_string())?;
    // The route is live and the speaker is waiting: if nothing has been
    // fed after a few seconds the problem is upstream (peer capturing
    // silence, frames not arriving) — name it once, so "no sound" is
    // attributable from this side's log alone.
    let mut starve_check = Some(Instant::now());
    while !stop.load(Ordering::SeqCst) {
        if let Some(started) = starve_check {
            if started.elapsed() >= Duration::from_secs(5) {
                starve_check = None;
                if fed.load(Ordering::Relaxed) == 0 {
                    tracing::warn!(
                        "audio playback for {route_id} has received no frames after 5s — \
                         the sending side is likely capturing silence or not capturing at all"
                    );
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn park_until_stopped(stop: &AtomicBool) {
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(100));
    }
}

// ---- sample helpers (pure) -------------------------------------------

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
fn resample_linear(samples: &[i16], from_rate: u32, to_rate: u32) -> Vec<i16> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmix_stereo_to_mono_averages() {
        assert_eq!(downmix(&[10, 20, 30, 40], 2), vec![15, 35]);
        assert_eq!(downmix(&[1, 2, 3], 1), vec![1, 2, 3]);
    }

    #[test]
    fn resample_upsamples_length() {
        let up = resample_linear(&[0, 100, 200, 300], 24_000, 48_000);
        assert_eq!(up.len(), 8);
        assert_eq!(up[0], 0);
        // Same rate is a passthrough.
        assert_eq!(resample_linear(&[1, 2, 3], 48_000, 48_000), vec![1, 2, 3]);
    }

    #[test]
    fn sample_conversions_round_trip_near_unity() {
        assert_eq!(f32_to_i16(0.0), 0);
        assert!(f32_to_i16(1.0) >= 32760);
        assert!((i16_to_f32(32767) - 1.0).abs() < 0.01);
    }

    #[test]
    fn level_stats_flag_a_pure_silence_window() {
        let mut stats = LevelStats::new();
        assert!(!stats.note(&[0, 0, 0], |_| {}), "window not elapsed yet");
        stats.since = Instant::now() - Duration::from_secs(6);
        assert!(stats.note(&[0, 0, 0], |_| {}), "all-zeros window is silent");

        let mut loud = LevelStats::new();
        loud.since = Instant::now() - Duration::from_secs(6);
        assert!(!loud.note(&[0, 900, 0], |_| {}), "signal present");
    }

    #[test]
    fn capture_and_playback_for_one_route_coexist() {
        // A loopback route starts both; stopping must kill both. (The
        // threads themselves bail instantly in CI — no audio devices —
        // which is fine: this exercises the bookkeeping, not cpal.)
        let bridge = AudioBridge::new();
        bridge.start_capture("r".into(), |_, _| {});
        bridge.start_playback("r".into());
        assert!(bridge.captures.lock().contains_key("r"));
        assert!(bridge.playbacks.lock().contains_key("r"));
        bridge.stop("r");
        assert!(!bridge.is_running("r"));
    }
}
