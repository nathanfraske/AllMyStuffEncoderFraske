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
//! v1 simplifications, called out honestly: it uses the *default* input /
//! output device (mapping a specific scanned device to a cpal device by
//! name is a follow-up), and transports mono. Both are noted in the README.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;

use allmystuff_session::AudioFrame;

/// One running route's audio resources.
struct RouteAudio {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// Present for playback routes: the ring the output stream drains and
    /// the device's output sample rate that `feed` resamples to.
    playback: Option<Playback>,
}

#[derive(Clone)]
struct Playback {
    ring: Arc<Mutex<VecDeque<i16>>>,
    out_rate: Arc<AtomicU32>,
}

impl Drop for RouteAudio {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[derive(Default)]
pub struct AudioBridge {
    routes: Mutex<HashMap<String, RouteAudio>>,
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
            if let Err(e) = run_capture(&stop_thread, on_frame) {
                tracing::warn!("audio capture for {id} stopped: {e}");
            }
        });
        self.routes.lock().insert(
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
        let id = route_id.clone();
        let thread = std::thread::spawn(move || {
            if let Err(e) = run_playback(&stop_thread, ring_thread, out_rate_thread) {
                tracing::warn!("audio playback for {id} stopped: {e}");
            }
        });
        self.routes.lock().insert(
            route_id,
            RouteAudio {
                stop,
                thread: Some(thread),
                playback: Some(Playback { ring, out_rate }),
            },
        );
    }

    /// Push an inbound frame into a playback route's ring (mono, resampled
    /// to the device rate). No-op if the route isn't a playback route.
    pub fn feed(&self, route_id: &str, frame: &AudioFrame) {
        let routes = self.routes.lock();
        let Some(pb) = routes.get(route_id).and_then(|r| r.playback.as_ref()) else {
            return;
        };
        let mono = downmix(&frame.pcm, frame.channels);
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
        self.routes.lock().remove(route_id);
    }

    pub fn stop_all(&self) {
        self.routes.lock().clear();
    }

    pub fn is_running(&self, route_id: &str) -> bool {
        self.routes.lock().contains_key(route_id)
    }
}

// ---- capture ----------------------------------------------------------

fn run_capture<F>(stop: &AtomicBool, on_frame: F) -> Result<(), String>
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
    let err = |e| tracing::warn!("input stream error: {e}");

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
    ring: Arc<Mutex<VecDeque<i16>>>,
    out_rate: Arc<AtomicU32>,
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
    park_until_stopped(stop);
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
}
