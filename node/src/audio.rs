//! The audio media plane: capture and playback with `cpal` (plus the
//! platform loopback paths), so an active audio route actually moves sound
//! across the mesh.
//!
//! cpal's `Stream` is `!Send`, so each route runs on its own dedicated
//! thread that builds the stream, keeps it alive, and drops it on stop. The
//! mesh layer never touches a stream directly — it calls [`AudioBridge`]:
//!
//!  * **source side** — [`AudioBridge::start_capture`] records what the
//!    routed capability names ([`CaptureSource`]): the default microphone
//!    for a scanned input device, or — for the synthetic `system-audio`
//!    capability, "what this machine is playing" — the OS loopback of the
//!    default output (WASAPI loopback on Windows, the pulse server's
//!    monitor source on Linux; macOS has no OS loopback API and degrades
//!    to the default input, loudly). The native L/R pair is preserved as
//!    interleaved stereo i16 for the media lane; the legacy capture callback
//!    remains available and explicitly down-mixes for older callers.
//!  * **sink side** — [`AudioBridge::start_playback`] opens the default
//!    output and drains a ring buffer that [`AudioBridge::feed`] fills from
//!    inbound frames (linear-resampled to the device rate).
//!
//! Because "routes active, no sound" is invisible from the route handshake,
//! both ends are deliberately loud about the things that go silently wrong
//! in the field: each stream logs **which device** it opened (the default
//! device may not be the one the user is thinking of), a mic capture that
//! produces nothing but zeros for its first seconds names the OS
//! microphone permission (macOS TCC and Windows privacy both deliver
//! silence rather than an error — a silent *system* capture is just a
//! quiet desktop, so that source never warns), and a playback that never
//! gets fed says so once instead of playing silence forever. The
//! flowing-state counters (`audio out/in` lines) ride the same
//! `ALLMYSTUFF_VIDEO_STATS` dial-in switch the video pipeline uses, at
//! debug otherwise.
//!
//! Capture still uses the category's *default* device (mapping a specific
//! scanned device to a cpal device by name is a follow-up).

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;

use allmystuff_session::AudioFrame;

/// What a sourcing audio route records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureSource {
    /// The default input device — a scanned microphone capability.
    Mic,
    /// This machine's own playback (the synthetic `system-audio`
    /// capability): the loopback of the default output.
    System,
}

/// Absolute receive-side guardrail. Each mode starts lower and adapts within
/// this bound; the ring remains a jitter buffer, never a latency reservoir.
const MAX_DEPTH_MS: u32 = 200;

// ---- the mesh's Opus audio lane (encode side) --------------------------

/// The audio lane's clock: Opus always runs a 48 kHz RTP clock.
pub(crate) const OPUS_RATE: u32 = 48_000;
/// 20 ms at 48 kHz — the canonical Opus frame.
#[cfg(test)]
pub(crate) const OPUS_FRAME_SAMPLES: usize = 960;

/// Audio portion of the media-policy modes. Studio Lossless intentionally
/// uses [`AudioProfile::Studio`]: the current wire codec is high-rate Opus,
/// not mathematically lossless audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AudioProfile {
    Reach,
    Balanced,
    Game,
    Studio,
}

/// Concrete Opus/jitter contract for one media mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OpusProfile {
    pub bitrate_bps: u32,
    pub frame_duration_ms: u16,
    pub inband_fec: bool,
    pub jitter_target_ms: u32,
}

impl AudioProfile {
    pub(crate) const fn opus(self) -> OpusProfile {
        match self {
            AudioProfile::Reach => OpusProfile {
                bitrate_bps: 48_000,
                frame_duration_ms: 20,
                inband_fec: true,
                jitter_target_ms: 60,
            },
            AudioProfile::Balanced => OpusProfile {
                bitrate_bps: 96_000,
                frame_duration_ms: 10,
                inband_fec: true,
                jitter_target_ms: 40,
            },
            AudioProfile::Game => OpusProfile {
                bitrate_bps: 128_000,
                frame_duration_ms: 5,
                inband_fec: false,
                jitter_target_ms: 20,
            },
            AudioProfile::Studio => OpusProfile {
                bitrate_bps: 192_000,
                frame_duration_ms: 10,
                inband_fec: false,
                jitter_target_ms: 40,
            },
        }
    }
}

impl OpusProfile {
    pub(crate) const fn frame_samples_per_channel(self) -> usize {
        OPUS_RATE as usize * self.frame_duration_ms as usize / 1_000
    }

    pub(crate) const fn frame_duration_us(self) -> u64 {
        self.frame_duration_ms as u64 * 1_000
    }
}

/// Chops capture buffers at arbitrary device cadence into exact Opus frames.
/// Policy-configured streams are 48 kHz stereo; the legacy constructor stays
/// mono/20 ms until its existing mesh call site migrates to [`Self::with_profile`].
pub(crate) struct OpusStream {
    enc: opus::Encoder,
    profile: OpusProfile,
    channels: u16,
    /// Keeps the fractional device-to-48 kHz phase across capture callbacks.
    /// Independently rounding short 44.1/48 kHz callbacks accumulates drift.
    resampler: StreamingResampler,
    /// 48 kHz interleaved samples awaiting a full frame.
    buf: Vec<i16>,
}

impl OpusStream {
    /// Backward-compatible lane used by older mesh callers.
    ///
    /// New policy-aware callers should use [`Self::with_profile`] and pass
    /// the capture channel count to [`Self::push_interleaved`].
    #[cfg(test)]
    pub(crate) fn new() -> Result<Self, String> {
        Self::build(
            OpusProfile {
                bitrate_bps: 96_000,
                frame_duration_ms: 20,
                inband_fec: false,
                jitter_target_ms: 80,
            },
            1,
        )
    }

    /// Build the stereo Opus encoder specified by the selected media mode.
    pub(crate) fn with_profile(profile: AudioProfile) -> Result<Self, String> {
        Self::build(profile.opus(), 2)
    }

    fn build(profile: OpusProfile, channels: u16) -> Result<Self, String> {
        let opus_channels = if channels == 1 {
            opus::Channels::Mono
        } else {
            opus::Channels::Stereo
        };
        let mut enc = opus::Encoder::new(OPUS_RATE, opus_channels, opus::Application::Audio)
            .map_err(|e| e.to_string())?;
        enc.set_bitrate(opus::Bitrate::Bits(profile.bitrate_bps as i32))
            .map_err(|e| e.to_string())?;
        enc.set_inband_fec(profile.inband_fec)
            .map_err(|e| e.to_string())?;
        if profile.inband_fec {
            // Opus uses this estimate to decide how much redundancy to spend.
            enc.set_packet_loss_perc(if profile.bitrate_bps <= 48_000 { 10 } else { 5 })
                .map_err(|e| e.to_string())?;
        }
        let frame_samples = profile.frame_samples_per_channel() * channels as usize;
        Ok(OpusStream {
            enc,
            profile,
            channels,
            resampler: StreamingResampler::new(),
            buf: Vec::with_capacity(frame_samples * 2),
        })
    }

    pub(crate) fn profile(&self) -> OpusProfile {
        self.profile
    }

    #[cfg(test)]
    pub(crate) fn channels(&self) -> u16 {
        self.channels
    }

    /// Legacy mono input. A policy-configured stereo encoder duplicates the
    /// mono signal without inventing channel separation.
    #[cfg(test)]
    pub(crate) fn push(&mut self, pcm: &[i16], rate: u32, emit: impl FnMut(Vec<u8>)) {
        self.push_interleaved(pcm, rate, 1, emit);
    }

    /// Push interleaved capture PCM while preserving the native L/R pair.
    pub(crate) fn push_interleaved(
        &mut self,
        pcm: &[i16],
        rate: u32,
        input_channels: u16,
        mut emit: impl FnMut(Vec<u8>),
    ) {
        let channel_converted;
        let shaped = if input_channels.max(1) == self.channels {
            pcm
        } else {
            channel_converted = convert_channels(pcm, input_channels, self.channels);
            &channel_converted
        };
        if rate == OPUS_RATE || rate == 0 {
            // A device-rate transition is a discontinuity. Do not retain a
            // fractional phase from the old rate if the device moves to the
            // Opus clock (or temporarily reports an unknown rate).
            self.resampler.reset();
            self.buf.extend_from_slice(shaped);
        } else {
            let resampled = self
                .resampler
                .process(shaped, rate, OPUS_RATE, self.channels);
            self.buf.extend_from_slice(&resampled);
        }
        let frame_samples = self.profile.frame_samples_per_channel() * self.channels as usize;
        let mut off = 0;
        while self.buf.len() - off >= frame_samples {
            let frame = &self.buf[off..off + frame_samples];
            match self.enc.encode_vec(frame, 4000) {
                Ok(pkt) => emit(pkt),
                // A failed frame costs one policy packet, never the stream.
                Err(e) => tracing::debug!("opus encode failed: {e}"),
            }
            off += frame_samples;
        }
        self.buf.drain(..off);
    }
}

