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
/// 20 ms at 48 kHz — the canonical Opus frame.
#[allow(dead_code)]
pub(crate) const OPUS_FRAME_SAMPLES: usize = 960;
/// The same 20 ms, as the lane's RTP pacing value.
#[allow(dead_code)]
pub(crate) const OPUS_FRAME_US: u64 = 20_000;

/// Encode-side Opus shim. On a capture-less build there is nothing to
/// encode; `new` fails so the mesh takes its existing PCM-fallback branch
/// (which then also produces nothing, because capture never starts).
pub(crate) struct OpusStream {}

impl OpusStream {
    pub(crate) fn new() -> Result<Self, String> {
        Err("audio capture is not built into this node (no `host` feature)".into())
    }

    /// Never called — `new` never hands out an instance — but the signature
    /// keeps call sites compiling.
    #[allow(dead_code)]
    pub(crate) fn push(&mut self, _pcm: &[i16], _rate: u32, _emit: impl FnMut(Vec<u8>)) {}
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

    /// Playback isn't wired on this build yet; inbound audio is decoded and
    /// dropped.
    pub fn start_playback(&self, route_id: String) {
        tracing::info!("audio playback for {route_id} unavailable: capture-less build");
    }

    pub fn feed(&self, _route_id: &str, _frame: &AudioFrame) {}

    pub fn stop(&self, _route_id: &str) {}

    #[allow(dead_code)]
    pub fn stop_all(&self) {}

    #[allow(dead_code)]
    pub fn is_running(&self, _route_id: &str) -> bool {
        false
    }
}
