//! No-op twin of [`crate::audio`] for capture-less builds (`--no-default-features`,
//! i.e. iOS — see the `host` feature in `Cargo.toml`).
//!
//! Same public surface, none of the machinery: no cpal, no capture threads,
//! no playback ring. Starting a capture logs and produces no frames (the
//! mesh's status plumbing reports the silence); feeding inbound audio drops
//! it. The Opus *lane constants* keep their real values — the decode path in
//! `mesh.rs` still runs on a capture-less node, it just has nowhere to play
//! yet.

use allmystuff_session::AudioFrame;

/// What a sourcing audio route records. Mirrors [`crate::audio`]'s enum so
/// route wiring in `mesh.rs` compiles unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureSource {
    /// The default input device — a scanned microphone capability.
    Mic,
    /// This machine's own playback (the synthetic `system-audio` capability).
    System,
}

/// The audio lane's clock: Opus always runs a 48 kHz RTP clock.
pub(crate) const OPUS_RATE: u32 = 48_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AudioProfile {
    Reach,
    Balanced,
    Game,
    Studio,
}

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
            Self::Reach => OpusProfile {
                bitrate_bps: 48_000,
                frame_duration_ms: 20,
                inband_fec: true,
                jitter_target_ms: 60,
            },
            Self::Balanced => OpusProfile {
                bitrate_bps: 96_000,
                frame_duration_ms: 10,
                inband_fec: true,
                jitter_target_ms: 40,
            },
            Self::Game => OpusProfile {
                bitrate_bps: 128_000,
                frame_duration_ms: 5,
                inband_fec: false,
                jitter_target_ms: 20,
            },
            Self::Studio => OpusProfile {
                bitrate_bps: 192_000,
                frame_duration_ms: 10,
                inband_fec: false,
                jitter_target_ms: 40,
            },
        }
    }
}

impl OpusProfile {
    pub(crate) const fn frame_duration_us(self) -> u64 {
        self.frame_duration_ms as u64 * 1_000
    }
}

/// Encode-side Opus shim. On a capture-less build there is nothing to
/// encode; `new` fails so the mesh takes its existing PCM-fallback branch
/// (which then also produces nothing, because capture never starts).
pub(crate) struct OpusStream {}

impl OpusStream {
    pub(crate) fn new() -> Result<Self, String> {
        Err("audio capture is not built into this node (no `host` feature)".into())
    }

    pub(crate) fn with_profile(_profile: AudioProfile) -> Result<Self, String> {
        Self::new()
    }

    #[allow(dead_code)]
    pub(crate) fn profile(&self) -> OpusProfile {
        AudioProfile::Balanced.opus()
    }

    #[allow(dead_code)]
    pub(crate) fn channels(&self) -> u16 {
        2
    }

    /// Never called — `new` never hands out an instance — but the signature
    /// keeps call sites compiling.
    #[allow(dead_code)]
    pub(crate) fn push(&mut self, _pcm: &[i16], _rate: u32, _emit: impl FnMut(Vec<u8>)) {}

    #[allow(dead_code)]
    pub(crate) fn push_interleaved(
        &mut self,
        _pcm: &[i16],
        _rate: u32,
        _channels: u16,
        _emit: impl FnMut(Vec<u8>),
    ) {
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpusDecodeKind {
    Normal,
    Fec,
    Plc,
}

#[derive(Debug)]
pub(crate) struct DecodedOpus {
    pub seq: u64,
    pub media_timestamp_us: u64,
    pub kind: OpusDecodeKind,
    pub pcm: Vec<i16>,
}

pub(crate) struct OpusReceiver {}

impl OpusReceiver {
    pub(crate) fn new(_profile: AudioProfile) -> Result<Self, String> {
        Err("audio playback is not built into this node (no `host` feature)".into())
    }

    pub(crate) fn set_profile(&mut self, _profile: AudioProfile) {}

    pub(crate) fn decode(
        &mut self,
        _rtp_timestamp: u32,
        _packet: &[u8],
    ) -> Result<Vec<DecodedOpus>, String> {
        Err("audio playback is not built into this node (no `host` feature)".into())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AudioReceiveFeedback {
    pub target_depth_ms: u32,
    pub buffered_depth_ms: u32,
    pub arrival_jitter_us: u64,
    pub underrun_events: u64,
    pub underrun_frames: u64,
}

/// Handle the mesh holds either way; every operation is a shrug.
#[derive(Default)]
pub struct AudioBridge {}

impl AudioBridge {
    pub fn new() -> Self {
        Self::default()
    }

    /// Capture never starts: no device, no thread, no frames. The route's
    /// viewer simply hears nothing (same as a desktop whose device open
    /// failed).
    pub fn start_capture<F>(&self, route_id: String, _source: CaptureSource, _on_frame: F)
    where
        F: Fn(Vec<i16>, u32) + Send + Sync + 'static,
    {
        tracing::info!("audio capture for {route_id} unavailable: capture-less build");
    }

    pub fn start_capture_interleaved<F>(
        &self,
        route_id: String,
        _source: CaptureSource,
        _on_frame: F,
    ) where
        F: Fn(Vec<i16>, u32, u16) + Send + Sync + 'static,
    {
        tracing::info!("audio capture for {route_id} unavailable: capture-less build");
    }

    /// Playback isn't wired on this build yet; inbound audio is decoded and
    /// dropped.
    pub fn start_playback(&self, route_id: String) {
        tracing::info!("audio playback for {route_id} unavailable: capture-less build");
    }

    pub(crate) fn start_playback_with_profile(&self, route_id: String, _profile: AudioProfile) {
        self.start_playback(route_id);
    }

    pub(crate) fn set_playback_profile(&self, _route_id: &str, _profile: AudioProfile) {}

    pub fn feed(&self, _route_id: &str, _frame: &AudioFrame) {}

    pub(crate) fn take_receive_feedback(&self, _route_id: &str) -> Option<AudioReceiveFeedback> {
        None
    }

    pub fn stop(&self, _route_id: &str) {}

    #[allow(dead_code)]
    pub fn stop_all(&self) {}

    #[allow(dead_code)]
    pub fn is_running(&self, _route_id: &str) -> bool {
        false
    }
}

#[allow(dead_code)]
pub(crate) fn resample_interleaved(
    samples: &[i16],
    _from_rate: u32,
    _to_rate: u32,
    _channels: u16,
) -> Vec<i16> {
    samples.to_vec()
}

#[allow(dead_code)]
pub(crate) fn resample_linear(samples: &[i16], _from_rate: u32, _to_rate: u32) -> Vec<i16> {
    samples.to_vec()
}