/// How an Opus output buffer was produced. Exposing this distinction lets the
/// mesh report actual FEC/PLC recovery rather than counting every decoded
/// packet as healthy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpusDecodeKind {
    Normal,
    Fec,
    Plc,
}

/// One stereo PCM buffer emitted by [`OpusReceiver`]. `media_timestamp_us` is
/// unwrapped from the 48 kHz RTP clock and is therefore monotonic across the
/// 32-bit timestamp wrap.
#[derive(Debug)]
pub(crate) struct DecodedOpus {
    pub seq: u64,
    pub media_timestamp_us: u64,
    pub kind: OpusDecodeKind,
    pub pcm: Vec<i16>,
}

/// Stereo Opus decoder with one-packet inband-FEC recovery and bounded PLC.
/// RTP timestamps are media-plane metadata already present on inbound binary
/// frames; this type never consults or writes the signaling channel.
pub(crate) struct OpusReceiver {
    decoder: opus::Decoder,
    profile: OpusProfile,
    rtp_timestamps: RtpTimestampState,
    last_packet_samples: u32,
    media_sample_clock: u64,
    next_output_seq: u64,
}

/// A zero RTP timestamp is both a valid point on the 32-bit Opus clock and
/// the legacy JSON fallback's sentinel for "timestamp absent". Keep the first
/// zero provisional: a following clock step proves it was real, while a
/// second zero selects the legacy duration-only clock. Once timestamps are
/// present, zero remains fully valid at wrap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RtpTimestampState {
    Unknown,
    ProvisionalZero,
    Present(u32),
    Absent,
}

impl OpusReceiver {
    pub(crate) fn new(profile: AudioProfile) -> Result<Self, String> {
        let profile = profile.opus();
        let decoder = opus::Decoder::new(OPUS_RATE, opus::Channels::Stereo)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            decoder,
            profile,
            rtp_timestamps: RtpTimestampState::Unknown,
            last_packet_samples: profile.frame_samples_per_channel() as u32,
            media_sample_clock: 0,
            next_output_seq: 0,
        })
    }

    pub(crate) fn set_profile(&mut self, profile: AudioProfile) {
        self.profile = profile.opus();
    }

    /// Decode the current packet plus any immediately preceding missing
    /// packets inferable from its RTP timestamp. A gap beyond 120 ms resets
    /// codec history and resumes at the live packet instead of manufacturing
    /// a long, stale concealment tail.
    pub(crate) fn decode(
        &mut self,
        rtp_timestamp: u32,
        packet: &[u8],
    ) -> Result<Vec<DecodedOpus>, String> {
        let mut outputs = Vec::new();
        let frame_samples = self.profile.frame_samples_per_channel() as u32;

        let (last_timestamp, next_timestamp_state, anchor_clock) = match self.rtp_timestamps {
            RtpTimestampState::Unknown => (
                None,
                if rtp_timestamp == 0 {
                    RtpTimestampState::ProvisionalZero
                } else {
                    RtpTimestampState::Present(rtp_timestamp)
                },
                Some(rtp_timestamp),
            ),
            RtpTimestampState::ProvisionalZero if rtp_timestamp == 0 => {
                (None, RtpTimestampState::Absent, None)
            }
            RtpTimestampState::ProvisionalZero => {
                (Some(0), RtpTimestampState::Present(rtp_timestamp), None)
            }
            RtpTimestampState::Present(last) => {
                (Some(last), RtpTimestampState::Present(rtp_timestamp), None)
            }
            RtpTimestampState::Absent if rtp_timestamp == 0 => {
                (None, RtpTimestampState::Absent, None)
            }
            // A legacy sender may be upgraded/restarted under a live route.
            // Adopt its clock without interpreting the transition as loss.
            RtpTimestampState::Absent => (None, RtpTimestampState::Present(rtp_timestamp), None),
        };
        if let Some(anchor) = anchor_clock {
            self.media_sample_clock = u64::from(anchor);
        }

        let missing_packets = if let Some(last) = last_timestamp {
            let expected = last.wrapping_add(self.last_packet_samples);
            let gap_samples = rtp_timestamp.wrapping_sub(expected);
            if gap_samples == 0 {
                0
            } else if gap_samples % frame_samples == 0
                && gap_samples / frame_samples
                    <= (120 / self.profile.frame_duration_ms as u32).max(1)
            {
                gap_samples / frame_samples
            } else {
                self.decoder
                    .reset_state()
                    .map_err(|error| error.to_string())?;
                0
            }
        } else {
            0
        };

        if missing_packets > 0 {
            let plc_packets = if self.profile.inband_fec {
                missing_packets - 1
            } else {
                missing_packets
            };
            for _ in 0..plc_packets {
                if let Ok(decoded) = self.decode_one(&[], false, OpusDecodeKind::Plc) {
                    outputs.push(decoded);
                }
            }
            if self.profile.inband_fec {
                if let Ok(decoded) = self.decode_one(packet, true, OpusDecodeKind::Fec) {
                    outputs.push(decoded);
                }
            }
        }

        let current = self.decode_one(packet, false, OpusDecodeKind::Normal)?;
        self.last_packet_samples = (current.pcm.len() / 2) as u32;
        outputs.push(current);
        self.rtp_timestamps = next_timestamp_state;
        Ok(outputs)
    }

    fn decode_one(
        &mut self,
        packet: &[u8],
        fec: bool,
        kind: OpusDecodeKind,
    ) -> Result<DecodedOpus, String> {
        // For an ordinary packet, Opus may legally return as much as 120 ms.
        // For PLC/FEC, however, libopus uses the output-buffer capacity as the
        // requested concealment duration. Giving those calls a 120 ms buffer
        // would therefore turn one missing 5/10/20 ms packet into 120 ms of
        // synthetic audio and advance the media clock by the same amount.
        let frames_capacity = match kind {
            OpusDecodeKind::Normal => OPUS_RATE as usize * 120 / 1_000,
            OpusDecodeKind::Fec | OpusDecodeKind::Plc => self.profile.frame_samples_per_channel(),
        };
        let mut pcm = vec![0i16; frames_capacity * 2];
        let frames = self
            .decoder
            .decode(packet, &mut pcm, fec)
            .map_err(|error| error.to_string())?;
        pcm.truncate(frames * 2);
        let decoded = DecodedOpus {
            seq: self.next_output_seq,
            media_timestamp_us: self
                .media_sample_clock
                .saturating_mul(1_000_000)
                .saturating_div(OPUS_RATE as u64),
            kind,
            pcm,
        };
        self.next_output_seq = self.next_output_seq.wrapping_add(1);
        self.media_sample_clock = self.media_sample_clock.saturating_add(frames as u64);
        Ok(decoded)
    }
}

/// One running route's audio resources (a capture or a playback thread).
struct RouteAudio {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// Present for playback routes: the ring the output stream drains and
    /// the device's output sample rate that `feed` resamples to.
    playback: Option<Playback>,
}

struct Playback {
    /// Canonical 48 kHz-or-device-rate interleaved stereo. Holding a fixed
    /// wire shape keeps L/R intact regardless of the output device layout.
    ring: Arc<Mutex<VecDeque<i16>>>,
    out_rate: Arc<AtomicU32>,
    /// Frames `feed` has accepted — the playback thread warns once when
    /// this is still zero seconds after the route went live.
    fed: Arc<AtomicU64>,
    runtime: Arc<PlaybackRuntime>,
    jitter: Mutex<AdaptiveJitter>,
    /// Device-rate conversion is route-local so fractional phase and the
    /// boundary sample survive each network packet.
    resampler: Mutex<StreamingResampler>,
    clock_origin: Instant,
    /// Receive-side dial-in counters.
    stats: Mutex<LevelStats>,
}

/// Non-destructive receive telemetry for diagnostics and effective-policy
/// readouts. Depth and underrun counts are measured in per-channel frames.
#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AudioReceiveStats {
    pub target_depth_ms: u32,
    pub buffered_depth_ms: u32,
    pub arrival_jitter_us: u64,
    pub underrun_events: u64,
    pub underrun_frames: u64,
    pub trimmed_frames: u64,
}

/// Interval counters intended for the existing receiver-feedback extension.
/// Calling `take_receive_feedback` clears only the interval underrun counts;
/// totals in [`AudioReceiveStats`] remain monotonic.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AudioReceiveFeedback {
    pub target_depth_ms: u32,
    pub buffered_depth_ms: u32,
    pub arrival_jitter_us: u64,
    pub underrun_events: u64,
    pub underrun_frames: u64,
}

struct PlaybackRuntime {
    base_depth_ms: AtomicU32,
    target_depth_ms: AtomicU32,
    primed: AtomicBool,
    arrival_jitter_us: AtomicU64,
    underrun_events: AtomicU64,
    underrun_frames: AtomicU64,
    pending_underrun_events: AtomicU64,
    pending_underrun_frames: AtomicU64,
    trimmed_frames: AtomicU64,
}

