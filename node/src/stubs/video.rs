//! No-op twin of [`crate::video`] for capture-less builds (`--no-default-features`,
//! i.e. iOS — see the `host` feature in `Cargo.toml`).
//!
//! Same public surface, no capture: starting a stream immediately reports
//! [`VideoStatusState::GrabFailed`] with an honest reason, so a viewer that
//! points a route at this node sees "can't capture here" instead of a black
//! rectangle with no explanation. Decode (the *viewer* side, `video_decode`)
//! is a separate ungated module — this node still watches other machines.

use std::sync::Arc;
use std::time::Instant;

use allmystuff_session::{VideoFrame, VideoStatusState};

/// Which transport a video-carrying route's stream encodes for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoMode {
    /// Standalone JPEG frames over the media channel.
    Mjpeg,
    /// H.264 access units for the mesh's RTP track lane.
    H264,
}

/// What a route's capture thread would point at, if this build had one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoSource {
    /// A monitor: `None` = the primary, `Some(id)` = an extra screen.
    Screen(Option<u32>),
    /// A camera, by inventory device id.
    Camera(String),
}

/// One capture tick's output, headed for the forwarder.
#[derive(Debug, Clone)]
pub enum VideoPacket {
    Jpeg(VideoFrame),
    H264 {
        /// Annex-B access unit.
        data: Vec<u8>,
        /// Capture-tick pacing for the RTP clock (1/fps).
        duration_us: u64,
    },
}

/// Whether per-stream dial-in stats log at info — mirrored so shared call
/// sites in `mesh.rs` compile; nothing here ever produces a stream to log.
pub(crate) fn stats_to_info() -> bool {
    false
}

/// How a video route's path to its viewer flows — mirrored from
/// [`crate::video::LinkClass`] so the shared LAN-gate call sites in
/// `mesh.rs` (`link_class_of`, `seed_peer_links`, `Tune { link, .. }`)
/// compile on a capture-less build. Inert here: nothing streams, so the
/// class never governs a dial.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LinkClass {
    /// The nominated candidate pair is host↔host — a direct local link.
    Lan,
    /// Reflexive or relayed — real internet between the ends.
    Wan,
    /// No nominated pair reported (yet). Conservative until known.
    #[default]
    Unknown,
}

/// One stream's viewer-requested overrides. Accepted and ignored — there is
/// no encoder to re-tune. `link` carries the LAN-gate class for surface
/// parity with the real [`crate::video::Tune`]; it governs nothing here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tune {
    pub max_edge: Option<u32>,
    pub bitrate: Option<u32>,
    pub fps: Option<u32>,
    pub link: LinkClass,
}

/// The host side of a capture-status report: state + optional OS error text.
pub type OnStatus = Arc<dyn Fn(VideoStatusState, Option<String>) + Send + Sync>;

/// The latest decode health a viewer reported for one of our outbound
/// streams. Still recorded — feedback arrives over the wire regardless of
/// whether this node can capture — so the sender-side logging keeps working
/// if a route ever *is* offered.
#[derive(Debug, Clone, Copy)]
pub struct RecvFeedback {
    pub recv_fps: u32,
    pub decode_fails: u32,
    pub queue_depth: u32,
    pub at: Instant,
}

/// Handle the mesh holds either way; capture starts fail loudly, everything
/// else is a shrug.
#[derive(Default)]
pub struct VideoBridge {
    feedback: parking_lot::Mutex<std::collections::HashMap<String, RecvFeedback>>,
}

impl VideoBridge {
    pub fn new() -> Self {
        Self::default()
    }

    /// No capture backend exists on this build: report the failure straight
    /// through `on_status` so the viewer's `vstat` line says why the stream
    /// is dark, and produce no packets.
    pub fn start_capture<F, S>(
        &self,
        route_id: String,
        _mode: VideoMode,
        _source: VideoSource,
        _tune: Tune,
        _on_packet: F,
        on_status: S,
    ) where
        F: Fn(VideoPacket) -> bool + Send + Sync + 'static,
        S: Fn(VideoStatusState, Option<String>) + Send + Sync + 'static,
    {
        tracing::info!("video capture for {route_id} unavailable: capture-less build");
        on_status(
            VideoStatusState::GrabFailed,
            Some("this device cannot capture its screen or camera".into()),
        );
    }

    pub fn force_idr(&self, _route_id: &str) {}

    /// No capture routes exist on this build, so there are never any ids to
    /// re-class. Present for surface parity with the real bridge (the LAN
    /// gate sweeps this in `refresh_peer_networks`).
    pub fn route_ids(&self) -> Vec<String> {
        Vec::new()
    }

    /// The LAN gate learning a route's link class — a no-op with nothing
    /// captured here. Returns `false` (no retune happened), the same
    /// contract as the real bridge on an unchanged/absent route.
    pub fn retune_link(&self, _route_id: &str, _link: LinkClass) -> bool {
        false
    }

    /// A viewer's Tune against a stream this build can't produce — accepted
    /// and ignored, matching the real bridge's signature.
    pub fn retune_dials(
        &self,
        _route_id: &str,
        _max_edge: Option<u32>,
        _bitrate: Option<u32>,
        _fps: Option<u32>,
    ) {
    }

    pub fn note_feedback(
        &self,
        route_id: &str,
        recv_fps: u32,
        decode_fails: u32,
        queue_depth: u32,
    ) {
        self.feedback.lock().insert(
            route_id.to_string(),
            RecvFeedback {
                recv_fps,
                decode_fails,
                queue_depth,
                at: Instant::now(),
            },
        );
    }

    #[allow(dead_code)]
    pub fn latest_feedback(&self, route_id: &str) -> Option<RecvFeedback> {
        self.feedback.lock().get(route_id).copied()
    }

    pub fn retune(&self, _route_id: &str, _tune: Tune) {}

    pub fn stop(&self, route_id: &str) {
        self.feedback.lock().remove(route_id);
    }
}

/// No monitors to enumerate — the capability list simply carries no
/// `screen:<id>` entries (and no primary `screen` either; the bridge layer
/// decides that from the scan, not from here).
pub fn extra_screens() -> Vec<allmystuff_bridge::ScreenSource> {
    Vec::new()
}