impl PlaybackRuntime {
    fn new(base_depth_ms: u32) -> Self {
        let base_depth_ms = base_depth_ms.clamp(5, MAX_DEPTH_MS);
        Self {
            base_depth_ms: AtomicU32::new(base_depth_ms),
            target_depth_ms: AtomicU32::new(base_depth_ms),
            primed: AtomicBool::new(false),
            arrival_jitter_us: AtomicU64::new(0),
            underrun_events: AtomicU64::new(0),
            underrun_frames: AtomicU64::new(0),
            pending_underrun_events: AtomicU64::new(0),
            pending_underrun_frames: AtomicU64::new(0),
            trimmed_frames: AtomicU64::new(0),
        }
    }

    /// One starvation episode, not one callback: the caller flips `primed`
    /// false after this, so playback must refill before another episode can
    /// be counted. The target rises immediately and decays only on arrivals.
    fn note_underrun(&self, missing_frames: u64) {
        self.underrun_events.fetch_add(1, Ordering::Relaxed);
        self.underrun_frames
            .fetch_add(missing_frames, Ordering::Relaxed);
        self.pending_underrun_events.fetch_add(1, Ordering::Relaxed);
        self.pending_underrun_frames
            .fetch_add(missing_frames, Ordering::Relaxed);
        let base_depth_ms = self.base_depth_ms.load(Ordering::Relaxed);
        let step = (base_depth_ms / 2).max(5);
        let _ =
            self.target_depth_ms
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                    Some(current.saturating_add(step).min(MAX_DEPTH_MS))
                });
        self.primed.store(false, Ordering::Release);
    }

    fn set_base_depth_ms(&self, base_depth_ms: u32) {
        let base_depth_ms = base_depth_ms.clamp(5, MAX_DEPTH_MS);
        self.base_depth_ms.store(base_depth_ms, Ordering::Relaxed);
        let _ =
            self.target_depth_ms
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                    Some(current.max(base_depth_ms).min(MAX_DEPTH_MS))
                });
    }
}

/// RFC-3550-style arrival variation with bounded fast-rise/slow-fall target
/// adaptation. Sender media timestamps make the estimate independent of
/// capture callback cadence; old frames fall back to their PCM duration.
struct AdaptiveJitter {
    last_seq: Option<u64>,
    last_media_timestamp_us: Option<u64>,
    last_duration_us: u64,
    last_arrival_us: Option<u64>,
    jitter_us: f64,
}

impl AdaptiveJitter {
    fn new() -> Self {
        Self {
            last_seq: None,
            last_media_timestamp_us: None,
            last_duration_us: 0,
            last_arrival_us: None,
            jitter_us: 0.0,
        }
    }

    fn note_arrival(
        &mut self,
        seq: u64,
        media_timestamp_us: Option<u64>,
        duration_us: u64,
        arrival_us: u64,
        runtime: &PlaybackRuntime,
    ) {
        if let (Some(last_seq), Some(last_arrival)) = (self.last_seq, self.last_arrival_us) {
            if seq > last_seq {
                let arrival_delta = arrival_us.saturating_sub(last_arrival);
                let media_delta = match (media_timestamp_us, self.last_media_timestamp_us) {
                    (Some(now), Some(previous)) if now > previous => now - previous,
                    _ => self
                        .last_duration_us
                        .saturating_mul(seq.saturating_sub(last_seq)),
                };
                // Ignore pauses and obviously reset clocks. They should cause
                // rebuffering, not poison the steady-state jitter estimate.
                if media_delta > 0 && media_delta <= 1_000_000 && arrival_delta <= 1_000_000 {
                    let transit_delta = arrival_delta.abs_diff(media_delta) as f64;
                    self.jitter_us += (transit_delta - self.jitter_us) / 16.0;
                    runtime
                        .arrival_jitter_us
                        .store(self.jitter_us.round() as u64, Ordering::Relaxed);

                    let base_depth_ms = runtime.base_depth_ms.load(Ordering::Relaxed);
                    let desired = base_depth_ms
                        .saturating_add((self.jitter_us * 4.0 / 1_000.0).ceil() as u32)
                        .clamp(base_depth_ms, MAX_DEPTH_MS);
                    let current = runtime.target_depth_ms.load(Ordering::Relaxed);
                    let next = if desired > current {
                        desired // fast rise
                    } else if current > desired {
                        current - 1 // slow fall: at most 1 ms per packet
                    } else {
                        current
                    };
                    runtime.target_depth_ms.store(next, Ordering::Relaxed);
                }
            }
        }
        self.last_seq = Some(seq);
        self.last_media_timestamp_us = media_timestamp_us;
        self.last_duration_us = duration_us;
        self.last_arrival_us = Some(arrival_us);
    }
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

    /// Legacy mono capture callback. New media-policy callers should use
    /// [`Self::start_capture_interleaved`] so the source's L/R pair reaches
    /// the stereo Opus lane.
    pub fn start_capture<F>(&self, route_id: String, source: CaptureSource, on_frame: F)
    where
        F: Fn(Vec<i16>, u32) + Send + Sync + 'static,
    {
        self.start_capture_interleaved(route_id, source, move |pcm, rate, channels| {
            on_frame(downmix(&pcm, channels), rate)
        });
    }

    /// Begin capturing interleaved stereo for `route_id`.
    /// `on_frame(pcm, sample_rate, channels)` always reports two channels;
    /// mono devices are duplicated and devices with more channels preserve
    /// their first L/R pair.
    pub fn start_capture_interleaved<F>(&self, route_id: String, source: CaptureSource, on_frame: F)
    where
        F: Fn(Vec<i16>, u32, u16) + Send + Sync + 'static,
    {
        // Exactly one capture pump per route (the video bridge's discipline):
        // a duplicate StartMedia (the daemon redelivers an Offer once per
        // shared network) must not double-start audio capture — and with the
        // release profile's `panic = "abort"` a panic on a stray capture
        // thread aborts the host. Keyed on the captures map only, so a
        // loopback route (capture + playback under one id) is unaffected.
        if self.captures.lock().contains_key(&route_id) {
            tracing::debug!(
                "audio capture already running for {route_id}; ignoring duplicate start"
            );
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let id = route_id.clone();
        let thread = std::thread::spawn(move || {
            if let Err(e) = run_capture(&stop_thread, &id, source, on_frame) {
                tracing::warn!("audio capture for {id} stopped: {e}");
            }
        });
        // Insert, then join any displaced thread off the lock (RouteAudio::drop
        // joins) — never under the captures lock.
        let displaced = {
            let mut captures = self.captures.lock();
            captures.insert(
                route_id,
                RouteAudio {
                    stop,
                    thread: Some(thread),
                    playback: None,
                },
            )
        };
        drop(displaced);
    }

    /// Begin playing inbound audio using the Balanced receive contract.
    /// Policy-aware callers should use [`Self::start_playback_with_profile`].
    pub fn start_playback(&self, route_id: String) {
        self.start_playback_with_profile(route_id, AudioProfile::Balanced);
    }

    /// Begin playing inbound audio with the selected mode's initial jitter
    /// target. Arrival variation and underruns adapt it within 5–200 ms.
    pub(crate) fn start_playback_with_profile(&self, route_id: String, profile: AudioProfile) {
        // One playback pump per route, for the same reason as `start_capture`;
        // keyed on the playbacks map so a loopback route still runs both.
        if self.playbacks.lock().contains_key(&route_id) {
            tracing::debug!(
                "audio playback already running for {route_id}; ignoring duplicate start"
            );
            return;
        }
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
        let runtime = Arc::new(PlaybackRuntime::new(profile.opus().jitter_target_ms));
        let runtime_thread = runtime.clone();
        let clock_origin = Instant::now();
        let id = route_id.clone();
        let thread = std::thread::spawn(move || {
            if let Err(e) = run_playback(
                &stop_thread,
                &id,
                ring_thread,
                out_rate_thread,
                fed_thread,
                runtime_thread,
            ) {
                tracing::warn!("audio playback for {id} stopped: {e}");
            }
        });
        let displaced = {
            let mut playbacks = self.playbacks.lock();
            playbacks.insert(
                route_id,
                RouteAudio {
                    stop,
                    thread: Some(thread),
                    playback: Some(Playback {
                        ring,
                        out_rate,
                        fed,
                        runtime,
                        jitter: Mutex::new(AdaptiveJitter::new()),
                        resampler: Mutex::new(StreamingResampler::new()),
                        clock_origin,
                        stats: Mutex::new(LevelStats::new()),
                    }),
                },
            )
        };
        // Join any displaced playback thread off the lock (RouteAudio::drop).
        drop(displaced);
    }

    /// Change the receive jitter floor without restarting the output device.
    pub(crate) fn set_playback_profile(&self, route_id: &str, profile: AudioProfile) {
        let routes = self.playbacks.lock();
        if let Some(pb) = routes.get(route_id).and_then(|r| r.playback.as_ref()) {
            pb.runtime
                .set_base_depth_ms(profile.opus().jitter_target_ms);
        }
    }

    /// Push an inbound frame into a playback route's stereo jitter ring,
    /// resampling each channel independently. No-op for unknown routes.
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
        let channel_converted;
        let stereo = if frame.channels.max(1) == 2 {
            &frame.pcm[..]
        } else {
            channel_converted = convert_channels(&frame.pcm, frame.channels, 2);
            &channel_converted[..]
        };
        {
            let mut stats = pb.stats.lock();
            let id = route_id.to_string();
            stats.note(stereo, move |line| {
                stats_log(format!("audio in {id}: {line}"))
            });
        }
        let out_rate = pb.out_rate.load(Ordering::Relaxed);
        let resampled;
        let samples = if frame.sample_rate == out_rate || frame.sample_rate == 0 {
            pb.resampler.lock().reset();
            stereo
        } else {
            resampled = pb
                .resampler
                .lock()
                .process(stereo, frame.sample_rate, out_rate, 2);
            &resampled[..]
        };

        let duration_us = if frame.sample_rate == 0 {
            0
        } else {
            (frame.frame_count() as u64)
                .saturating_mul(1_000_000)
                .saturating_div(frame.sample_rate as u64)
        };
        pb.jitter.lock().note_arrival(
            frame.seq,
            frame.media_timestamp_us,
            duration_us,
            pb.clock_origin.elapsed().as_micros().min(u64::MAX as u128) as u64,
            &pb.runtime,
        );

        let mut ring = pb.ring.lock();
        ring.extend(samples.iter().copied());
        // Keep playout close behind the live edge. The ring is a jitter
        // buffer, not a reservoir: whatever piles up beyond MAX_DEPTH_MS —
        // the device-open transient at session start, a network burst,
        // slow clock drift between the two ends — becomes *permanent* lag
        // behind the video stream (which drops stale frames) unless it's
        // cut. Trim the *oldest* samples back to the adaptive target: one
        // small audible skip, and the audio is current again.
        let target_ms = pb.runtime.target_depth_ms.load(Ordering::Relaxed);
        let trim_at_ms = target_ms
            .saturating_add((target_ms / 2).max(40))
            .min(MAX_DEPTH_MS);
        let ring_frames = ring.len() / 2;
        let max_frames = (out_rate as usize / 1_000) * trim_at_ms as usize;
        let target_frames = (out_rate as usize / 1_000) * target_ms as usize;
        if ring_frames > max_frames.max(1) {
            let excess_frames = ring_frames - target_frames;
            ring.drain(..excess_frames * 2);
            pb.runtime
                .trimmed_frames
                .fetch_add(excess_frames as u64, Ordering::Relaxed);
            tracing::debug!(
                "audio ring for {route_id} trimmed to {target_ms} ms (held {} ms)",
                ring_frames / (out_rate as usize / 1_000).max(1)
            );
        }
    }

    /// Snapshot monotonic receive diagnostics without perturbing feedback.
    #[cfg(test)]
    pub(crate) fn receive_stats(&self, route_id: &str) -> Option<AudioReceiveStats> {
        let routes = self.playbacks.lock();
        let pb = routes.get(route_id)?.playback.as_ref()?;
        let out_rate = pb.out_rate.load(Ordering::Relaxed).max(1);
        let buffered_depth_ms = ((pb.ring.lock().len() / 2) as u64)
            .saturating_mul(1_000)
            .saturating_div(out_rate as u64) as u32;
        Some(AudioReceiveStats {
            target_depth_ms: pb.runtime.target_depth_ms.load(Ordering::Relaxed),
            buffered_depth_ms,
            arrival_jitter_us: pb.runtime.arrival_jitter_us.load(Ordering::Relaxed),
            underrun_events: pb.runtime.underrun_events.load(Ordering::Relaxed),
            underrun_frames: pb.runtime.underrun_frames.load(Ordering::Relaxed),
            trimmed_frames: pb.runtime.trimmed_frames.load(Ordering::Relaxed),
        })
    }

    /// Drain interval underrun counters for receiver feedback while retaining
    /// monotonic totals for diagnostics.
    pub(crate) fn take_receive_feedback(&self, route_id: &str) -> Option<AudioReceiveFeedback> {
        let routes = self.playbacks.lock();
        let pb = routes.get(route_id)?.playback.as_ref()?;
        let out_rate = pb.out_rate.load(Ordering::Relaxed).max(1);
        let buffered_depth_ms = ((pb.ring.lock().len() / 2) as u64)
            .saturating_mul(1_000)
            .saturating_div(out_rate as u64) as u32;
        Some(AudioReceiveFeedback {
            target_depth_ms: pb.runtime.target_depth_ms.load(Ordering::Relaxed),
            buffered_depth_ms,
            arrival_jitter_us: pb.runtime.arrival_jitter_us.load(Ordering::Relaxed),
            underrun_events: pb
                .runtime
                .pending_underrun_events
                .swap(0, Ordering::Relaxed),
            underrun_frames: pb
                .runtime
                .pending_underrun_frames
                .swap(0, Ordering::Relaxed),
        })
    }

    pub fn stop(&self, route_id: &str) {
        // Drop runs the thread join + stream teardown — bind the removed
        // values so that join happens after each lock guard is released, never
        // under it (an unbound `remove(..);` drops while the guard is held).
        let removed_capture = self.captures.lock().remove(route_id);
        let removed_playback = self.playbacks.lock().remove(route_id);
        drop(removed_capture);
        drop(removed_playback);
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

fn run_capture<F>(
    stop: &AtomicBool,
    route_id: &str,
    source: CaptureSource,
    on_frame: F,
) -> Result<(), String>
where
    F: Fn(Vec<i16>, u32, u16) + Send + Sync + 'static,
{
    // The meter wraps every path; the `Arc` is so a failed loopback can
    // hand the same consumer to its fallback (cpal consumes the callback
    // even when the stream build fails).
    let on_frame = Arc::new(metered(route_id, source, on_frame));
    match source {
        CaptureSource::Mic => run_capture_cpal(stop, route_id, false, on_frame),
        CaptureSource::System => run_system_capture(stop, route_id, on_frame),
    }
}

/// Wrap a capture consumer with the level meter: the ~5 s dial-in line,
/// plus the one-shot pure-silence warning. The warning names the OS
/// microphone permission and only fires for [`CaptureSource::Mic`] —
/// macOS TCC / Windows privacy deliver silence rather than an error,
/// while a silent *system* capture is just a quiet desktop.
fn metered<F>(
    route_id: &str,
    source: CaptureSource,
    on_frame: F,
) -> impl Fn(Vec<i16>, u32, u16) + Send + Sync + 'static
where
    F: Fn(Vec<i16>, u32, u16) + Send + Sync + 'static,
{
    let stats = Mutex::new(LevelStats::new());
    let id = route_id.to_string();
    move |pcm: Vec<i16>, rate: u32, channels: u16| {
        {
            let mut stats = stats.lock();
            let line_id = id.clone();
            let silent = stats.note(&pcm, move |line| {
                stats_log(format!("audio out {line_id}: {line}"));
            });
            if silent && source == CaptureSource::Mic && !stats.warned_silent {
                stats.warned_silent = true;
                tracing::warn!(
                    "audio capture for {id} is producing pure silence — check the OS \
                     microphone permission for this app (and the default input device)"
                );
            }
        }
        on_frame(pcm, rate, channels);
    }
}

/// System-audio capture on Windows: WASAPI loopback. An *input* stream
/// built on the default *output* device captures the machine's playback
/// mix (cpal raises `AUDCLNT_STREAMFLAGS_LOOPBACK` for render-device
/// input streams). One quirk to know when reading logs: loopback delivers
/// buffers only while something renders, so a fully silent desktop
/// produces no frames at all — that's normal, not a stall.
#[cfg(windows)]
fn run_system_capture<F>(stop: &AtomicBool, route_id: &str, on_frame: Arc<F>) -> Result<(), String>
where
    F: Fn(Vec<i16>, u32, u16) + Send + Sync + 'static,
{
    match run_capture_cpal(stop, route_id, true, on_frame.clone()) {
        Err(e) => {
            tracing::warn!(
                "system-audio loopback for {route_id} failed ({e}) — capturing the default \
                 input instead"
            );
            run_capture_cpal(stop, route_id, false, on_frame)
        }
        done => done,
    }
}

/// System-audio capture on Linux: the pulse server's monitor of the
/// default sink (PulseAudio natively, PipeWire via pipewire-pulse — i.e.
/// effectively every desktop). A box without a pulse server (bare ALSA)
/// degrades to the default input, loudly.
#[cfg(target_os = "linux")]
fn run_system_capture<F>(stop: &AtomicBool, route_id: &str, on_frame: Arc<F>) -> Result<(), String>
where
    F: Fn(Vec<i16>, u32, u16) + Send + Sync + 'static,
{
    match pulse_monitor::Monitor::open(route_id) {
        Ok(monitor) => {
            tracing::info!(
                "audio capture for {route_id}: system audio (monitor of the default output, \
                 via the pulse server)"
            );
            monitor.pump(stop, &*on_frame)
        }
        Err(e) => {
            tracing::warn!(
                "system-audio capture for {route_id} unavailable ({e}) — capturing the \
                 default input instead"
            );
            run_capture_cpal(stop, route_id, false, on_frame)
        }
    }
}

/// System-audio capture on macOS: there is no OS loopback API — capturing
/// the played mix needs a virtual output device (BlackHole et al.). Until
/// that's wired, the honest degradation is the default input, named in
/// the log.
#[cfg(target_os = "macos")]
fn run_system_capture<F>(stop: &AtomicBool, route_id: &str, on_frame: Arc<F>) -> Result<(), String>
where
    F: Fn(Vec<i16>, u32, u16) + Send + Sync + 'static,
{
    tracing::warn!(
        "system-audio capture isn't available on macOS without a virtual loopback device — \
         capturing the default input for {route_id} instead"
    );
    run_capture_cpal(stop, route_id, false, on_frame)
}

/// Everywhere else with the audio plane built (iOS today; any BSD
/// tomorrow): no loopback API at all, so the same honest degradation as
/// macOS — the default input, named in the log.
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn run_system_capture<F>(stop: &AtomicBool, route_id: &str, on_frame: Arc<F>) -> Result<(), String>
where
    F: Fn(Vec<i16>, u32, u16) + Send + Sync + 'static,
{
    tracing::warn!(
        "system-audio capture isn't available on this platform — capturing the default \
         input for {route_id} instead"
    );
    run_capture_cpal(stop, route_id, false, on_frame)
}

fn run_capture_cpal<F>(
    stop: &AtomicBool,
    route_id: &str,
    loopback: bool,
    on_frame: Arc<F>,
) -> Result<(), String>
where
    F: Fn(Vec<i16>, u32, u16) + Send + Sync + 'static,
{
    let host = cpal::default_host();
    // Loopback (Windows only — see `run_system_capture`) records the
    // default *output* device with its render mix format; everything else
    // records the default input.
    let (device, supported, what) = if loopback {
        let device = host
            .default_output_device()
            .ok_or_else(|| "no default output device to capture".to_string())?;
        let supported = device.default_output_config().map_err(|e| e.to_string())?;
        (device, supported, "system audio (loopback)")
    } else {
        let device = host
            .default_input_device()
            .ok_or_else(|| "no default input device".to_string())?;
        let supported = device.default_input_config().map_err(|e| e.to_string())?;
        (device, supported, "default input")
    };
    let sample_rate = supported.sample_rate().0;
    let channels = supported.channels();
    let config: cpal::StreamConfig = supported.config();
    let fmt = supported.sample_format();
    // Name the device: "no sound" is regularly "the default device isn't
    // the one you think it is".
    tracing::info!(
        "audio capture for {route_id}: {what} — device \"{}\", {sample_rate} Hz, {channels} ch, {fmt:?}",
        device.name().unwrap_or_else(|_| "unknown".into()),
    );
    let err = |e| tracing::warn!("input stream error: {e}");

    // Only one arm runs; each gets its own handle on the shared consumer.
    let stream = match fmt {
        cpal::SampleFormat::F32 => {
            let on_frame = on_frame.clone();
            device.build_input_stream(
                &config,
                move |data: &[f32], _: &_| {
                    let native: Vec<i16> = data.iter().map(|&f| f32_to_i16(f)).collect();
                    on_frame(convert_channels(&native, channels, 2), sample_rate, 2);
                },
                err,
                None,
            )
        }
        cpal::SampleFormat::I16 => {
            let on_frame = on_frame.clone();
            device.build_input_stream(
                &config,
                move |data: &[i16], _: &_| {
                    on_frame(convert_channels(data, channels, 2), sample_rate, 2)
                },
                err,
                None,
            )
        }
        cpal::SampleFormat::U16 => {
            let on_frame = on_frame.clone();
            device.build_input_stream(
                &config,
                move |data: &[u16], _: &_| {
                    let native: Vec<i16> =
                        data.iter().map(|&u| (u as i32 - 32768) as i16).collect();
                    on_frame(convert_channels(&native, channels, 2), sample_rate, 2);
                },
                err,
                None,
            )
        }
        other => return Err(format!("unsupported input sample format: {other:?}")),
    }
    .map_err(|e| e.to_string())?;

    stream.play().map_err(|e| e.to_string())?;
    park_until_stopped(stop);
    Ok(())
}

/// Recording the pulse server's monitor source — Linux's "what this
/// machine is playing". Uses PulseAudio's **simple** API, dlopen'd at
/// runtime so machines without `libpulse` (a bare-ALSA box, a container)
/// degrade to the caller's fallback instead of failing to load the app.
/// The server side resamples to the requested spec, so this asks for the
/// route's wire shape (48 kHz stereo S16) directly.
#[cfg(target_os = "linux")]
mod pulse_monitor {
    use std::ffi::{c_char, c_int, c_void, CString};
    use std::sync::atomic::{AtomicBool, Ordering};

    /// `pa_sample_spec` — pulse/sample.h.
    #[repr(C)]
    struct SampleSpec {
        format: c_int,
        rate: u32,
        channels: u8,
    }

    /// `pa_buffer_attr` — pulse/def.h. `u32::MAX` means "server default".
    #[repr(C)]
    struct BufferAttr {
        maxlength: u32,
        tlength: u32,
        prebuf: u32,
        minreq: u32,
        fragsize: u32,
    }

    const PA_SAMPLE_S16LE: c_int = 3;
    const PA_STREAM_RECORD: c_int = 2;
    const RATE: u32 = 48_000;
    const CHANNELS: usize = 2;
    /// 20 ms per blocking read — the same order as a cpal capture buffer.
    const SAMPLES_PER_READ: usize = RATE as usize / 50;

    type PaSimpleNew = unsafe extern "C" fn(
        *const c_char, // server (null = default)
        *const c_char, // application name
        c_int,         // direction
        *const c_char, // device
        *const c_char, // stream name
        *const SampleSpec,
        *const c_void, // channel map (null = default)
        *const BufferAttr,
        *mut c_int, // error out
    ) -> *mut c_void;
    type PaSimpleRead = unsafe extern "C" fn(*mut c_void, *mut c_void, usize, *mut c_int) -> c_int;
    type PaSimpleFree = unsafe extern "C" fn(*mut c_void);

    struct Api {
        new: PaSimpleNew,
        read: PaSimpleRead,
        free: PaSimpleFree,
    }

    /// dlopen once per process. The library handle is deliberately leaked
    /// so the extracted function pointers stay valid for the app's life.
    fn api() -> Option<&'static Api> {
        static API: std::sync::OnceLock<Option<Api>> = std::sync::OnceLock::new();
        API.get_or_init(|| unsafe {
            let lib = ["libpulse-simple.so.0", "libpulse-simple.so"]
                .iter()
                .find_map(|name| libloading::Library::new(name).ok())?;
            let api = {
                let new: libloading::Symbol<PaSimpleNew> = lib.get(b"pa_simple_new\0").ok()?;
                let read: libloading::Symbol<PaSimpleRead> = lib.get(b"pa_simple_read\0").ok()?;
                let free: libloading::Symbol<PaSimpleFree> = lib.get(b"pa_simple_free\0").ok()?;
                Api {
                    new: *new,
                    read: *read,
                    free: *free,
                }
            };
            std::mem::forget(lib);
            Some(api)
        })
        .as_ref()
    }

    pub(super) struct Monitor {
        api: &'static Api,
        stream: *mut c_void,
    }

    impl Monitor {
        /// Open a recording stream on the monitor of the default sink.
        /// `@DEFAULT_MONITOR@` is resolved server-side, so it lands on
        /// whatever the default output is at open time.
        pub(super) fn open(route_id: &str) -> Result<Self, String> {
            let api = api().ok_or("libpulse-simple isn't available")?;
            let spec = SampleSpec {
                format: PA_SAMPLE_S16LE,
                rate: RATE,
                channels: CHANNELS as u8,
            };
            let attr = BufferAttr {
                maxlength: u32::MAX,
                tlength: u32::MAX,
                prebuf: u32::MAX,
                minreq: u32::MAX,
                // Small fragments keep the route's added latency at ~one
                // read instead of the server's roomy record default.
                fragsize: (SAMPLES_PER_READ * CHANNELS * std::mem::size_of::<i16>()) as u32,
            };
            let app = CString::new("AllMyStuff").expect("static name");
            let stream_name =
                CString::new(route_id).map_err(|_| "route id contains a NUL".to_string())?;
            let device = CString::new("@DEFAULT_MONITOR@").expect("static name");
            let mut error: c_int = 0;
            let stream = unsafe {
                (api.new)(
                    std::ptr::null(),
                    app.as_ptr(),
                    PA_STREAM_RECORD,
                    device.as_ptr(),
                    stream_name.as_ptr(),
                    &spec,
                    std::ptr::null(),
                    &attr,
                    &mut error,
                )
            };
            if stream.is_null() {
                return Err(format!("pa_simple_new failed (error {error})"));
            }
            Ok(Monitor { api, stream })
        }

        /// Blocking read loop: hand each ~20 ms of stereo PCM to `on_frame`
        /// until `stop` flips. A monitor source streams continuously
        /// (zeros while the desktop is silent), so the stop flag is seen
        /// within one read.
        pub(super) fn pump(
            &self,
            stop: &AtomicBool,
            on_frame: &(impl Fn(Vec<i16>, u32, u16) + Send + Sync),
        ) -> Result<(), String> {
            let mut buf = vec![0i16; SAMPLES_PER_READ * CHANNELS];
            while !stop.load(Ordering::SeqCst) {
                let mut error: c_int = 0;
                let rc = unsafe {
                    (self.api.read)(
                        self.stream,
                        buf.as_mut_ptr().cast(),
                        buf.len() * std::mem::size_of::<i16>(),
                        &mut error,
                    )
                };
                if rc < 0 {
                    return Err(format!("pa_simple_read failed (error {error})"));
                }
                on_frame(buf.clone(), RATE, CHANNELS as u16);
            }
            Ok(())
        }
    }

    impl Drop for Monitor {
        fn drop(&mut self) {
            unsafe { (self.api.free)(self.stream) };
        }
    }
}

// ---- playback ---------------------------------------------------------

fn run_playback(
    stop: &AtomicBool,
    route_id: &str,
    ring: Arc<Mutex<VecDeque<i16>>>,
    out_rate: Arc<AtomicU32>,
    fed: Arc<AtomicU64>,
    runtime: Arc<PlaybackRuntime>,
) -> Result<(), String> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| "no default output device".to_string())?;
    let supported = device.default_output_config().map_err(|e| e.to_string())?;
    let device_rate = supported.sample_rate().0;
    out_rate.store(device_rate, Ordering::Relaxed);
    let channels = supported.channels() as usize;
    let config: cpal::StreamConfig = supported.config();
    let fmt = supported.sample_format();
    tracing::info!(
        "audio playback for {route_id}: device \"{}\", {} Hz, {channels} ch, {fmt:?}",
        device.name().unwrap_or_else(|_| "unknown".into()),
        supported.sample_rate().0,
    );
    let err = |e| tracing::warn!("output stream error: {e}");

    // The ring is interleaved stereo. Playback primes to the adaptive target,
    // maps L/R to the device layout, and falls back to silence on starvation.
    // One starvation callback is one underrun episode; `note_underrun` then
    // closes the gate until the enlarged target has refilled.
    macro_rules! fill {
        ($data:expr, $conv:expr) => {{
            let mut guard = ring.lock();
            let target_frames = (device_rate as usize / 1_000)
                * runtime.target_depth_ms.load(Ordering::Relaxed) as usize;
            if !runtime.primed.load(Ordering::Acquire) && guard.len() / 2 >= target_frames.max(1) {
                runtime.primed.store(true, Ordering::Release);
            }

            let playing = runtime.primed.load(Ordering::Acquire);
            let mut missing_frames = 0u64;
            for frame in $data.chunks_mut(channels) {
                let (left, right) = if playing && guard.len() >= 2 {
                    (
                        guard.pop_front().unwrap_or(0),
                        guard.pop_front().unwrap_or(0),
                    )
                } else {
                    if playing {
                        missing_frames += 1;
                    }
                    (0, 0)
                };
                let mono = ((left as i32 + right as i32) / 2) as i16;
                for (channel, slot) in frame.iter_mut().enumerate() {
                    let sample = match (channels, channel) {
                        (1, _) => mono,
                        (_, 0) => left,
                        (_, 1) => right,
                        _ => mono,
                    };
                    *slot = $conv(sample);
                }
            }
            if missing_frames > 0 {
                runtime.note_underrun(missing_frames);
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

/// Convert interleaved device PCM to a stable mono or stereo shape. Stereo
/// keeps the first native L/R pair; mono sources are duplicated.
fn convert_channels(samples: &[i16], input_channels: u16, output_channels: u16) -> Vec<i16> {
    let input_channels = input_channels.max(1) as usize;
    let output_channels = output_channels.clamp(1, 2) as usize;
    if input_channels == output_channels {
        return samples.to_vec();
    }

    let mut out = Vec::with_capacity(samples.len() / input_channels * output_channels);
    for frame in samples.chunks(input_channels) {
        if output_channels == 1 {
            let mono =
                frame.iter().map(|&sample| sample as i32).sum::<i32>() / frame.len().max(1) as i32;
            out.push(mono as i16);
        } else {
            let left = frame.first().copied().unwrap_or(0);
            let right = frame.get(1).copied().unwrap_or(left);
            out.extend_from_slice(&[left, right]);
        }
    }
    out
}

/// A bounded-memory linear resampler for packetized PCM.
///
/// The previous implementation rounded `input_frames * ratio` independently
/// for every packet. At 48 kHz -> 44.1 kHz, a 5 ms packet contains 240 input
/// frames and 220.5 output frames, so that rounding discarded half a frame on
/// every call. This resampler instead places output samples on one continuous
/// rational timeline and retains only the preceding interleaved frame.
#[derive(Debug, Default)]
struct StreamingResampler {
    from_rate: u32,
    to_rate: u32,
    channels: u16,
    /// Distance from the preceding input frame to the next output point, in
    /// units whose denominator is `to_rate`. It remains <= `from_rate`, so
    /// the clock is exact without ever-growing counters.
    phase_numerator: u64,
    previous: Vec<i16>,
}

impl StreamingResampler {
    fn new() -> Self {
        Self::default()
    }

    fn reset(&mut self) {
        self.from_rate = 0;
        self.to_rate = 0;
        self.channels = 0;
        self.phase_numerator = 0;
        self.previous.clear();
    }

    fn process(
        &mut self,
        samples: &[i16],
        from_rate: u32,
        to_rate: u32,
        channels: u16,
    ) -> Vec<i16> {
        if samples.is_empty() {
            return Vec::new();
        }
        if from_rate == 0 || to_rate == 0 || from_rate == to_rate {
            self.reset();
            return samples.to_vec();
        }

        let channels = channels.max(1);
        let channel_count = channels as usize;
        let input_frames = samples.len() / channel_count;
        if input_frames == 0 {
            return Vec::new();
        }
        if self.from_rate != from_rate || self.to_rate != to_rate || self.channels != channels {
            self.reset();
            self.from_rate = from_rate;
            self.to_rate = to_rate;
            self.channels = channels;
            self.previous.reserve(channel_count);
        }

        let estimated_frames = ((input_frames as u128 * u128::from(to_rate))
            / u128::from(from_rate))
        .saturating_add(2)
        .min(usize::MAX as u128) as usize;
        let mut out = Vec::with_capacity(estimated_frames.saturating_mul(channel_count));

        for frame in samples[..input_frames * channel_count].chunks_exact(channel_count) {
            if self.previous.is_empty() {
                // Anchor both timelines at the first real input sample. The
                // causal converter then carries a fixed sub-sample latency,
                // never a packet-by-packet rate error.
                out.extend_from_slice(frame);
                self.previous.extend_from_slice(frame);
                self.phase_numerator = u64::from(from_rate);
                continue;
            }

            let segment = u64::from(to_rate);
            while self.phase_numerator <= segment {
                let fraction = self.phase_numerator as f64 / to_rate as f64;
                for (previous, current) in self.previous.iter().zip(frame.iter()) {
                    let a = f64::from(*previous);
                    let b = f64::from(*current);
                    out.push((a + (b - a) * fraction) as i16);
                }
                self.phase_numerator += u64::from(from_rate);
            }
            self.phase_numerator -= segment;
            self.previous.copy_from_slice(frame);
        }
        out
    }
}

/// One-shot linear resampler used by conversion-shape unit tests. Production
/// packet streams use [`StreamingResampler`] so fractional phase is retained.
#[cfg(test)]
pub(crate) fn resample_interleaved(
    samples: &[i16],
    from_rate: u32,
    to_rate: u32,
    channels: u16,
) -> Vec<i16> {
    if samples.is_empty() || from_rate == 0 || to_rate == 0 || from_rate == to_rate {
        return samples.to_vec();
    }
    let channels = channels.max(1) as usize;
    let input_frames = samples.len() / channels;
    if input_frames == 0 {
        return Vec::new();
    }
    let ratio = to_rate as f64 / from_rate as f64;
    let output_frames = ((input_frames as f64) * ratio) as usize;
    let mut out = Vec::with_capacity(output_frames * channels);
    for frame_index in 0..output_frames {
        let source = frame_index as f64 / ratio;
        let frame_0 = source.floor() as usize;
        let fraction = source - frame_0 as f64;
        let frame_1 = (frame_0 + 1).min(input_frames - 1);
        for channel in 0..channels {
            let a = samples[frame_0.min(input_frames - 1) * channels + channel] as f64;
            let b = samples[frame_1 * channels + channel] as f64;
            out.push((a + (b - a) * fraction) as i16);
        }
    }
    out
}

/// Linear-interpolating resampler from `from_rate` to `to_rate` (mono).
#[cfg(test)]
pub(crate) fn resample_linear(samples: &[i16], from_rate: u32, to_rate: u32) -> Vec<i16> {
    resample_interleaved(samples, from_rate, to_rate, 1)
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
    fn stereo_conversion_and_resampling_preserve_lr() {
        assert_eq!(
            convert_channels(&[10, 20, 30], 1, 2),
            vec![10, 10, 20, 20, 30, 30]
        );
        assert_eq!(
            convert_channels(&[10, -10, 20, -20], 2, 2),
            vec![10, -10, 20, -20]
        );
        assert_eq!(
            convert_channels(&[10, 20, 99, 30, 40, 88], 3, 2),
            vec![10, 20, 30, 40]
        );

        let up = resample_interleaved(&[0, 1_000, 100, 1_100], 24_000, 48_000, 2);
        assert_eq!(up.len(), 8);
        for frame in up.chunks_exact(2) {
            assert_eq!(frame[1] - frame[0], 1_000, "channels must not cross-mix");
        }
    }

    #[test]
    fn streaming_resampler_preserves_fractional_clock_and_packet_boundaries() {
        // Ten seconds of 5 ms 48 kHz callbacks. A stateless floor(220.5)
        // converter produces only 440,000 frames; the continuous clock must
        // produce the full 441,000 (within its fixed one-sample causal delay).
        let down_chunk = vec![7i16; 240 * 2];
        let mut down = StreamingResampler::new();
        let mut down_frames = 0usize;
        for _ in 0..2_000 {
            down_frames += down.process(&down_chunk, 48_000, 44_100, 2).len() / 2;
        }
        assert!(down_frames.abs_diff(441_000) <= 1, "{down_frames}");

        // The reverse direction receives alternating 220/221-frame 5 ms
        // buffers. Its long-run 48 kHz count likewise stays within the one
        // causal boundary sample instead of accumulating packet rounding.
        let mut up = StreamingResampler::new();
        let mut up_frames = 0usize;
        for packet in 0..2_000 {
            let frames = if packet % 2 == 0 { 220 } else { 221 };
            let chunk = vec![9i16; frames * 2];
            up_frames += up.process(&chunk, 44_100, 48_000, 2).len() / 2;
        }
        assert!(up_frames.abs_diff(480_000) <= 1, "{up_frames}");

        // Processing the same stream as irregular packets must be bit-for-bit
        // identical to processing one contiguous buffer: interpolation never
        // clamps at or restarts on packet edges.
        let input: Vec<i16> = (0..4_800)
            .flat_map(|frame| {
                let left = (frame % 10_000) as i16;
                [left, left.saturating_add(1_000)]
            })
            .collect();
        let mut whole_resampler = StreamingResampler::new();
        let whole = whole_resampler.process(&input, 48_000, 44_100, 2);
        let mut packet_resampler = StreamingResampler::new();
        let mut packetized = Vec::new();
        let mut offset_frames = 0usize;
        for frames in [137usize, 241, 509, 83, 997, 1_201, 1_632] {
            let end = offset_frames + frames;
            packetized.extend(packet_resampler.process(
                &input[offset_frames * 2..end * 2],
                48_000,
                44_100,
                2,
            ));
            offset_frames = end;
        }
        assert_eq!(offset_frames, 4_800);
        assert_eq!(packetized, whole);
    }

    #[test]
    fn audio_profile_contracts_are_exact() {
        let expected = [
            (AudioProfile::Reach, 48_000, 20, true, 60),
            (AudioProfile::Balanced, 96_000, 10, true, 40),
            (AudioProfile::Game, 128_000, 5, false, 20),
            (AudioProfile::Studio, 192_000, 10, false, 40),
        ];
        for (mode, bitrate, packet_ms, fec, jitter_ms) in expected {
            let profile = mode.opus();
            assert_eq!(profile.bitrate_bps, bitrate);
            assert_eq!(profile.frame_duration_ms, packet_ms);
            assert_eq!(profile.frame_duration_us(), packet_ms as u64 * 1_000);
            assert_eq!(profile.inband_fec, fec);
            assert_eq!(profile.jitter_target_ms, jitter_ms);
        }
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
    fn opus_stream_frames_and_survives_a_roundtrip() {
        // Arbitrary capture cadence in (here: 30 ms buffers at 24 kHz),
        // canonical 20 ms Opus packets out — and what was encoded
        // decodes again, which is the whole contract with the far side.
        let mut stream = OpusStream::new().expect("encoder");
        let mut packets = Vec::new();
        // 3 × 30 ms at 24 kHz = 90 ms → resampled to 48 kHz → 4 full
        // frames (80 ms) plus a 10 ms remainder held in the buffer.
        let buf: Vec<i16> = (0..720).map(|i| ((i * 37) % 1000) as i16).collect();
        for _ in 0..3 {
            stream.push(&buf, 24_000, |pkt| packets.push(pkt));
        }
        assert_eq!(packets.len(), 4, "90 ms yields 4 complete frames");
        assert!(
            stream.buf.len() < OPUS_FRAME_SAMPLES,
            "remainder stays buffered"
        );

        let mut dec = opus::Decoder::new(OPUS_RATE, opus::Channels::Mono).expect("decoder");
        let mut out = vec![0i16; OPUS_FRAME_SAMPLES * 6];
        let n = dec.decode(&packets[0], &mut out, false).expect("decode");
        assert_eq!(n, OPUS_FRAME_SAMPLES, "one packet = one 20 ms frame");
    }

    #[test]
    fn policy_opus_streams_are_stereo_and_use_mode_packet_sizes() {
        for mode in [
            AudioProfile::Reach,
            AudioProfile::Balanced,
            AudioProfile::Game,
            AudioProfile::Studio,
        ] {
            let mut stream = OpusStream::with_profile(mode).expect("stereo encoder");
            let profile = stream.profile();
            assert_eq!(stream.channels(), 2);
            assert_eq!(
                stream.enc.get_bitrate().unwrap(),
                opus::Bitrate::Bits(profile.bitrate_bps as i32)
            );
            assert_eq!(stream.enc.get_inband_fec().unwrap(), profile.inband_fec);

            let per_channel = profile.frame_samples_per_channel();
            let mut pcm = Vec::with_capacity(per_channel * 2);
            for i in 0..per_channel {
                pcm.push((i % 2_000) as i16);
                pcm.push(-((i % 2_000) as i16));
            }
            let mut packets = Vec::new();
            stream.push_interleaved(&pcm, OPUS_RATE, 2, |packet| packets.push(packet));
            assert_eq!(packets.len(), 1, "one exact mode frame emits one packet");

            let mut decoder =
                opus::Decoder::new(OPUS_RATE, opus::Channels::Stereo).expect("stereo decoder");
            let mut decoded = vec![0i16; per_channel * 2 * 6];
            let frames = decoder
                .decode(&packets[0], &mut decoded, false)
                .expect("decode policy packet");
            assert_eq!(frames, per_channel);
        }
    }

    #[test]
    fn opus_receiver_uses_inband_fec_for_one_missing_packet() {
        let mut encoder = OpusStream::with_profile(AudioProfile::Reach).expect("encoder");
        let profile = encoder.profile();
        let frames = profile.frame_samples_per_channel();
        let mut packets = Vec::new();
        for packet_index in 0..3 {
            let mut pcm = Vec::with_capacity(frames * 2);
            for i in 0..frames {
                let sample = (((packet_index * frames + i) % 4_000) as i16) - 2_000;
                pcm.extend_from_slice(&[sample, -sample]);
            }
            encoder.push_interleaved(&pcm, OPUS_RATE, 2, |packet| packets.push(packet));
        }
        assert_eq!(packets.len(), 3);

        let mut receiver = OpusReceiver::new(AudioProfile::Reach).expect("receiver");
        let step = frames as u32;
        let first = receiver.decode(1_000, &packets[0]).expect("first");
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].kind, OpusDecodeKind::Normal);

        // Packet 1 is omitted. Packet 2's RTP timestamp exposes one missing
        // duration, so its inband copy is decoded before the live packet.
        let recovered = receiver
            .decode(1_000u32.wrapping_add(step * 2), &packets[2])
            .expect("recover and decode current");
        assert_eq!(recovered.len(), 2);
        assert_eq!(recovered[0].kind, OpusDecodeKind::Fec);
        assert_eq!(recovered[1].kind, OpusDecodeKind::Normal);
        assert_eq!(recovered[0].seq, 1);
        assert_eq!(recovered[1].seq, 2);
        assert_eq!(recovered[0].pcm.len(), frames * 2);
        assert!(recovered[1].media_timestamp_us > recovered[0].media_timestamp_us);
    }

    #[test]
    fn opus_receiver_distinguishes_valid_zero_wrap_from_legacy_absence() {
        let mut encoder = OpusStream::with_profile(AudioProfile::Reach).expect("encoder");
        let profile = encoder.profile();
        let frames = profile.frame_samples_per_channel();
        let mut packets = Vec::new();
        for packet_index in 0..4 {
            let mut pcm = Vec::with_capacity(frames * 2);
            for i in 0..frames {
                let sample = (((packet_index * frames + i) % 4_000) as i16) - 2_000;
                pcm.extend_from_slice(&[sample, -sample]);
            }
            encoder.push_interleaved(&pcm, OPUS_RATE, 2, |packet| packets.push(packet));
        }
        let step = frames as u32;

        // A stream is allowed to start at RTP zero. The following step proves
        // that zero was real, and a loss immediately after zero is recovered.
        let mut zero_start = OpusReceiver::new(AudioProfile::Reach).expect("receiver");
        let first = zero_start.decode(0, &packets[0]).expect("zero start");
        let recovered = zero_start
            .decode(step.wrapping_mul(2), &packets[2])
            .expect("recover after zero");
        assert_eq!(first[0].media_timestamp_us, 0);
        assert_eq!(recovered.len(), 2);
        assert_eq!(recovered[0].kind, OpusDecodeKind::Fec);
        assert_eq!(
            zero_start.rtp_timestamps,
            RtpTimestampState::Present(step * 2)
        );

        // Once timestamped, zero is the legitimate wrap point, not an absent
        // sentinel. The unwrapped media clock remains monotonic across it.
        let mut wrapped = OpusReceiver::new(AudioProfile::Reach).expect("receiver");
        let before = wrapped
            .decode(0u32.wrapping_sub(step), &packets[0])
            .expect("before wrap");
        let after = wrapped.decode(0, &packets[1]).expect("at wrap");
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].kind, OpusDecodeKind::Normal);
        assert_eq!(wrapped.rtp_timestamps, RtpTimestampState::Present(0));
        assert_eq!(
            after[0]
                .media_timestamp_us
                .saturating_sub(before[0].media_timestamp_us),
            profile.frame_duration_us()
        );

        // Old base64 daemons omit the timestamp and therefore deliver zero
        // forever. Two zeros select duration-only fallback without recovery
        // frames, resets, or a frozen media clock.
        let mut legacy = OpusReceiver::new(AudioProfile::Reach).expect("receiver");
        let mut timestamps = Vec::new();
        for packet in &packets[..3] {
            let decoded = legacy.decode(0, packet).expect("legacy packet");
            assert_eq!(decoded.len(), 1);
            assert_eq!(decoded[0].kind, OpusDecodeKind::Normal);
            timestamps.push(decoded[0].media_timestamp_us);
        }
        assert_eq!(legacy.rtp_timestamps, RtpTimestampState::Absent);
        assert_eq!(timestamps, vec![0, 20_000, 40_000]);
    }

    #[test]
    fn opus_receiver_concealment_matches_each_profile_packet_duration() {
        for mode in [
            AudioProfile::Reach,
            AudioProfile::Balanced,
            AudioProfile::Game,
            AudioProfile::Studio,
        ] {
            let mut encoder = OpusStream::with_profile(mode).expect("encoder");
            let profile = encoder.profile();
            let frames = profile.frame_samples_per_channel();
            let mut packets = Vec::new();
            for packet_index in 0..4 {
                let mut pcm = Vec::with_capacity(frames * 2);
                for i in 0..frames {
                    let sample = (((packet_index * frames + i) % 4_000) as i16) - 2_000;
                    pcm.extend_from_slice(&[sample, -sample]);
                }
                encoder.push_interleaved(&pcm, OPUS_RATE, 2, |packet| packets.push(packet));
            }
            assert_eq!(packets.len(), 4);

            let mut receiver = OpusReceiver::new(mode).expect("receiver");
            let base = 10_000u32;
            let step = frames as u32;
            receiver.decode(base, &packets[0]).expect("first packet");

            // Omit two packets. FEC modes should emit one exact-duration PLC
            // frame plus one exact-duration FEC frame; non-FEC modes should
            // emit two exact-duration PLC frames. The live packet follows.
            let recovered = receiver
                .decode(base.wrapping_add(step * 3), &packets[3])
                .expect("recover two missing packets");
            assert_eq!(recovered.len(), 3, "{mode:?}");
            assert_eq!(recovered[0].kind, OpusDecodeKind::Plc, "{mode:?}");
            assert_eq!(
                recovered[1].kind,
                if profile.inband_fec {
                    OpusDecodeKind::Fec
                } else {
                    OpusDecodeKind::Plc
                },
                "{mode:?}"
            );
            assert_eq!(recovered[2].kind, OpusDecodeKind::Normal, "{mode:?}");
            for concealed in &recovered[..2] {
                assert_eq!(concealed.pcm.len(), frames * 2, "{mode:?}");
            }
            assert_eq!(recovered[2].pcm.len(), frames * 2, "{mode:?}");
            for pair in recovered.windows(2) {
                assert_eq!(
                    pair[1]
                        .media_timestamp_us
                        .saturating_sub(pair[0].media_timestamp_us),
                    profile.frame_duration_us(),
                    "{mode:?}"
                );
            }
        }
    }

    #[test]
    fn jitter_target_rises_fast_and_falls_slowly() {
        let runtime = PlaybackRuntime::new(20);
        let mut jitter = AdaptiveJitter::new();
        jitter.note_arrival(0, Some(0), 5_000, 0, &runtime);
        jitter.note_arrival(1, Some(5_000), 5_000, 5_000, &runtime);
        assert_eq!(runtime.target_depth_ms.load(Ordering::Relaxed), 20);

        // A 20 ms transit-time jump raises the RFC-style estimator and the
        // four-sigma target immediately.
        jitter.note_arrival(2, Some(10_000), 5_000, 30_000, &runtime);
        let raised = runtime.target_depth_ms.load(Ordering::Relaxed);
        assert!(raised > 20);

        // Stable arrivals lower the target one millisecond per packet at most.
        let before_one_stable = runtime.target_depth_ms.load(Ordering::Relaxed);
        jitter.note_arrival(3, Some(15_000), 5_000, 35_000, &runtime);
        let after_one_stable = runtime.target_depth_ms.load(Ordering::Relaxed);
        assert!(before_one_stable.saturating_sub(after_one_stable) <= 1);
    }

    #[test]
    fn underruns_are_counted_once_and_raise_rebuffer_target() {
        let runtime = PlaybackRuntime::new(20);
        runtime.primed.store(true, Ordering::Relaxed);
        runtime.note_underrun(240);
        assert!(!runtime.primed.load(Ordering::Relaxed));
        assert_eq!(runtime.target_depth_ms.load(Ordering::Relaxed), 30);
        assert_eq!(runtime.underrun_events.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.underrun_frames.load(Ordering::Relaxed), 240);
        assert_eq!(
            runtime.pending_underrun_events.swap(0, Ordering::Relaxed),
            1
        );
        assert_eq!(runtime.pending_underrun_events.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn playback_ring_trims_back_to_the_target_depth() {
        // The ring is a jitter buffer: a burst beyond its hysteresis is
        // cut back to the mode target (oldest frames dropped) so audio
        // can't fall permanently behind the video stream. Install a route
        // without a device thread so the test is deterministic on hosts that
        // do have an active default output.
        let bridge = AudioBridge::new();
        let runtime = Arc::new(PlaybackRuntime::new(
            AudioProfile::Balanced.opus().jitter_target_ms,
        ));
        bridge.playbacks.lock().insert(
            "r".into(),
            RouteAudio {
                stop: Arc::new(AtomicBool::new(false)),
                thread: None,
                playback: Some(Playback {
                    ring: Arc::new(Mutex::new(VecDeque::new())),
                    out_rate: Arc::new(AtomicU32::new(48_000)),
                    fed: Arc::new(AtomicU64::new(0)),
                    runtime,
                    jitter: Mutex::new(AdaptiveJitter::new()),
                    resampler: Mutex::new(StreamingResampler::new()),
                    clock_origin: Instant::now(),
                    stats: Mutex::new(LevelStats::new()),
                }),
            },
        );
        // Default device rate 48 kHz; Balanced starts at 40 ms.
        let frame = AudioFrame::new("r", 0, 48_000, 1, vec![1i16; 4800]); // 100 ms
        for _ in 0..5 {
            bridge.feed("r", &frame);
        }
        let depth = {
            let routes = bridge.playbacks.lock();
            let pb = routes.get("r").and_then(|r| r.playback.as_ref()).unwrap();
            let stereo_samples = pb.ring.lock().len();
            stereo_samples / 2
        };
        // 500 ms went in; the trims must have pulled it back to target.
        assert_eq!(depth, (48_000 / 1000) * 40);
        let stats = bridge.receive_stats("r").unwrap();
        assert_eq!(stats.target_depth_ms, 40);
        assert_eq!(stats.buffered_depth_ms, 40);
        assert!(stats.trimmed_frames > 0);
        bridge.stop("r");
    }

    #[test]
    fn capture_and_playback_for_one_route_coexist() {
        // A loopback route owns both map entries and stopping must remove
        // both. Keep this bookkeeping test independent of physical WASAPI /
        // CoreAudio devices: opening native streams from a unit-test process
        // is nondeterministic and has crashed vendor drivers in CI.
        let bridge = AudioBridge::new();
        let route = || RouteAudio {
            stop: Arc::new(AtomicBool::new(false)),
            thread: None,
            playback: None,
        };
        bridge.captures.lock().insert("r".into(), route());
        bridge.playbacks.lock().insert("r".into(), route());
        assert!(bridge.captures.lock().contains_key("r"));
        assert!(bridge.playbacks.lock().contains_key("r"));
        bridge.stop("r");
        assert!(!bridge.is_running("r"));
    }
}
