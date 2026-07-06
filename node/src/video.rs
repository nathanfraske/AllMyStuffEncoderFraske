//! The video media plane: capture of this machine's screens **and
//! cameras**, so an active display or camera route actually streams
//! pixels — H.264 on the mesh's track lane, MJPEG as the compatibility
//! floor (the piKVM transport: every frame a standalone JPEG, no codec
//! state to desync).
//!
//! Mirrors [`crate::audio::AudioBridge`]'s shape: each sourcing route runs
//! a dedicated thread that captures **what its source capability names**
//! (the synthetic `screen` is the primary monitor, `screen:<id>` one of
//! the others, a camera capability the OS camera it scanned — see
//! [`VideoSource`]), fits it to the transport's ceiling, encodes, and
//! hands the packet to a callback the mesh forwards. Everything from the
//! encoder down — transports, tuning, refresh asks, stats, the status
//! reports — is one pipeline; only the frame *source* differs, and camera
//! frames enter through the same pump the persistent screen sessions use
//! ([`crate::camera_capture`]).
//!
//! Capture prefers a **persistent session**: our own DXGI Output
//! Duplication on Windows (see [`crate::win_capture`] for why xcap's
//! recorder can't carry it), `xcap`'s `VideoRecorder` elsewhere (PipeWire
//! ScreenCast on Wayland, AVFoundation on macOS). The OS negotiates the
//! stream once per route and pushes frames, often only on damage. The
//! alternative — one `capture_image()` screenshot per tick — pays the
//! platform's full one-shot cost every frame (the Wayland portal literally
//! has the compositor write a PNG to disk per call), which is what made
//! v1's framerate so dire. The paced one-shot loop remains as the X11 path
//! (xcap's X11 "recorder" is that same screenshot in an unpaced hot loop)
//! and as the fallback wherever a session can't start (denied portal,
//! headless session, an output another session holds) — so the stream
//! degrades to v1 behaviour, never to nothing.
//!
//! The H.264 encoder budgets its bitrate from the monitor's true pixel
//! count and runs native resolution up to 4K by default; the edge, rate,
//! and bitrate are env-dialable for dial-in sessions (see the knobs by
//! [`target_fps`]).
//!
//! Two costs are skipped outright when they buy nothing: a frame whose
//! pixels match the previous one isn't re-encoded or re-sent (an idle
//! desktop costs one buffer compare per tick, with a periodic refresh so
//! late joiners aren't stranded), and when the link can't keep up the
//! bounded forwarder drops captures rather than queueing stale ones.
//!
//! Three failure modes the capture used to suffer silently are handled
//! here as part of the stream:
//!
//!  * **A sleeping display.** Hosting a route holds a keep-awake guard
//!    and nudges an already-asleep display at start (see [`crate::wake`])
//!    — damage-driven backends produce nothing from a dark screen, and a
//!    deep-sleeping DisplayPort monitor detaches from the desktop
//!    entirely (which is why a failed monitor lookup gets one retry
//!    after the nudge has had a beat to re-attach things).
//!  * **Wayland consent.** Route starts ride a portal session with a
//!    **restore token** ([`crate::wayland_capture`]): the compositor's
//!    share-picker is a once-per-machine event, and every start after it
//!    is silent — which is what an unattended host needs.
//!  * **Silence with no explanation.** Capture state changes travel to
//!    the viewer in-band (`vstat` media frames, [`StatusReporter`]):
//!    "waiting for consent", "display asleep", "no monitor", "grabs
//!    failing" — so the far end reads the real condition, never a
//!    wordless black stage.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use allmystuff_session::{VideoFrame, VideoStatusState};

use crate::wake;

/// Which transport a video-carrying route's stream encodes for — picked
/// by the mesh from the offer's `video` accepts (see `RouteControl::Offer`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoMode {
    /// Standalone JPEG frames over the media channel — the v1 transport
    /// and the universal fallback.
    Mjpeg,
    /// H.264 access units for the mesh's RTP track lane.
    H264,
}

/// What a route's capture thread points at — the one place a display
/// route and a camera route differ. Everything downstream (encoder,
/// transport, tune/refresh, status) is shared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoSource {
    /// A monitor: `None` = the primary (the synthetic `screen`
    /// capability), `Some(id)` = the monitor with that enumeration id
    /// (a `screen:<id>` capability — see [`extra_screens`]).
    Screen(Option<u32>),
    /// A camera, by the inventory device id its capability embeds
    /// (`cam:video0`) — resolved back to the OS device by
    /// [`crate::camera_capture`].
    Camera(String),
}

/// One capture tick's output, headed for the forwarder. (H.264 units
/// carry no key flag — the receiving daemon re-derives IDR-ness from the
/// NAL stream itself when it reassembles units off RTP.)
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

/// Pipeline counters for one outbound stream, logged every
/// [`STATS_EVERY`] — the dial-in line: effective fps out, where each
/// frame's milliseconds went (scale vs encode), payload bitrate, and the
/// three ways a tick can produce nothing (unchanged screen, downstream
/// drop, rate-control skip). Times accumulate; the log shows per-frame
/// averages over the window.
pub struct StreamStats {
    route_id: String,
    label: &'static str,
    since: Instant,
    sent: u32,
    keyframes: u32,
    static_skipped: u32,
    dropped: u32,
    bytes: u64,
    scale: Duration,
    encode: Duration,
    out_w: u32,
    out_h: u32,
}

impl StreamStats {
    fn new(route_id: &str, mode: VideoMode) -> Self {
        StreamStats {
            route_id: route_id.to_string(),
            label: match mode {
                VideoMode::H264 => "H.264",
                VideoMode::Mjpeg => "MJPEG",
            },
            since: Instant::now(),
            sent: 0,
            keyframes: 0,
            static_skipped: 0,
            dropped: 0,
            bytes: 0,
            scale: Duration::ZERO,
            encode: Duration::ZERO,
            out_w: 0,
            out_h: 0,
        }
    }

    fn maybe_log(&mut self) {
        let elapsed = self.since.elapsed();
        if elapsed < STATS_EVERY {
            return;
        }
        let secs = elapsed.as_secs_f64();
        let frames = self.sent.max(1) as f64;
        let line = format!(
            "video out {}: {:.1} fps {} {}×{} · {:.1} Mbps · scale {:.1}ms · encode {:.1}ms · {} key · {} static-skip · {} dropped",
            self.route_id,
            self.sent as f64 / secs,
            self.label,
            self.out_w,
            self.out_h,
            (self.bytes as f64 * 8.0) / secs / 1_000_000.0,
            self.scale.as_secs_f64() * 1000.0 / frames,
            self.encode.as_secs_f64() * 1000.0 / frames,
            self.keyframes,
            self.static_skipped,
            self.dropped,
        );
        if stats_to_info() {
            tracing::info!("{line}");
        } else {
            tracing::debug!("{line}");
        }
        self.since = Instant::now();
        self.sent = 0;
        self.keyframes = 0;
        self.static_skipped = 0;
        self.dropped = 0;
        self.bytes = 0;
        self.scale = Duration::ZERO;
        self.encode = Duration::ZERO;
    }
}

/// MJPEG ceiling on the longest frame edge. Defaults to **1920** so a 1080p
/// desktop streams at native HD — at 1280 a 1080p screen was downscaled to
/// 720p, too soft to read text. JPEG frames are chunked under the 64 KiB
/// data-channel ceiling, so a higher edge costs bandwidth, not correctness.
/// (MJPEG is the compatibility floor; the H.264 path carries the full
/// picture.) Override: `ALLMYSTUFF_MJPEG_MAX_EDGE`.
fn mjpeg_max_edge() -> u32 {
    static EDGE: std::sync::LazyLock<u32> =
        std::sync::LazyLock::new(|| env_u32("ALLMYSTUFF_MJPEG_MAX_EDGE", 1920).clamp(320, 3840));
    *EDGE
}
/// Mid-range JPEG quality — piKVM's default neighbourhood; text stays
/// legible, photos stay cheap.
const JPEG_QUALITY: u8 = 60;
/// An unchanged screen still re-sends one frame this often, so a viewer
/// that lost a frame (or joined a quiet stream) is never stranded on a
/// stale picture. Every tick in between costs one buffer compare.
const STATIC_REFRESH: Duration = Duration::from_secs(2);
/// Forced-IDR cadence floor (ms) — bounds how long a viewer that joined
/// mid-stream, lost an unrepaired packet, or rebuilt its decoder waits for a
/// clean decode entry. The cadence when a viewer reports trouble (or hasn't
/// reported), i.e. today's behaviour, and the default the adaptation starts
/// from. 2 s costs a fraction of the stream: cheap insurance next to a
/// multi-second freeze.
const IDR_MS_TIGHT: u64 = 2000;
/// The forced-IDR ceiling (ms) — the relaxed cadence for a viewer that's
/// keeping up cleanly. A keyframe is the costliest, most loss-exposed thing
/// on the wire (hundreds of packets, all-or-nothing); a healthy link doesn't
/// need one every 2 s, since the viewer asks for a fresh entry the instant it
/// actually glitches. Stretching cuts keyframe bursts → less loss exposure
/// and bandwidth, without slowing real recovery.
const IDR_MS_RELAXED: u64 = 8000;
/// Feedback older than this is treated as absent — a viewer that went quiet
/// (or whose link died) must not hold the cadence relaxed.
const FEEDBACK_FRESH: Duration = Duration::from_secs(6);

/// The forced-IDR interval (ms) the latest receiver feedback implies. The
/// conservative half of the adaptation: relax to [`IDR_MS_RELAXED`] only on
/// *confirmed* health (recent report, no decode failures, queue draining);
/// anything else — no feedback, stale feedback, any glitch — stays at the
/// [`IDR_MS_TIGHT`] floor, i.e. exactly today's behaviour.
fn adaptive_idr_ms(fb: Option<RecvFeedback>) -> u64 {
    match fb {
        Some(fb)
            if fb.at.elapsed() < FEEDBACK_FRESH && fb.decode_fails == 0 && fb.queue_depth <= 8 =>
        {
            IDR_MS_RELAXED
        }
        _ => IDR_MS_TIGHT,
    }
}
/// How often each stream logs its pipeline counters — the dial-in line:
/// effective fps, where the per-frame milliseconds go, and the bitrate.
const STATS_EVERY: Duration = Duration::from_secs(5);

// The performance dials, each overridable for dial-in sessions without a
// rebuild (read once per process). Defaults aim at "the screen, full
// fidelity": H.264 carries up to a native 4K frame at a rate budgeted
// from its true pixel count.

/// Capture cadence to aim for — a ceiling, not a promise. Session capture
/// sustains it (damage-driven backends produce less on quiet screens);
/// the one-shot fallback runs at whatever the platform's screenshot path
/// allows. Override: `ALLMYSTUFF_VIDEO_FPS`.
pub(crate) fn target_fps() -> u32 {
    // Default 60 — this is a Parsec-tier 4K60 stream; 30 made fast motion look
    // choppy. It's a ceiling, not a promise (damage-driven backends produce
    // less on quiet screens), and a constrained/WAN link can dial it back with
    // ALLMYSTUFF_VIDEO_FPS until link-adaptive rate control lands.
    static FPS: std::sync::LazyLock<u32> =
        std::sync::LazyLock::new(|| env_u32("ALLMYSTUFF_VIDEO_FPS", 60).clamp(1, 120));
    *FPS
}

/// H.264 ceiling on the longest edge. 3840 means "native up to 4K" — no
/// downscale on anything up to a UHD monitor (openh264's own hard limit
/// is 3840×2160). Dimensions are forced even (4:2:0 chroma needs it).
/// Override: `ALLMYSTUFF_VIDEO_MAX_EDGE`.
fn h264_max_edge() -> u32 {
    static EDGE: std::sync::LazyLock<u32> =
        std::sync::LazyLock::new(|| env_u32("ALLMYSTUFF_VIDEO_MAX_EDGE", 3840).clamp(320, 3840));
    *EDGE
}

/// Target bitrate for one stream's encode, budgeted from what it actually
/// carries: ~0.16 bits per pixel per frame — the density 1080p30 was
/// tuned crisp at (10 Mbps) — clamped to 8–80 Mbps. The cap is 80 Mbps so a
/// 4K60 desktop (~80 Mbps at this density) and a 3440×1440@60 ultrawide
/// (~48 Mbps) reach their budget instead of being pinned at the old 40 Mbps
/// ceiling, which was *itself* the QP wall that blocked fast motion. Trivial on
/// a LAN, where direct peers live; link-adaptive rate (BWE) remains the
/// follow-up that makes the high cap safe on relayed/WAN paths.
/// Override (a fixed bps for every stream): `ALLMYSTUFF_VIDEO_BITRATE`.
fn h264_bitrate_for(w: u32, h: u32, fps: u32) -> u32 {
    static OVERRIDE: std::sync::LazyLock<u32> =
        std::sync::LazyLock::new(|| env_u32("ALLMYSTUFF_VIDEO_BITRATE", 0));
    if *OVERRIDE > 0 {
        return *OVERRIDE;
    }
    let px = u64::from(w) * u64::from(h);
    let bps = px * u64::from(fps) * 16 / 100;
    bps.clamp(8_000_000, 80_000_000) as u32
}

/// A `u32` env dial; `default` when unset or unparseable.
fn env_u32(key: &str, default: u32) -> u32 {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => match v.trim().parse() {
            Ok(n) => {
                tracing::info!("{key}={n} (override)");
                n
            }
            Err(_) => {
                tracing::warn!("{key}={v} isn't a number — using {default}");
                default
            }
        },
        _ => default,
    }
}

/// Whether the periodic pipeline stats print at info. Off by default —
/// steady-state runs stay quiet; set `ALLMYSTUFF_VIDEO_STATS=1` while
/// dialing performance in (without it the same lines land at debug, so
/// the `ALLMYSTUFF_GUI_LOG` filter can also reach them).
pub(crate) fn stats_to_info() -> bool {
    static ON: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        std::env::var("ALLMYSTUFF_VIDEO_STATS").is_ok_and(|v| !v.is_empty() && v != "0")
    });
    *ON
}

/// One stream's viewer-requested overrides (`RouteControl::Tune`): each
/// `None` falls back to the global dial / pixel budget. Applied by
/// restarting the route's capture, so a change costs one IDR's worth of
/// hiccup, never a desynced encoder.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tune {
    pub max_edge: Option<u32>,
    pub bitrate: Option<u32>,
    pub fps: Option<u32>,
}

impl Tune {
    fn fps(&self) -> u32 {
        self.fps.unwrap_or_else(target_fps).clamp(1, 120)
    }
    fn h264_edge(&self) -> u32 {
        self.max_edge.unwrap_or_else(h264_max_edge).clamp(320, 3840)
    }
    /// MJPEG honours the Res control the same way H.264 does — up to a 4K
    /// hard cap (chunked under the 64 KiB data channel) — so the pill moves
    /// both encodings. Untuned it defaults to HD ([`mjpeg_max_edge`], 1920)
    /// rather than 4K, since a JPEG frame is far heavier than an H.264 one.
    fn mjpeg_edge(&self) -> u32 {
        self.max_edge
            .unwrap_or_else(mjpeg_max_edge)
            .clamp(320, 3840)
    }
    /// MJPEG has no bitrate, so the Rate control maps to JPEG quality — the
    /// same pill means something on both encodings. `None` (auto) keeps the
    /// neutral [`JPEG_QUALITY`] default.
    fn jpeg_quality(&self) -> u8 {
        self.bitrate.map_or(JPEG_QUALITY, mjpeg_quality_for)
    }
}

/// Map a target bitrate (the console's Rate pill: 4–40 Mbps) to a JPEG
/// quality. Higher rate → crisper frames; the curve spans the pill range so
/// "Speed" reads softer and "Quality" reads sharp.
fn mjpeg_quality_for(bps: u32) -> u8 {
    if bps <= 5_000_000 {
        45
    } else if bps <= 10_000_000 {
        55
    } else if bps <= 18_000_000 {
        65
    } else if bps <= 30_000_000 {
        78
    } else {
        88
    }
}

/// The host side of a capture-status report: state + optional OS error
/// text, forwarded to the viewer as a `vstat` media frame by the mesh.
pub type OnStatus = Arc<dyn Fn(VideoStatusState, Option<String>) + Send + Sync>;

/// Dedupe wrapper over [`OnStatus`]: only state *transitions* hit the
/// wire, so a failing grab loop costs one frame, not one per tick.
struct StatusReporter {
    cb: OnStatus,
    last: Option<VideoStatusState>,
}

impl StatusReporter {
    fn new(cb: OnStatus) -> Self {
        StatusReporter { cb, last: None }
    }

    fn report(&mut self, state: VideoStatusState, detail: Option<String>) {
        if self.last == Some(state) {
            return;
        }
        self.last = Some(state);
        // The injector pulses the display awake on inbound clicks only
        // while a stream is dark — this transition is what tells it.
        wake::set_stream_dark(!matches!(state, VideoStatusState::Ok));
        (self.cb)(state, detail);
    }
}

struct RouteVideo {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// Everything needed to restart this capture on a retune.
    mode: VideoMode,
    source: VideoSource,
    on_packet: Arc<dyn Fn(VideoPacket) -> bool + Send + Sync>,
    on_status: OnStatus,
    /// One-shot "give the viewer a clean entry now" flag the encoder
    /// consumes (IDR for H.264, an immediate resend for MJPEG).
    refresh: Arc<AtomicBool>,
    /// The H.264 forced-IDR interval (ms), adapted from receiver feedback
    /// ([`note_feedback`] → [`adaptive_idr_ms`]); the encode thread reads it
    /// each frame. Default [`IDR_MS_TIGHT`] = today's fixed cadence.
    idr_ms: Arc<AtomicU64>,
    /// The tune this capture was started with — the controller compares the
    /// viewer's reported fps against its fps target.
    tune: Tune,
    /// The receiver-driven resolution cap (see [`AutoAdapt`]); fresh per
    /// capture, so a manual retune resets it.
    auto: Arc<AutoAdapt>,
}

impl Drop for RouteVideo {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The latest decode health a viewer reported for one of our outbound
/// streams (receiver → sender). Observability today; the signal the stream
/// adaptation — recovery cadence, then bitrate/res auto-scaling — reads next.
#[derive(Debug, Clone, Copy)]
pub struct RecvFeedback {
    pub recv_fps: u32,
    pub decode_fails: u32,
    pub queue_depth: u32,
    pub at: Instant,
}

// ---- receiver-driven auto-adaptation ---------------------------------
//
// The viewer reports its decode health every couple of seconds
// (`RouteControl::VideoFeedback`); [`adaptive_idr_ms`] already reads it for
// the recovery cadence. This is the other half that was marked "auto-scaling
// later": when the viewer demonstrably can't keep up — a 60 fps encode
// arriving at 0–6 fps, the exact "console shows a slideshow" failure — the
// sender steps its encode **resolution** down one rung, and steps back up
// once the viewer has been healthy for a sustained stretch (fast down, slow
// up, so the picture never oscillates).
//
// Resolution is the one dial that needs no capture restart: the encode path
// fits every frame to the effective edge per frame, and a changed fit
// re-inits the encoder through the existing budget-size rebuild (bitrate
// re-budgets from the smaller frame automatically; the next unit out is an
// IDR). A manual retune replaces the route and so resets the cap — the
// viewer's new picks change the conditions, and the controller re-learns.

/// Master switch for the receiver-driven resolution auto-adaptation. **Off for
/// now** — the manual Speed↔Quality slider (and the pills) are the quality
/// control, and auto-stepping fought them (it re-tuned under the same feedback
/// the user was reacting to). Deferred until it's a real, user-toggleable
/// setting that yields to a manual tune; revisit with the perf roadmap's slider
/// auto-traversal. The adaptive **IDR cadence** ([`adaptive_idr_ms`]) is a
/// separate, benign recovery lever and stays on regardless.
const AUTO_ADAPT_ENABLED: bool = false;

/// The auto-cap rungs, descending. `0` (uncapped) sits above the first.
const AUTO_EDGES: &[u32] = &[2560, 1920, 1280, 960];
/// Consecutive struggling reports (~2 s apart) before a step down.
const AUTO_BAD_STREAK: u32 = 3;
/// Consecutive healthy reports before a step back up — deliberately long:
/// stepping up too eagerly re-breaks the viewer and oscillates.
const AUTO_GOOD_STREAK: u32 = 20;
/// Settle time after any step before another *down* step — the encoder
/// rebuild and the viewer's pipeline refill need a beat to show up in the
/// feedback before it's evidence about the new rung.
const AUTO_DOWN_HOLD: Duration = Duration::from_secs(8);
/// Hold after any step before a step *up*.
const AUTO_UP_HOLD: Duration = Duration::from_secs(30);

/// One route's receiver-driven cap on the encode edge, shared between the
/// feedback path (writer, via [`AutoAdapt::observe`]) and the encode thread
/// (reader, via [`AutoAdapt::edge_cap`] each frame).
pub(crate) struct AutoAdapt {
    /// The current cap on the longest encode edge; `0` = uncapped.
    edge: AtomicU32,
    state: Mutex<AdaptState>,
}

#[derive(Default)]
struct AdaptState {
    bad: u32,
    good: u32,
    last_step: Option<Instant>,
}

impl AutoAdapt {
    fn new() -> Arc<Self> {
        Arc::new(AutoAdapt {
            edge: AtomicU32::new(0),
            state: Mutex::new(AdaptState::default()),
        })
    }

    /// The cap the encode path applies (min with the tuned edge), if any.
    fn edge_cap(&self) -> Option<u32> {
        if !AUTO_ADAPT_ENABLED {
            return None;
        }
        match self.edge.load(Ordering::Relaxed) {
            0 => None,
            e => Some(e),
        }
    }

    /// Fold one feedback report in; returns `Some((from, to))` when the cap
    /// stepped (0 = uncapped), for the caller to log. `now` is passed in so
    /// the streak/hold logic is unit-testable.
    fn observe(&self, fb: &RecvFeedback, fps_target: u32, now: Instant) -> Option<(u32, u32)> {
        if !AUTO_ADAPT_ENABLED {
            return None;
        }
        // Struggling: arriving at under a quarter of the encode rate (the
        // field failure was 0–6 fps of 60), or a queue backing far up.
        // Healthy: at least three quarters of it, decoding cleanly, queue
        // drained. The band between counts as neither — streaks reset, no
        // step. Decode failures alone are corruption, not overload — the
        // adaptive IDR cadence owns those.
        let bad = fb.recv_fps * 4 < fps_target || fb.queue_depth > 24;
        let good = fb.recv_fps * 4 >= fps_target * 3 && fb.decode_fails == 0 && fb.queue_depth <= 8;
        let mut st = self.state.lock();
        if bad {
            st.bad += 1;
            st.good = 0;
        } else if good {
            st.good += 1;
            st.bad = 0;
        } else {
            st.bad = 0;
            st.good = 0;
        }
        let cur = self.edge.load(Ordering::Relaxed);
        let held_for = |hold: Duration, st: &AdaptState| {
            st.last_step.is_none_or(|t| now.duration_since(t) >= hold)
        };
        if st.bad >= AUTO_BAD_STREAK && held_for(AUTO_DOWN_HOLD, &st) {
            // Next rung strictly below the current cap (uncapped → first).
            let Some(next) = AUTO_EDGES.iter().copied().find(|&e| cur == 0 || e < cur) else {
                st.bad = 0; // already at the floor — nothing left to give
                return None;
            };
            self.edge.store(next, Ordering::Relaxed);
            *st = AdaptState {
                last_step: Some(now),
                ..AdaptState::default()
            };
            return Some((cur, next));
        }
        if st.good >= AUTO_GOOD_STREAK && cur != 0 && held_for(AUTO_UP_HOLD, &st) {
            let next = match AUTO_EDGES.iter().position(|&e| e == cur) {
                Some(0) | None => 0,
                Some(i) => AUTO_EDGES[i - 1],
            };
            self.edge.store(next, Ordering::Relaxed);
            *st = AdaptState {
                last_step: Some(now),
                ..AdaptState::default()
            };
            return Some((cur, next));
        }
        None
    }
}

#[derive(Default)]
pub struct VideoBridge {
    routes: Mutex<HashMap<String, RouteVideo>>,
    /// Per-route receiver health, keyed by route id (see [`RecvFeedback`]).
    feedback: Mutex<HashMap<String, RecvFeedback>>,
}

impl VideoBridge {
    pub fn new() -> Self {
        Self::default()
    }

    /// Begin streaming `source` for `route_id`, encoding for `mode` — a
    /// monitor for a display route, a camera for a video one. `on_packet`
    /// is called with each encoded packet; it returns `false` when the
    /// packet was dropped downstream (backpressure), which is fine — the
    /// next capture simply carries the newer picture. `on_status` is
    /// called on capture-state transitions, for the viewer's benefit (see
    /// [`StatusReporter`]).
    pub fn start_capture<F, S>(
        &self,
        route_id: String,
        mode: VideoMode,
        source: VideoSource,
        on_packet: F,
        on_status: S,
    ) where
        F: Fn(VideoPacket) -> bool + Send + Sync + 'static,
        S: Fn(VideoStatusState, Option<String>) + Send + Sync + 'static,
    {
        // Exactly one capture pump per route. A duplicate `StartMedia` (the
        // daemon redelivers an Offer once per shared network) must not start a
        // second capture for a route that already has one — two backends bound
        // to one monitor, and with the release profile's `panic = "abort"` a
        // panic on either aborts the host. A genuine restart (a viewer's
        // retune) goes through `spawn_route` directly, which is allowed to
        // replace; only this entry point dedupes.
        if self.routes.lock().contains_key(&route_id) {
            tracing::debug!(
                "video capture already running for {route_id}; ignoring duplicate start"
            );
            return;
        }
        self.spawn_route(
            route_id,
            mode,
            source,
            Tune::default(),
            Arc::new(on_packet),
            Arc::new(on_status),
        );
    }

    fn spawn_route(
        &self,
        route_id: String,
        mode: VideoMode,
        source: VideoSource,
        tune: Tune,
        on_packet: Arc<dyn Fn(VideoPacket) -> bool + Send + Sync>,
        on_status: OnStatus,
    ) {
        let stop = Arc::new(AtomicBool::new(false));
        let refresh = Arc::new(AtomicBool::new(false));
        let idr_ms = Arc::new(AtomicU64::new(IDR_MS_TIGHT));
        let auto = AutoAdapt::new();
        let (stop_thread, refresh_thread, idr_thread, auto_thread, cb) = (
            stop.clone(),
            refresh.clone(),
            idr_ms.clone(),
            auto.clone(),
            on_packet.clone(),
        );
        let status_cb = on_status.clone();
        let id = route_id.clone();
        let src = source.clone();
        let thread = std::thread::spawn(move || {
            let what = match &src {
                VideoSource::Screen(_) => "screen",
                VideoSource::Camera(_) => "camera",
            };
            if let Err(e) = run_capture(
                &stop_thread,
                &refresh_thread,
                &idr_thread,
                &auto_thread,
                &id,
                mode,
                &src,
                tune,
                cb,
                status_cb,
            ) {
                tracing::warn!("{what} capture for {id} stopped: {e}");
            }
        });
        let displaced = {
            let mut routes = self.routes.lock();
            routes.insert(
                route_id,
                RouteVideo {
                    stop,
                    thread: Some(thread),
                    mode,
                    source,
                    on_packet,
                    on_status,
                    refresh,
                    idr_ms,
                    tune,
                    auto,
                },
            )
        };
        // Join any displaced capture thread (RouteVideo::drop) only after the
        // routes lock is released — joining a thread under the lock would
        // block every other route op, and on the async start path stall a
        // tokio worker. `start_capture` dedupes so this is normally `None`;
        // the explicit drop keeps the off-lock guarantee for any caller.
        drop(displaced);
    }

    /// Ask `route_id`'s encoder for a clean decode entry on its next
    /// frame — the viewer's decoder lost its place. No-op for a route
    /// this machine isn't streaming.
    pub fn force_idr(&self, route_id: &str) {
        if let Some(r) = self.routes.lock().get(route_id) {
            r.refresh.store(true, Ordering::SeqCst);
            tracing::debug!("refresh requested for {route_id}");
        }
    }

    /// Record the decode health a viewer reported for one of our streams
    /// (receiver → sender). Logged at info when the link looks unhealthy
    /// (decode failures, or the queue backing up) so a struggling stream is
    /// visible from the *sender's* logs, and stored for the stream
    /// adaptation to read.
    pub fn note_feedback(
        &self,
        route_id: &str,
        recv_fps: u32,
        decode_fails: u32,
        queue_depth: u32,
    ) {
        let fb = RecvFeedback {
            recv_fps,
            decode_fails,
            queue_depth,
            at: Instant::now(),
        };
        self.feedback.lock().insert(route_id.to_string(), fb);
        if decode_fails > 0 || queue_depth > 8 {
            tracing::info!(
                "video feedback {route_id}: viewer {recv_fps} fps · {decode_fails} decode-fail · queue {queue_depth}"
            );
        } else {
            tracing::debug!(
                "video feedback {route_id}: viewer {recv_fps} fps · queue {queue_depth}"
            );
        }
        // Adapt the H.264 forced-IDR cadence for this route: relax it when the
        // viewer is keeping up cleanly, tighten it the moment it isn't. The
        // encode thread reads the new value on its next frame.
        let want = adaptive_idr_ms(Some(fb));
        let adapt = {
            let routes = self.routes.lock();
            let Some(r) = routes.get(route_id) else {
                return;
            };
            let was = r.idr_ms.swap(want, Ordering::Relaxed);
            if was != want {
                tracing::debug!("video {route_id}: forced-IDR cadence {was}ms → {want}ms");
            }
            (r.auto.clone(), r.tune)
        };
        // The auto-scale half: step the encode resolution down when the
        // viewer demonstrably can't keep up, back up when it recovers
        // (see [`AutoAdapt`]). Run outside the routes lock — the observe
        // takes its own.
        let (auto, tune) = adapt;
        if let Some((from, to)) = auto.observe(&fb, tune.fps(), Instant::now()) {
            let name = |e: u32| {
                if e == 0 {
                    "native".to_string()
                } else {
                    format!("≤{e}")
                }
            };
            tracing::info!(
                "video auto-adapt {route_id}: viewer at {recv_fps} fps of {} — encode edge {} → {}                  (bitrate re-budgets at the new size; next unit is an IDR)",
                tune.fps(),
                name(from),
                name(to),
            );
        }
    }

    /// The most recent feedback a viewer reported for `route_id`, if any —
    /// the hook the stream adaptation (recovery cadence, auto-scale) reads.
    pub fn latest_feedback(&self, route_id: &str) -> Option<RecvFeedback> {
        self.feedback.lock().get(route_id).copied()
    }

    /// Restart `route_id`'s capture with the viewer's quality picks.
    pub fn retune(&self, route_id: &str, tune: Tune) {
        let Some(old) = self.routes.lock().remove(route_id) else {
            return;
        };
        let (mode, source, on_packet, on_status) = (
            old.mode,
            old.source.clone(),
            old.on_packet.clone(),
            old.on_status.clone(),
        );
        drop(old); // joins the old capture thread; its session releases
        tracing::info!(
            "route {route_id} retuned: edge {} · bitrate {} · fps {}",
            tune.max_edge.map_or("auto".into(), |v| v.to_string()),
            tune.bitrate
                .map_or("auto".into(), |v| format!("{:.1} Mbps", v as f64 / 1e6)),
            tune.fps.map_or("auto".into(), |v| v.to_string()),
        );
        self.spawn_route(
            route_id.to_string(),
            mode,
            source,
            tune,
            on_packet,
            on_status,
        );
    }

    pub fn stop(&self, route_id: &str) {
        // Bind the removed route so its Drop (the capture-thread join) runs
        // after the routes lock guard is released, never under it — an
        // unbound `remove(..);` would drop the RouteVideo (and join) while the
        // guard is still held (temporary drop order), blocking the lock on a
        // thread join.
        let removed = self.routes.lock().remove(route_id);
        self.feedback.lock().remove(route_id);
        drop(removed);
    }
}

#[allow(clippy::too_many_arguments)]
fn run_capture(
    stop: &AtomicBool,
    refresh: &Arc<AtomicBool>,
    idr_ms: &Arc<AtomicU64>,
    auto: &Arc<AutoAdapt>,
    route_id: &str,
    mode: VideoMode,
    source: &VideoSource,
    tune: Tune,
    on_packet: Arc<dyn Fn(VideoPacket) -> bool + Send + Sync>,
    on_status: OnStatus,
) -> Result<(), String> {
    let on_packet = &*on_packet;
    let mut reporter = StatusReporter::new(on_status);

    // A camera source skips the whole monitor story: open the device,
    // pump its frames into the same encoder. Open failures are told to
    // the viewer in-band, like every capture condition — "no camera"
    // when there's nothing to open, "camera failed" (with the OS text)
    // when there is but it won't stream: held by another app, or a
    // permission denial.
    if let VideoSource::Camera(device) = source {
        // A hosted camera is active use the OS can't see — hold the
        // machine awake like a hosted screen (without forcing the panel
        // on; the camera doesn't need the display lit).
        let _awake = wake::DisplayAwake::hold("hosting a camera stream");
        let fps = tune.fps();
        let mut encoder = make_encoder(route_id, mode, (0, 0), tune, refresh, idr_ms, auto)?;
        let mut stats = StreamStats::new(route_id, encoder.mode());
        let (session, frames) = match crate::camera_capture::open(device, fps) {
            Ok(open) => open,
            Err(e) => {
                let state = if e.contains("no camera") {
                    VideoStatusState::NoCamera
                } else {
                    VideoStatusState::CameraFailed
                };
                reporter.report(state, Some(e.clone()));
                return Err(e);
            }
        };
        let result = pump_frames_with_stall(
            stop,
            fps,
            &frames,
            |f: crate::camera_capture::RawFrame| (f.rgba, f.width, f.height),
            on_packet,
            &mut encoder,
            &mut stats,
            &mut reporter,
            VideoStatusState::CameraFailed,
            None,
        );
        drop(session);
        // A camera whose stream died mid-route (unplugged) ends with the
        // viewer told why, not just a frozen last frame.
        if let Err(e) = &result {
            if !stop.load(Ordering::SeqCst) {
                reporter.report(VideoStatusState::CameraFailed, Some(e.clone()));
            }
        }
        return result;
    }
    let monitor_id = match source {
        VideoSource::Screen(id) => *id,
        VideoSource::Camera(_) => unreachable!("camera handled above"),
    };

    // A hosted screen is active use the OS can't see: hold the display
    // awake for the stream's lifetime, and force one that's already dark
    // back on — neither capture path can grab from a sleeping panel.
    let _awake = wake::DisplayAwake::hold("hosting a screen stream");
    wake::force_display_on();
    // The monitor up front: its resolution budgets the encoder's bitrate.
    // One retry after a beat: the wake may still be re-attaching outputs
    // (a deep-sleeping DisplayPort monitor detaches from the desktop
    // entirely, and re-enumeration after wake takes a moment).
    let monitor = match select_monitor(monitor_id) {
        Ok(m) => m,
        Err(first) => {
            std::thread::sleep(Duration::from_millis(1500));
            if stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            match select_monitor(monitor_id) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(
                        "no monitor to capture for {route_id}: {e} (first attempt: {first})"
                    );
                    reporter.report(VideoStatusState::NoMonitor, Some(e.clone()));
                    return Err(e);
                }
            }
        }
    };
    let source_hint = (monitor.width().unwrap_or(0), monitor.height().unwrap_or(0));
    // A physically-rotated monitor: Windows DXGI hands over the raw scan-out and
    // reports its own rotation per frame (the orient pass rotates it upright);
    // every other backend delivers the upright presentation image (rotation 0).
    // `source_hint` is the monitor's reported size — it only pre-budgets the
    // encoder's starting bitrate; the first real frame locks the true size.
    let mut encoder = make_encoder(route_id, mode, source_hint, tune, refresh, idr_ms, auto)?;
    let mut stats = StreamStats::new(route_id, encoder.mode());
    let fps = tune.fps();

    // Windows: our own DXGI Output Duplication session — damage-driven,
    // per-monitor, releasable (see `win_capture` for why xcap's recorder
    // can't carry this). A failed start (output held elsewhere, RDP
    // session) falls through to the screenshot loop.
    #[cfg(windows)]
    {
        match monitor.id() {
            Ok(mid) => match crate::win_capture::start(mid) {
                Ok((session, frames)) => {
                    tracing::info!("DXGI duplication started for {route_id} (monitor {mid:#x})");
                    let result = pump_frames(
                        stop,
                        fps,
                        &frames,
                        move |f: crate::win_capture::RawFrame| {
                            // DXGI is the only raw, unrotated scan-out; its own
                            // DXGI_OUTDUPL_DESC.Rotation (carried on the frame)
                            // is authoritative for making it upright.
                            orient_to_monitor(f.rgba, f.width, f.height, f.rotation_deg)
                        },
                        on_packet,
                        &mut encoder,
                        &mut stats,
                        &mut reporter,
                    );
                    drop(session);
                    match result {
                        Ok(()) => return Ok(()),
                        Err(e) => {
                            if stop.load(Ordering::SeqCst) {
                                return Ok(());
                            }
                            tracing::warn!(
                                "DXGI duplication for {route_id} ended ({e}); \
                                 falling back to per-frame screenshots"
                            );
                        }
                    }
                }
                Err(e) => tracing::warn!(
                    "DXGI duplication for {route_id} unavailable ({e}); \
                     falling back to per-frame screenshots"
                ),
            },
            Err(e) => tracing::warn!(
                "monitor id unreadable for {route_id} ({e}); \
                 falling back to per-frame screenshots"
            ),
        }
    }

    // Linux Wayland: our own portal session — the only sanctioned capture
    // there, run with a restore token so an unattended start is silent
    // (see `wayland_capture`). When no token is stored yet, the consent
    // dialog needs a human at this machine — say so to the viewer before
    // the wait. A failed or refused session degrades to the per-grab
    // path below, which keeps explaining itself in-band — and so does a
    // session that *opens* but never delivers a frame: a restored grant
    // can point at an output the compositor no longer paints (silent
    // token restores skip the initial frame some compositors only send
    // on fresh consent; a KVM-switched or re-plugged monitor strands the
    // grant entirely), and without a deadline that's a black stage
    // forever with "display asleep" as a wrong diagnosis.
    #[cfg(target_os = "linux")]
    if wayland_session() {
        let had_token = crate::wayland_capture::has_restore_token(monitor_id);
        if !had_token {
            reporter.report(VideoStatusState::WaitingConsent, None);
        }
        match crate::wayland_capture::open(monitor_id) {
            Ok((session, frames)) => {
                tracing::info!("wayland screencast session started for {route_id}");
                let result = pump_frames_with_stall(
                    stop,
                    fps,
                    &frames,
                    move |f: crate::wayland_capture::RawFrame| {
                        // The portal hands over the upright presentation image.
                        orient_to_monitor(f.rgba, f.width, f.height, 0)
                    },
                    on_packet,
                    &mut encoder,
                    &mut stats,
                    &mut reporter,
                    VideoStatusState::DisplayAsleep,
                    Some(WAYLAND_FIRST_FRAME_DEADLINE),
                );
                drop(session);
                if stop.load(Ordering::SeqCst) {
                    return Ok(());
                }
                match result {
                    Ok(()) => return Ok(()),
                    Err(e) => {
                        // A frameless restored session means the saved token
                        // points at an output the compositor no longer paints:
                        // forget it so the next connect re-prompts for fresh
                        // consent (the real recovery) instead of replaying a
                        // dead grant into another frameless wait.
                        if had_token && e.contains("no frame") {
                            crate::wayland_capture::forget_token(monitor_id);
                            tracing::warn!(
                                "wayland screencast for {route_id} delivered no frames; \
                                 dropped its restore token — reconnect to re-consent"
                            );
                        } else {
                            tracing::warn!("wayland screencast for {route_id} ended: {e}");
                        }
                        reporter.report(VideoStatusState::GrabFailed, Some(e));
                    }
                }
            }
            Err(e) => {
                tracing::warn!("wayland screencast for {route_id} unavailable: {e}");
                reporter.report(VideoStatusState::GrabFailed, Some(e));
            }
        }
        // On Wayland the per-frame Screenshot fallback is *not* an acceptable
        // degrade: xcap's grab there routes through the xdg-desktop-portal
        // Screenshot API, and most compositors (GNOME especially) play their
        // screenshot-flash animation on every single grab — the whole panel
        // strobes white at the capture rate, useless and alarming. The
        // ScreenCast portal is the only sane capture path on Wayland, so when
        // it can't run we surface that and stop rather than flash the screen.
        return Ok(());
    }

    // macOS: xcap's AVFoundation session. Two attempts, each with a
    // freshly enumerated monitor: a route that restarts can hand the
    // session a stale display handle, and re-enumerating is exactly what
    // heals that. Only then do we settle for per-frame screenshots — the
    // dire-framerate path of last resort.
    #[cfg(target_os = "macos")]
    for attempt in 0..2 {
        let monitor = select_monitor(monitor_id)?;
        match run_session_capture(
            stop,
            fps,
            route_id,
            &monitor,
            on_packet,
            &mut encoder,
            &mut stats,
            &mut reporter,
        ) {
            Ok(()) => return Ok(()),
            Err(e) => {
                if stop.load(Ordering::SeqCst) {
                    return Ok(());
                }
                if attempt == 0 {
                    tracing::warn!(
                        "capture session for {route_id} unavailable ({e}); \
                         retrying with a fresh monitor handle"
                    );
                    std::thread::sleep(Duration::from_millis(300));
                } else {
                    tracing::warn!(
                        "capture session for {route_id} unavailable ({e}); \
                         falling back to per-frame screenshots"
                    );
                }
            }
        }
    }
    let monitor = select_monitor(monitor_id).inspect_err(|e| {
        reporter.report(VideoStatusState::NoMonitor, Some(e.clone()));
    })?;
    run_oneshot_capture(
        stop,
        fps,
        route_id,
        &monitor,
        on_packet,
        &mut encoder,
        &mut stats,
        &mut reporter,
    )
}

/// Mirrors xcap's (private) `wayland_detect`, so our path choice matches
/// the one xcap will take internally.
#[cfg(target_os = "linux")]
fn wayland_session() -> bool {
    let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    session.eq_ignore_ascii_case("wayland") || display.to_lowercase().contains("wayland")
}

/// How long a just-started session may produce nothing before the
/// viewer is told the display is dark. Only the *first* frame is held
/// to this — once one arrived, a quiet channel is an idle desktop on a
/// damage-driven backend, not a problem.
const FIRST_FRAME_STALL: Duration = Duration::from_secs(5);

/// How long a Wayland portal session may stay frameless before the
/// route gives up on it and degrades to per-frame grabs. Longer than
/// [`FIRST_FRAME_STALL`] so the viewer still hears the honest interim
/// status, and long enough for two wake pulses (one per ~3 s) to coax
/// out a first frame if sleep really was the story. Wayland-only: on
/// Windows/macOS a frameless session reliably *is* a dark display and
/// the wait is the right behaviour, but a portal stream restored from
/// a token can be silently dead on an awake desktop, and only the
/// one-shot path (the Screenshot portal) still produces pixels there.
///
/// Linux-only: the sole caller is the Wayland capture arm, so the constant
/// would be dead code elsewhere (and `-D warnings` rejects it on macOS/Windows).
#[cfg(target_os = "linux")]
const WAYLAND_FIRST_FRAME_DEADLINE: Duration = Duration::from_secs(20);

/// Drain a session's frame channel into the encoder, paced to the target
/// rate: each tick encodes the *freshest* pending frame; a backlog is
/// skipped, never transcoded late. Generic over the session's frame type
/// (`raw` extracts RGBA + dimensions) so the platform backends — our DXGI
/// duplication, xcap's recorders — share one pump. (The Wayland arm
/// calls [`pump_frames_with_stall`] directly: it's the one session a
/// first-frame deadline applies to.)
#[cfg(any(windows, target_os = "macos"))]
#[allow(clippy::too_many_arguments)]
fn pump_frames<T, X>(
    stop: &AtomicBool,
    fps: u32,
    frames: &mpsc::Receiver<T>,
    raw: X,
    on_packet: &(dyn Fn(VideoPacket) -> bool + Send + Sync),
    encoder: &mut StreamEncoder,
    stats: &mut StreamStats,
    reporter: &mut StatusReporter,
) -> Result<(), String>
where
    X: Fn(T) -> (Vec<u8>, u32, u32),
{
    pump_frames_with_stall(
        stop,
        fps,
        frames,
        raw,
        on_packet,
        encoder,
        stats,
        reporter,
        VideoStatusState::DisplayAsleep,
        None,
    )
}

/// [`pump_frames`] with the first-frame-stall condition named by the
/// caller: a frameless screen session is a dark display (worth wake
/// pressure on the panel), a frameless camera is the camera failing —
/// different words to the viewer, and no point lighting the screen.
/// `first_frame_deadline` bounds how long the session may stay
/// frameless before the pump gives up with an error (so the caller can
/// degrade to another capture path); `None` waits as long as the route
/// lives. Only the first frame is ever held to it.
#[allow(clippy::too_many_arguments)]
fn pump_frames_with_stall<T, X>(
    stop: &AtomicBool,
    fps: u32,
    frames: &mpsc::Receiver<T>,
    raw: X,
    on_packet: &(dyn Fn(VideoPacket) -> bool + Send + Sync),
    encoder: &mut StreamEncoder,
    stats: &mut StreamStats,
    reporter: &mut StatusReporter,
    stall_state: VideoStatusState,
    first_frame_deadline: Option<Duration>,
) -> Result<(), String>
where
    X: Fn(T) -> (Vec<u8>, u32, u32),
{
    let budget = Duration::from_secs(1) / fps.max(1);
    let started = Instant::now();
    let from_screen = stall_state == VideoStatusState::DisplayAsleep;
    let mut got_any = false;
    loop {
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        // A bounded wait keeps the stop flag responsive on idle screens.
        let mut frame = match frames.recv_timeout(Duration::from_millis(250)) {
            Ok(f) => f,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // A screen session that opened fine but never delivers is
                // a dark display: damage-driven backends send nothing from
                // a sleeping screen, and even a still desktop hands over
                // its first frame on connect. Keep wake pressure on the
                // panel the whole frameless window (the pulse rate-limits
                // itself) — one polite wiggle at start demonstrably isn't
                // enough on Windows.
                if !got_any {
                    if from_screen {
                        wake::force_display_on();
                    }
                    if started.elapsed() >= FIRST_FRAME_STALL {
                        reporter.report(stall_state, None);
                    }
                    if let Some(deadline) = first_frame_deadline {
                        if started.elapsed() >= deadline {
                            return Err(format!(
                                "no frame within {}s of session start",
                                deadline.as_secs()
                            ));
                        }
                    }
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("capture session ended".to_string());
            }
        };
        got_any = true;
        reporter.report(VideoStatusState::Ok, None);
        let started = Instant::now();
        while let Ok(newer) = frames.try_recv() {
            frame = newer;
        }
        let (rgba, w, h) = raw(frame);
        match encoder.encode(rgba, w, h, stats) {
            Ok(Some(out)) => {
                if on_packet(out) {
                    stats.sent += 1;
                } else {
                    stats.dropped += 1;
                }
            }
            Ok(None) => {}
            Err(e) => return Err(e),
        }
        stats.maybe_log();
        if let Some(rest) = budget.checked_sub(started.elapsed()) {
            std::thread::sleep(rest);
        }
    }
}

/// Bring a captured frame upright to match a **physically-rotated** monitor.
///
/// `rotation_deg` is **authoritative and backend-supplied**. Windows DXGI
/// Output Duplication hands over the raw, *unrotated* scan-out and reports its
/// true clockwise orientation via `DXGI_OUTDUPL_DESC.Rotation` (0/90/180/270),
/// carried on the frame. Every other backend (X11 grab, Wayland portal, macOS
/// AVFoundation, Windows GDI/WGC fallback) delivers the already-upright
/// *presentation* image and passes 0, falling through the 0-turn path
/// untouched.
///
/// This replaces an earlier orientation-mismatch heuristic that compared the
/// buffer against the monitor's reported size: it read as "not rotated at all"
/// whenever the monitor library reported native (unrotated) dimensions, so the
/// mismatch never fired. DXGI's rotation is the actual scan-out orientation and
/// does not under-report, so it is trusted exclusively. Quarter-turns = deg/90,
/// clockwise (`rotate_rgba`'s convention): ROTATE90→1, ROTATE270→3 — undoing
/// the display's counter-clockwise rotation (direction verified against the
/// canonical DXGI Desktop Duplication sample's vertex transform).
fn orient_to_monitor(rgba: Vec<u8>, bw: u32, bh: u32, rotation_deg: u32) -> (Vec<u8>, u32, u32) {
    let turns = ((rotation_deg / 90) % 4) as u8;
    if turns == 0 {
        log_orient(rotation_deg, bw, bh, turns, bw, bh);
        return (rgba, bw, bh);
    }
    // rotate_rgba swaps dims for odd quarter-turns and preserves them for 180°.
    let (rotated, ow, oh) = allmystuff_pixels::rotate_rgba(&rgba, bw, bh, turns);
    log_orient(rotation_deg, bw, bh, turns, ow, oh);
    (rotated, ow, oh)
}

/// One-line ground-truth log for [`orient_to_monitor`], emitted once per
/// distinct (rotation, dims, turns) tuple so a rotated-monitor report is
/// self-diagnosing without printing a line per frame. Survives multiple
/// concurrent monitors (each distinct config logs exactly once, ever).
fn log_orient(rotation_deg: u32, bw: u32, bh: u32, turns: u8, ow: u32, oh: u32) {
    use std::collections::hash_map::DefaultHasher;
    use std::collections::HashSet;
    use std::hash::{Hash, Hasher};
    use std::sync::{Mutex, OnceLock};
    static SEEN: OnceLock<Mutex<HashSet<u64>>> = OnceLock::new();
    let mut h = DefaultHasher::new();
    (rotation_deg, bw, bh, turns).hash(&mut h);
    let fresh = SEEN
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .map(|mut s| s.insert(h.finish()))
        .unwrap_or(false);
    if fresh {
        tracing::info!(
            target: "capture::orient",
            "rotation_deg={rotation_deg} buf={bw}x{bh} -> turns={turns} out={ow}x{oh}"
        );
    }
}

/// Stream from xcap's persistent AVFoundation capture session. Set-up
/// happens once; frames arrive as the OS produces them — damage-driven
/// backends send nothing while the screen is still. (Wayland rides
/// `wayland_capture` instead; X11 prefers the paced one-shot loop.)
#[cfg(target_os = "macos")]
#[allow(clippy::too_many_arguments)]
fn run_session_capture(
    stop: &AtomicBool,
    fps: u32,
    route_id: &str,
    monitor: &xcap::Monitor,
    on_packet: &(dyn Fn(VideoPacket) -> bool + Send + Sync),
    encoder: &mut StreamEncoder,
    stats: &mut StreamStats,
    reporter: &mut StatusReporter,
) -> Result<(), String> {
    let (recorder, frames) = monitor.video_recorder().map_err(|e| e.to_string())?;
    recorder.start().map_err(|e| e.to_string())?;
    tracing::info!("screen capture session started for {route_id}");
    let result = pump_frames(
        stop,
        fps,
        &frames,
        // AVFoundation delivers the upright presentation image.
        move |f| orient_to_monitor(f.raw, f.width, f.height, 0),
        on_packet,
        encoder,
        stats,
        reporter,
    );
    let _ = recorder.stop();
    result
}

/// One screenshot per tick — the X11 path and the universal fallback.
/// Every grab pays the platform's full one-shot cost, so the effective
/// rate is whatever that path allows; the encoder's unchanged-frame gate
/// at least makes idle screens cheap to *send*.
#[allow(clippy::too_many_arguments)]
fn run_oneshot_capture(
    stop: &AtomicBool,
    fps: u32,
    route_id: &str,
    monitor: &xcap::Monitor,
    on_packet: &(dyn Fn(VideoPacket) -> bool + Send + Sync),
    encoder: &mut StreamEncoder,
    stats: &mut StreamStats,
    reporter: &mut StatusReporter,
) -> Result<(), String> {
    let budget = Duration::from_secs(1) / fps.max(1);
    let mut failures = 0u64;
    while !stop.load(Ordering::SeqCst) {
        let started = Instant::now();
        let outcome = monitor
            .capture_image()
            .map_err(|e| e.to_string())
            .and_then(|image| {
                let (sw, sh) = (image.width(), image.height());
                // capture_image (X11 grab, Windows GDI/WGC fallback) is upright.
                let (rgba, sw, sh) = orient_to_monitor(image.into_raw(), sw, sh, 0);
                encoder.encode(rgba, sw, sh, stats)
            });
        match outcome {
            Ok(Some(packet)) => {
                failures = 0;
                reporter.report(VideoStatusState::Ok, None);
                if on_packet(packet) {
                    stats.sent += 1;
                } else {
                    stats.dropped += 1;
                }
            }
            Ok(None) => {
                failures = 0;
                reporter.report(VideoStatusState::Ok, None);
            }
            Err(e) => {
                // A transient grab failure (screen lock, monitor sleep)
                // shouldn't end the stream — but a *persistent* one (a
                // denied screen-recording permission, a Wayland portal
                // that never granted) must be loud, not a debug whisper:
                // it reads as "connected but no pixels" at the far end.
                // The viewer hears it too, in-band — and the panel keeps
                // getting wake pressure in case sleep is the whole story.
                wake::force_display_on();
                failures += 1;
                if failures == 1 || failures.is_multiple_of(100) {
                    tracing::warn!("screen grab failing for {route_id} ({failures}x): {e}");
                } else {
                    tracing::debug!("screen grab failed for {route_id}: {e}");
                }
                reporter.report(VideoStatusState::GrabFailed, Some(e));
            }
        }
        stats.maybe_log();
        if let Some(rest) = budget.checked_sub(started.elapsed()) {
            std::thread::sleep(rest);
        }
    }
    Ok(())
}

/// The monitor a capture should run on: by enumeration id when the route
/// names one, else the primary. A named monitor that's gone (unplugged
/// since the scan) degrades to the primary with a note — a stream beats
/// an error.
fn select_monitor(monitor_id: Option<u32>) -> Result<xcap::Monitor, String> {
    let monitors = xcap::Monitor::all().map_err(|e| e.to_string())?;
    if let Some(id) = monitor_id {
        for m in &monitors {
            if m.id().ok() == Some(id) {
                return Ok(m.clone());
            }
        }
        tracing::warn!("monitor {id} not found (unplugged?); capturing the primary instead");
    }
    let mut first = None;
    for m in monitors {
        if m.is_primary().unwrap_or(false) {
            return Ok(m);
        }
        first.get_or_insert(m);
    }
    first.ok_or_else(|| "no monitor to capture".to_string())
}

/// Every monitor beyond the primary, as `(id, label)` for the bridge's
/// per-monitor `screen:<id>` capabilities — so a multi-monitor machine
/// gets one console tab per screen. The primary stays the synthetic
/// `screen` capability (and the universal fallback), so a single-monitor
/// machine advertises exactly what it did before. Ids are xcap's own
/// enumeration ids: stable for this app run, and resolved back to the
/// same monitor by [`select_monitor`] when a route starts.
pub fn extra_screens() -> Vec<allmystuff_bridge::ScreenSource> {
    let Ok(monitors) = xcap::Monitor::all() else {
        return Vec::new();
    };
    // Mirror select_monitor's primary choice (first flagged, else first
    // listed) so the two views of "which one is the primary" can't drift.
    let primary = monitors
        .iter()
        .position(|m| m.is_primary().unwrap_or(false))
        .unwrap_or(0);
    monitors
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != primary)
        .filter_map(|(i, m)| {
            let id = m.id().ok()?;
            // Windows reports device paths (`\\.\DISPLAY2`); strip the
            // prefix so the tab reads as a name, not an escape sequence.
            let name = m.name().unwrap_or_default();
            let name = name.trim_start_matches(r"\\.\").trim();
            let label = if name.is_empty() {
                format!("Screen {}", i + 1)
            } else {
                format!("Screen — {name}")
            };
            Some(allmystuff_bridge::ScreenSource { id, label })
        })
        .collect()
}

/// The downscale + JPEG stage of one route's stream, with the
/// unchanged-frame gate: pixels identical to the last send (compared
/// post-downscale, where the buffer is small) return `None` instead of
/// burning an encode on a picture the viewer already has — refreshed every
/// [`STATIC_REFRESH`] regardless. Owns the stream's `seq`, which counts
/// *sent* frames (the receiver logs its first frame as `seq == 0`).
struct FrameEncoder {
    route_id: String,
    seq: u64,
    prev: Vec<u8>,
    prev_size: (u32, u32),
    last_sent: Option<Instant>,
    max_edge: u32,
    /// JPEG quality this stream encodes at — the Rate pill, mapped through
    /// [`Tune::jpeg_quality`] (MJPEG's stand-in for a bitrate).
    quality: u8,
    /// The route's one-shot "resend now" flag (a viewer asked).
    refresh: Arc<AtomicBool>,
    /// The receiver-driven resolution cap — read per frame, min'd with
    /// `max_edge` (see [`AutoAdapt`]).
    auto: Arc<AutoAdapt>,
}

impl FrameEncoder {
    fn new(route_id: &str, tune: Tune, refresh: Arc<AtomicBool>, auto: Arc<AutoAdapt>) -> Self {
        FrameEncoder {
            route_id: route_id.to_string(),
            seq: 0,
            prev: Vec::new(),
            prev_size: (0, 0),
            last_sent: None,
            max_edge: tune.mjpeg_edge(),
            quality: tune.jpeg_quality(),
            refresh,
            auto,
        }
    }

    fn encode(
        &mut self,
        rgba: Vec<u8>,
        sw: u32,
        sh: u32,
        stats: &mut StreamStats,
    ) -> Result<Option<VideoFrame>, String> {
        let edge = match self.auto.edge_cap() {
            Some(cap) => self.max_edge.min(cap),
            None => self.max_edge,
        };
        let (dw, dh) = fit_within(sw, sh, edge);
        let t0 = Instant::now();
        let scaled = if (dw, dh) == (sw, sh) {
            rgba
        } else {
            scale_rgba(&rgba, sw, sh, dw, dh)
        };
        stats.scale += t0.elapsed();
        (stats.out_w, stats.out_h) = (dw, dh);
        let refresh_due = self.refresh.swap(false, Ordering::SeqCst)
            || self
                .last_sent
                .is_none_or(|sent| sent.elapsed() >= STATIC_REFRESH);
        if !refresh_due && self.prev_size == (dw, dh) && self.prev == scaled {
            stats.static_skipped += 1;
            return Ok(None);
        }
        let t1 = Instant::now();
        let jpeg = encode_jpeg(&scaled, dw, dh, self.quality)?;
        stats.encode += t1.elapsed();
        stats.bytes += jpeg.len() as u64;
        stats.keyframes += 1; // every MJPEG frame is standalone
        self.prev = scaled;
        self.prev_size = (dw, dh);
        self.last_sent = Some(Instant::now());
        let frame = VideoFrame::new(&self.route_id, self.seq, dw, dh, sw, sh, jpeg);
        self.seq += 1;
        Ok(Some(frame))
    }
}

/// One route's encoder for the negotiated transport — with the rule that
/// an encoder that can't init (openh264 build/runtime trouble) must cost
/// quality, not the stream: fall back to MJPEG and say so.
#[allow(clippy::too_many_arguments)]
fn make_encoder(
    route_id: &str,
    mode: VideoMode,
    source_hint: (u32, u32),
    tune: Tune,
    refresh: &Arc<AtomicBool>,
    idr_ms: &Arc<AtomicU64>,
    auto: &Arc<AutoAdapt>,
) -> Result<StreamEncoder, String> {
    match StreamEncoder::new(route_id, mode, source_hint, tune, refresh, idr_ms, auto) {
        Ok(enc) => Ok(enc),
        Err(e) => {
            tracing::warn!("encoder for {route_id} unavailable ({e}); falling back to MJPEG");
            StreamEncoder::new(
                route_id,
                VideoMode::Mjpeg,
                source_hint,
                tune,
                refresh,
                idr_ms,
                auto,
            )
        }
    }
}

/// The per-route encode stage, dispatching on the negotiated transport.
/// (The H.264 arm boxes openh264's chunky encoder state.)
enum StreamEncoder {
    Mjpeg(FrameEncoder),
    H264(Box<H264Stream>),
}

impl StreamEncoder {
    /// The transport this encoder actually produces (the negotiated one,
    /// or the MJPEG floor [`make_encoder`] fell back to).
    fn mode(&self) -> VideoMode {
        match self {
            StreamEncoder::Mjpeg(_) => VideoMode::Mjpeg,
            StreamEncoder::H264(_) => VideoMode::H264,
        }
    }

    /// `source_hint` is the capture source's expected resolution (the
    /// monitor's), which pre-budgets the H.264 bitrate; `(0, 0)` =
    /// unknown. The real budget locks to the first frame's true fitted
    /// size either way (logical-vs-physical monitor reports differ).
    #[allow(clippy::too_many_arguments)]
    fn new(
        route_id: &str,
        mode: VideoMode,
        source_hint: (u32, u32),
        tune: Tune,
        refresh: &Arc<AtomicBool>,
        idr_ms: &Arc<AtomicU64>,
        auto: &Arc<AutoAdapt>,
    ) -> Result<Self, String> {
        match mode {
            // MJPEG is stateless — no keyframes — so the adaptive IDR cadence
            // doesn't apply; only the H.264 stream reads `idr_ms`.
            VideoMode::Mjpeg => Ok(StreamEncoder::Mjpeg(FrameEncoder::new(
                route_id,
                tune,
                refresh.clone(),
                auto.clone(),
            ))),
            VideoMode::H264 => Ok(StreamEncoder::H264(Box::new(H264Stream::new(
                source_hint,
                tune,
                refresh.clone(),
                idr_ms.clone(),
                auto.clone(),
            )?))),
        }
    }

    fn encode(
        &mut self,
        rgba: Vec<u8>,
        sw: u32,
        sh: u32,
        stats: &mut StreamStats,
    ) -> Result<Option<VideoPacket>, String> {
        match self {
            StreamEncoder::Mjpeg(enc) => {
                Ok(enc.encode(rgba, sw, sh, stats)?.map(VideoPacket::Jpeg))
            }
            StreamEncoder::H264(enc) => enc.encode(rgba, sw, sh, stats),
        }
    }
}

/// The H.264 encode stage of one route's stream — openh264 in
/// screen-content mode, scaled to the [`h264_max_edge`] ceiling (even
/// dimensions for 4:2:0, native up to 4K by default), with the same
/// unchanged-frame gate as MJPEG and a forced IDR on an adaptive cadence
/// ([`adaptive_idr_ms`], floored at [`IDR_MS_TIGHT`]) so a viewer always has
/// a decode entry point within seconds. A resolution change (monitor swap)
/// re-initializes the encoder inside openh264; the next unit out is an IDR.
struct H264Stream {
    /// The active H.264 backend — a hardware encoder (Media Foundation's GPU
    /// H.264 MFT on Windows; NVENC/AMF/QSV/VideoToolbox/VA-API via FFmpeg on
    /// Linux/macOS) when one passed the frame-send test, else software openh264.
    /// Rebuilt (re-laddered) on a resize.
    codec: Box<dyn H264Codec>,
    /// The fitted size the current encoder's bitrate was budgeted for.
    /// The first real frame (or a monitor swap) that fits to a different
    /// size rebuilds the encoder with a corrected budget — monitor
    /// *reports* are logical pixels on HiDPI while captures are physical,
    /// and a 4× pixel gap on the same budget is a mush stream.
    budget_size: (u32, u32),
    tune: Tune,
    fps: u32,
    prev: Vec<u8>,
    prev_size: (u32, u32),
    last_sent: Option<Instant>,
    last_idr: Option<Instant>,
    /// Wall-clock instant of the last *emitted* access unit. The RTP duration of
    /// the next unit is the real gap since this — not a nominal 1/fps — so the
    /// 90 kHz clock tracks wall-clock across static-skip gaps (a 2 s idle then
    /// motion gets a ~2 s duration, not 1/fps), instead of lagging and churning
    /// the viewer's jitter buffer on motion onset.
    last_emit: Option<Instant>,
    /// The route's one-shot "clean entry now" flag (a viewer asked).
    refresh: Arc<AtomicBool>,
    /// The current forced-IDR interval (ms), adapted from receiver feedback
    /// ([`VideoBridge::note_feedback`]). Read fresh each frame; default
    /// [`IDR_MS_TIGHT`].
    idr_ms: Arc<AtomicU64>,
    /// The receiver-driven resolution cap ([`AutoAdapt`]), min'd with the
    /// tuned edge per frame — a changed fit re-inits the encoder through the
    /// budget-size rebuild below, no capture restart.
    auto: Arc<AutoAdapt>,
}

impl H264Stream {
    fn new(
        source_hint: (u32, u32),
        tune: Tune,
        refresh: Arc<AtomicBool>,
        idr_ms: Arc<AtomicU64>,
        auto: Arc<AutoAdapt>,
    ) -> Result<Self, String> {
        let fps = tune.fps();
        // Pre-budget from the monitor's report fitted to the edge
        // ceiling (unknown → 1080p, the old fixed default's density);
        // the first real frame corrects it if the capture's true size
        // differs.
        let auto_capped_edge = match auto.edge_cap() {
            Some(cap) => tune.h264_edge().min(cap),
            None => tune.h264_edge(),
        };
        let (bw, bh) = if source_hint.0 == 0 || source_hint.1 == 0 {
            (1920, 1080)
        } else {
            fit_within_even(source_hint.0, source_hint.1, auto_capped_edge)
        };
        let codec = make_h264_codec(bw, bh, fps, tune)?;
        Ok(H264Stream {
            codec,
            budget_size: (bw, bh),
            tune,
            fps,
            prev: Vec::new(),
            prev_size: (0, 0),
            last_sent: None,
            last_idr: None,
            last_emit: None,
            refresh,
            idr_ms,
            auto,
        })
    }

    /// The edge this frame fits to: the tuned ceiling, capped by the
    /// receiver-driven auto-adapt when it's active.
    fn effective_edge(&self) -> u32 {
        match self.auto.edge_cap() {
            Some(cap) => self.tune.h264_edge().min(cap),
            None => self.tune.h264_edge(),
        }
    }

    fn encode(
        &mut self,
        rgba: Vec<u8>,
        sw: u32,
        sh: u32,
        stats: &mut StreamStats,
    ) -> Result<Option<VideoPacket>, String> {
        if sw == 0 || sh == 0 {
            return Ok(None);
        }
        let (dw, dh) = fit_within_even(sw, sh, self.effective_edge());
        // The real fitted size is known now — if it differs from what the
        // bitrate was budgeted for (HiDPI monitors *report* logical pixels
        // but *capture* physical ones), rebuild the encoder on a corrected
        // budget. Its first unit out is an IDR.
        if (dw, dh) != self.budget_size {
            self.codec = make_h264_codec(dw, dh, self.fps, self.tune)?;
            self.budget_size = (dw, dh);
            self.last_idr = None;
        }
        // Downscale and convert straight to I420 in one fused pass — no RGB
        // intermediate buffer, and openh264's separate RGB→YUV walk is gone
        // (we hand it the planes directly below). The unchanged-frame compare
        // also runs on the smaller 1.5-byte/pixel I420 instead of 3-byte RGB.
        let t0 = Instant::now();
        let i420 = scale_rgba_to_i420(&rgba, sw, sh, dw, dh);
        stats.scale += t0.elapsed();
        (stats.out_w, stats.out_h) = (dw, dh);
        let refresh_asked = self.refresh.swap(false, Ordering::SeqCst);
        let refresh_due = refresh_asked
            || self
                .last_sent
                .is_none_or(|sent| sent.elapsed() >= STATIC_REFRESH);
        if !refresh_due && self.prev_size == (dw, dh) && self.prev == i420 {
            stats.static_skipped += 1;
            return Ok(None);
        }
        // The periodic-IDR interval is adaptive: the receiver's feedback
        // relaxes it on a healthy link and tightens it on a struggling one
        // (see `adaptive_idr_ms`). Default `IDR_MS_TIGHT` = the old fixed 2 s.
        let idr_every = Duration::from_millis(self.idr_ms.load(Ordering::Relaxed));
        let force_idr = refresh_asked || self.last_idr.is_none_or(|idr| idr.elapsed() >= idr_every);
        // Hand the I420 to whichever backend the ladder selected — hardware or
        // software. It returns the Annex-B access unit and whether it's a key.
        let t1 = Instant::now();
        let out = self
            .codec
            .encode_i420(&i420, dw as usize, dh as usize, force_idr)?;
        stats.encode += t1.elapsed();
        self.prev = i420;
        self.prev_size = (dw, dh);
        self.last_sent = Some(Instant::now());
        // Rate control (or an encoder still buffering) may emit nothing.
        let Some((data, key)) = out else {
            return Ok(None);
        };
        if key {
            self.last_idr = Some(Instant::now());
            stats.keyframes += 1;
        }
        if data.is_empty() {
            return Ok(None);
        }
        stats.bytes += data.len() as u64;
        // Duration = real wall-clock gap since the last emitted unit, so the RTP
        // timestamp tracks wall-clock (a static-skip gap carries its full
        // elapsed time forward). Clamped to [1/2fps, 5 s] so a paused/just-
        // started route can't emit an absurd duration. First unit uses nominal.
        let now = Instant::now();
        let nominal = 1_000_000u64 / u64::from(self.fps.max(1));
        let duration_us = match self.last_emit {
            Some(prev) => {
                (now.duration_since(prev).as_micros() as u64).clamp(nominal / 2, 5_000_000)
            }
            None => nominal,
        };
        self.last_emit = Some(now);
        Ok(Some(VideoPacket::H264 { data, duration_us }))
    }
}

/// A borrowing view over a contiguous I420 buffer (Y, then U, then V) that
/// satisfies openh264's `YUVSource` — lets [`H264Stream::encode`] hand the
/// planes straight to the encoder with no copy and no RGB→YUV step.
struct I420Frame<'a> {
    buf: &'a [u8],
    w: usize,
    h: usize,
}

impl openh264::formats::YUVSource for I420Frame<'_> {
    fn dimensions(&self) -> (usize, usize) {
        (self.w, self.h)
    }
    fn strides(&self) -> (usize, usize, usize) {
        (self.w, self.w / 2, self.w / 2)
    }
    fn y(&self) -> &[u8] {
        &self.buf[..self.w * self.h]
    }
    fn u(&self) -> &[u8] {
        let ys = self.w * self.h;
        let cs = (self.w / 2) * (self.h / 2);
        &self.buf[ys..ys + cs]
    }
    fn v(&self) -> &[u8] {
        let ys = self.w * self.h;
        let cs = (self.w / 2) * (self.h / 2);
        &self.buf[ys + cs..ys + 2 * cs]
    }
}

/// A pluggable H.264 backend: one fitted I420 frame in, an Annex-B access unit
/// out. The ladder ([`make_h264_codec`]) selects the implementation; everything
/// around it (scaling, the static-frame skip, the adaptive IDR cadence, stats)
/// stays in [`H264Stream`]. `Send` so the whole stream can live on the route's
/// capture/encode thread.
trait H264Codec: Send {
    /// Encode one contiguous I420 frame (`w*h` Y, then quarter-size U, then V).
    /// `force_idr` requests a keyframe. Returns the access unit + whether it was
    /// a keyframe, or `None` when the encoder emitted nothing (rate-control skip
    /// or buffering).
    fn encode_i420(
        &mut self,
        i420: &[u8],
        w: usize,
        h: usize,
        force_idr: bool,
    ) -> Result<Option<(Vec<u8>, bool)>, String>;
    /// Human label for logs ("openh264 (software)", "h264_nvenc", …).
    fn label(&self) -> &str;
}

/// Software openh264 — the guaranteed floor of the ladder.
struct OpenH264Codec(openh264::encoder::Encoder);

impl H264Codec for OpenH264Codec {
    fn encode_i420(
        &mut self,
        i420: &[u8],
        w: usize,
        h: usize,
        force_idr: bool,
    ) -> Result<Option<(Vec<u8>, bool)>, String> {
        if force_idr {
            self.0.force_intra_frame();
        }
        let stream = self
            .0
            .encode(&I420Frame { buf: i420, w, h })
            .map_err(|e| format!("openh264 encode: {e}"))?;
        let key = matches!(
            stream.frame_type(),
            openh264::encoder::FrameType::IDR | openh264::encoder::FrameType::I
        );
        let data = stream.to_vec();
        if data.is_empty() {
            Ok(None)
        } else {
            Ok(Some((data, key)))
        }
    }
    fn label(&self) -> &str {
        "openh264 (software)"
    }
}

/// Media Foundation hardware backend (NVENC/QuickSync/AMD via the OS's H.264
/// MFT) — the Windows hardware path, no FFmpeg toolchain. Thin wrapper so the
/// inherent methods on `mediafoundation::MediaFoundationH264` don't clash with
/// the trait.
#[cfg(windows)]
struct MfCodec(crate::mediafoundation::MediaFoundationH264);

#[cfg(windows)]
impl H264Codec for MfCodec {
    fn encode_i420(
        &mut self,
        i420: &[u8],
        _w: usize,
        _h: usize,
        force_idr: bool,
    ) -> Result<Option<(Vec<u8>, bool)>, String> {
        // The MFT is fixed to the size it was opened at — the same (dw, dh) the
        // ladder built it for; `H264Stream` rebuilds on resize.
        self.0.encode_i420(i420, force_idr)
    }
    fn label(&self) -> &str {
        self.0.label()
    }
}

/// VideoToolbox hardware backend — the Mac's media engine, no FFmpeg
/// toolchain (see `videotoolbox.rs`). Thin wrapper so the inherent methods
/// don't clash with the trait.
#[cfg(target_os = "macos")]
struct VtCodec(crate::videotoolbox::VideoToolboxH264);

#[cfg(target_os = "macos")]
impl H264Codec for VtCodec {
    fn encode_i420(
        &mut self,
        i420: &[u8],
        _w: usize,
        _h: usize,
        force_idr: bool,
    ) -> Result<Option<(Vec<u8>, bool)>, String> {
        // The session is fixed to the size it was opened at — the same
        // (dw, dh) the ladder built it for; `H264Stream` rebuilds on resize.
        self.0.encode_i420(i420, force_idr)
    }
    fn label(&self) -> &str {
        self.0.label()
    }
}

/// FFmpeg hardware backend (NVENC/AMF/QSV/VideoToolbox/VA-API), behind the
/// `hwenc` feature. Thin wrapper so the inherent methods on `hwenc::FfmpegH264`
/// don't clash with the trait.
#[cfg(feature = "hwenc")]
struct FfmpegCodec(crate::hwenc::FfmpegH264);

#[cfg(feature = "hwenc")]
impl H264Codec for FfmpegCodec {
    fn encode_i420(
        &mut self,
        i420: &[u8],
        _w: usize,
        _h: usize,
        force_idr: bool,
    ) -> Result<Option<(Vec<u8>, bool)>, String> {
        // The FFmpeg encoder is fixed to the size it was opened at — the same
        // (dw, dh) the ladder built it for; `H264Stream` rebuilds on resize.
        self.0.encode_i420(i420, force_idr)
    }
    fn label(&self) -> &str {
        self.0.label()
    }
}

/// Build the best H.264 backend for `bw`×`bh` at `fps`: walk the platform's
/// hardware candidates (NVENC first), open each and **frame-send-test** it —
/// the first that actually emits an access unit wins. Anything that won't open
/// or won't produce a frame (no GPU, driver/permission/session-cap trouble) is
/// stepped over, down to software openh264, which is the guaranteed floor.
///
/// The hardware rung is platform-split: Windows uses **Media Foundation** (the
/// GPU's own H.264 MFT, no FFmpeg toolchain); Linux/macOS use the **FFmpeg**
/// vendor encoders behind the `hwenc` feature.
fn make_h264_codec(bw: u32, bh: u32, fps: u32, tune: Tune) -> Result<Box<dyn H264Codec>, String> {
    // Windows: the GPU's hardware H.264 MFT via Media Foundation. Enumerated
    // best-first by the OS; each is opened and frame-send-tested, stepping down
    // to the next (then to software) on any failure. No extra build toolchain.
    #[cfg(windows)]
    {
        let bitrate = tune
            .bitrate
            .unwrap_or_else(|| h264_bitrate_for(bw, bh, fps))
            .clamp(250_000, 80_000_000);
        for hw in crate::mediafoundation::hardware_h264_mfts() {
            match hw.open(bw, bh, fps, bitrate) {
                Ok(enc) => {
                    let mut codec = MfCodec(enc);
                    if codec_emits_frame(&mut codec, bw, bh) {
                        tracing::info!(
                            "H.264 hardware encoder: {} · {bw}×{bh} · {:.1} Mbps @ {fps} fps (Media Foundation)",
                            codec.label(),
                            bitrate as f64 / 1e6
                        );
                        return Ok(Box::new(codec));
                    }
                    tracing::info!(
                        "H.264 encoder {} opened but produced no frame in the send test — stepping down",
                        codec.label()
                    );
                }
                Err(e) => tracing::debug!("Media Foundation H.264 MFT unavailable: {e}"),
            }
        }
    }
    // macOS: the media engine via VideoToolbox — hardware required (its
    // software fallback would just be a slower openh264), frame-send-tested
    // like every rung. This is what takes a Retina Mac host off software
    // openh264, the encoder the viewer experienced as a slideshow.
    #[cfg(target_os = "macos")]
    {
        let bitrate = tune
            .bitrate
            .unwrap_or_else(|| h264_bitrate_for(bw, bh, fps))
            .clamp(250_000, 80_000_000);
        match crate::videotoolbox::VideoToolboxH264::open(bw, bh, fps, bitrate) {
            Ok(enc) => {
                let mut codec = VtCodec(enc);
                if codec_emits_frame(&mut codec, bw, bh) {
                    tracing::info!(
                        "H.264 hardware encoder: {} · {bw}×{bh} · {:.1} Mbps @ {fps} fps",
                        codec.label(),
                        bitrate as f64 / 1e6
                    );
                    return Ok(Box::new(codec));
                }
                tracing::info!(
                    "VideoToolbox opened but produced no frame in the send test — stepping down"
                );
            }
            Err(e) => tracing::debug!("VideoToolbox H.264 unavailable: {e}"),
        }
    }
    #[cfg(feature = "hwenc")]
    {
        let bitrate = tune
            .bitrate
            .unwrap_or_else(|| h264_bitrate_for(bw, bh, fps))
            .clamp(250_000, 80_000_000);
        for &name in crate::hwenc::candidates() {
            match crate::hwenc::FfmpegH264::open(name, bw, bh, fps, bitrate) {
                Ok(enc) => {
                    let mut codec = FfmpegCodec(enc);
                    if codec_emits_frame(&mut codec, bw, bh) {
                        tracing::info!(
                            "H.264 hardware encoder: {} · {bw}×{bh} · {:.1} Mbps @ {fps} fps",
                            codec.label(),
                            bitrate as f64 / 1e6
                        );
                        return Ok(Box::new(codec));
                    }
                    tracing::info!(
                        "H.264 encoder {name} opened but produced no frame in the send test — stepping down"
                    );
                }
                Err(e) => tracing::debug!("H.264 encoder {name} unavailable: {e}"),
            }
        }
    }
    let codec: Box<dyn H264Codec> = Box::new(OpenH264Codec(make_h264_encoder(bw, bh, fps, tune)?));
    tracing::info!(
        "H.264 software encoder: {} · {bw}×{bh} · {:.1} Mbps @ {fps} fps",
        codec.label(),
        tune.bitrate
            .unwrap_or_else(|| h264_bitrate_for(bw, bh, fps))
            .clamp(250_000, 80_000_000) as f64
            / 1e6
    );
    Ok(codec)
}

/// The frame-send test that drives the step-down: feed one neutral-grey I420
/// and confirm the encoder emits an access unit within a few frames. A backend
/// that opens but won't actually produce frames is stepped over.
#[cfg(any(feature = "hwenc", windows, target_os = "macos"))]
fn codec_emits_frame(codec: &mut dyn H264Codec, w: u32, h: u32) -> bool {
    let (w, h) = (w as usize, h as usize);
    let grey = vec![128u8; w * h + 2 * ((w / 2) * (h / 2))];
    for _ in 0..3 {
        match codec.encode_i420(&grey, w, h, true) {
            Ok(Some((d, _))) if !d.is_empty() => return true,
            Ok(_) => continue, // buffering — try another frame
            Err(_) => return false,
        }
    }
    false
}

/// One openh264 encoder, budgeted for the given fitted size: the tune's
/// explicit bitrate when the viewer set one, the pixel budget otherwise.
fn make_h264_encoder(
    bw: u32,
    bh: u32,
    fps: u32,
    tune: Tune,
) -> Result<openh264::encoder::Encoder, String> {
    use openh264::encoder::{
        BitRate, Encoder, EncoderConfig, FrameRate, RateControlMode, UsageType,
    };
    let bitrate = tune
        .bitrate
        .unwrap_or_else(|| h264_bitrate_for(bw, bh, fps))
        .clamp(250_000, 80_000_000);
    let config = EncoderConfig::new()
        .usage_type(UsageType::ScreenContentRealTime)
        .rate_control_mode(RateControlMode::Bitrate)
        .bitrate(BitRate::from_bps(bitrate))
        .max_frame_rate(FrameRate::from_hz(fps as f32));
    Encoder::with_api_config(openh264::OpenH264API::from_source(), config)
        .map_err(|e| format!("openh264 init: {e}"))
}

/// [`fit_within`], then force both edges even (4:2:0 chroma subsampling
/// needs it; a 1-pixel crop is invisible at these sizes).
fn fit_within_even(w: u32, h: u32, max_edge: u32) -> (u32, u32) {
    let (w, h) = fit_within(w, h, max_edge);
    ((w & !1).max(2), (h & !1).max(2))
}

fn encode_jpeg(rgba: &[u8], w: u32, h: u32, quality: u8) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(64 * 1024);
    let encoder = jpeg_encoder::Encoder::new(&mut out, quality);
    encoder
        .encode(
            rgba,
            w.try_into().map_err(|_| "frame too wide")?,
            h.try_into().map_err(|_| "frame too tall")?,
            jpeg_encoder::ColorType::Rgba,
        )
        .map_err(|e| e.to_string())?;
    Ok(out)
}

/// Scale `(w, h)` down (never up) so the longest edge fits `max_edge`,
/// preserving aspect.
fn fit_within(w: u32, h: u32, max_edge: u32) -> (u32, u32) {
    let edge = w.max(h);
    if edge <= max_edge || edge == 0 {
        return (w, h);
    }
    let num = max_edge as u64;
    let den = edge as u64;
    (
        ((w as u64 * num / den) as u32).max(1),
        ((h as u64 * num / den) as u32).max(1),
    )
}

// The scalers live in `allmystuff-pixels` (path crate) purely so dev
// builds run them at opt-level 3 — at opt 0 they alone cap the stream at
// single-digit fps on a Retina/4K source.
use allmystuff_pixels::{scale_rgba, scale_rgba_to_i420};

#[cfg(test)]
mod tests {
    use super::*;

    fn fb(recv_fps: u32, decode_fails: u32, queue_depth: u32) -> RecvFeedback {
        RecvFeedback {
            recv_fps,
            decode_fails,
            queue_depth,
            at: Instant::now(),
        }
    }

    #[test]
    fn auto_adapt_steps_down_after_a_sustained_stall_and_holds_between_steps() {
        // The field failure: a 60 fps encode arriving at 0–6 fps for tens of
        // seconds while the sender kept pushing full resolution.
        let auto = AutoAdapt::new();
        let t0 = Instant::now();
        // Two struggling reports: not yet a verdict.
        assert_eq!(auto.observe(&fb(2, 2, 0), 60, t0), None);
        assert_eq!(auto.observe(&fb(0, 1, 0), 60, t0), None);
        assert!(auto.edge_cap().is_none());
        // Third consecutive: step down one rung.
        assert_eq!(auto.observe(&fb(4, 0, 0), 60, t0), Some((0, 2560)));
        assert_eq!(auto.edge_cap(), Some(2560));
        // Still struggling immediately after — held by the settle window,
        // no second step until it has had time to show up in feedback.
        for _ in 0..5 {
            assert_eq!(auto.observe(&fb(0, 0, 0), 60, t0), None);
        }
        // Past the hold, the sustained stall steps again.
        let t1 = t0 + AUTO_DOWN_HOLD;
        assert_eq!(auto.observe(&fb(0, 0, 0), 60, t1), Some((2560, 1920)));
        // A healthy report in a bad streak resets it — no flappy verdicts.
        let t2 = t1 + AUTO_DOWN_HOLD;
        assert_eq!(auto.observe(&fb(1, 0, 0), 60, t2), None);
        assert_eq!(auto.observe(&fb(1, 0, 0), 60, t2), None);
        assert_eq!(auto.observe(&fb(55, 0, 0), 60, t2), None); // healthy
        assert_eq!(auto.observe(&fb(1, 0, 0), 60, t2), None);
        assert_eq!(auto.observe(&fb(1, 0, 0), 60, t2), None);
        assert_eq!(auto.edge_cap(), Some(1920), "reset streak must not step");
    }

    #[test]
    fn auto_adapt_recovers_slowly_and_stops_at_the_floor() {
        let auto = AutoAdapt::new();
        let t0 = Instant::now();
        // Drive it to the floor.
        let mut t = t0;
        for _ in 0..AUTO_EDGES.len() {
            t += AUTO_DOWN_HOLD;
            for _ in 0..AUTO_BAD_STREAK {
                auto.observe(&fb(0, 0, 40), 60, t);
            }
        }
        assert_eq!(auto.edge_cap(), Some(*AUTO_EDGES.last().unwrap()));
        // Still bad at the floor: nothing further to give, no churn.
        t += AUTO_DOWN_HOLD;
        for _ in 0..10 {
            assert_eq!(auto.observe(&fb(0, 0, 40), 60, t), None);
        }
        // Sustained health past the up-hold steps back up exactly one rung
        // per streak — slow up, so the picture never oscillates.
        t += AUTO_UP_HOLD;
        let mut stepped = None;
        for _ in 0..AUTO_GOOD_STREAK {
            stepped = auto.observe(&fb(58, 0, 0), 60, t);
        }
        assert_eq!(stepped, Some((960, 1280)));
        // And from the top rung, recovery lifts the cap entirely.
        let auto = AutoAdapt::new();
        let t1 = Instant::now();
        for _ in 0..AUTO_BAD_STREAK {
            auto.observe(&fb(0, 0, 0), 60, t1);
        }
        assert_eq!(auto.edge_cap(), Some(2560));
        let t2 = t1 + AUTO_UP_HOLD;
        let mut lifted = None;
        for _ in 0..AUTO_GOOD_STREAK {
            lifted = auto.observe(&fb(58, 0, 0), 60, t2);
        }
        assert_eq!(lifted, Some((2560, 0)));
        assert!(auto.edge_cap().is_none());
    }

    #[test]
    fn h264_ladder_picks_a_backend_that_emits_a_frame() {
        // Runs the real step-down ladder: it tries each hardware encoder
        // (NVENC/VA-API/…) and frame-send-tests it; on a box without a GPU
        // those open or test-fail and it lands on software openh264. Either
        // way the returned backend must actually encode a frame — that's the
        // contract the whole ladder exists to guarantee.
        let (w, h) = (640u32, 480u32);
        let mut codec = make_h264_codec(w, h, 30, Tune::default()).expect("a working backend");
        let grey = vec![128u8; (w * h) as usize + 2 * ((w / 2 * h / 2) as usize)];
        // First frame forced as an IDR — must come out non-empty.
        let out = codec
            .encode_i420(&grey, w as usize, h as usize, true)
            .expect("encode");
        assert!(
            out.is_some_and(|(d, key)| !d.is_empty() && key),
            "ladder backend ({}) must emit a keyframe",
            codec.label()
        );
    }

    #[test]
    fn fit_within_caps_the_long_edge_and_keeps_aspect() {
        assert_eq!(fit_within(3840, 2160, 1280), (1280, 720));
        assert_eq!(fit_within(1080, 1920, 1280), (720, 1280));
        // Already small → untouched (never upscaled).
        assert_eq!(fit_within(800, 600, 1280), (800, 600));
        assert_eq!(fit_within(0, 0, 1280), (0, 0));
    }

    // A buffer whose every byte is distinct, so any reorder (rotation) is
    // detectable and a pass-through is provably identical.
    fn distinct_rgba(w: u32, h: u32) -> Vec<u8> {
        (0..(w * h * 4)).map(|i| i as u8).collect()
    }

    #[test]
    fn orient_passes_through_at_zero_degrees() {
        // Every presentation backend (and DXGI at IDENTITY) reports 0: pixels
        // and dims are returned untouched, no rotation applied.
        let buf = distinct_rgba(4, 2);
        let (out, ow, oh) = orient_to_monitor(buf.clone(), 4, 2, 0);
        assert_eq!((ow, oh), (4, 2));
        assert_eq!(out, buf);
    }

    #[test]
    fn orient_rotates_90_and_270_clockwise_swapping_dims() {
        // Raw DXGI landscape scan-out (4×2) of a portrait panel: a 90 or 270
        // report drives one/three clockwise quarter-turns → 2×4 upright.
        let buf = distinct_rgba(4, 2);

        let (cw90, ow, oh) = orient_to_monitor(buf.clone(), 4, 2, 90);
        assert_eq!((ow, oh), (2, 4), "90: dims swap");
        assert_eq!(cw90, allmystuff_pixels::rotate_rgba(&buf, 4, 2, 1).0);

        let (cw270, ow, oh) = orient_to_monitor(buf.clone(), 4, 2, 270);
        assert_eq!((ow, oh), (2, 4), "270: dims swap");
        assert_eq!(cw270, allmystuff_pixels::rotate_rgba(&buf, 4, 2, 3).0);

        // 90 and 270 are distinct rotations (direction matters).
        assert_ne!(cw90, cw270);
    }

    #[test]
    fn orient_flips_180_preserving_dims() {
        // 180 is dimensionally invisible but reorders every pixel.
        let buf = distinct_rgba(4, 2);
        let (out, ow, oh) = orient_to_monitor(buf.clone(), 4, 2, 180);
        assert_eq!((ow, oh), (4, 2), "180: dims unchanged");
        assert_eq!(out, allmystuff_pixels::rotate_rgba(&buf, 4, 2, 2).0);
        assert_ne!(out, buf, "180: pixels reordered");
    }

    #[test]
    fn orient_maps_degrees_to_clockwise_turns() {
        // The committed DXGI mapping, end to end: turns = (deg/90) mod 4,
        // clockwise. 90→1, 180→2, 270→3 — the inverse of the display's CCW
        // rotation, matching the canonical Desktop Duplication sample.
        let buf = distinct_rgba(4, 2);
        for (deg, turns) in [(0u32, 0u8), (90, 1), (180, 2), (270, 3)] {
            let got = orient_to_monitor(buf.clone(), 4, 2, deg).0;
            let want = allmystuff_pixels::rotate_rgba(&buf, 4, 2, turns).0;
            assert_eq!(got, want, "deg={deg} must equal {turns} CW turns");
        }
    }

    #[test]
    fn jpeg_encoder_produces_a_jpeg() {
        let rgba = vec![128u8; 8 * 8 * 4];
        let jpeg = encode_jpeg(&rgba, 8, 8, JPEG_QUALITY).expect("encode");
        // SOI marker.
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
    }

    fn test_frame_encoder() -> FrameEncoder {
        FrameEncoder::new(
            "r",
            Tune::default(),
            Arc::new(AtomicBool::new(false)),
            AutoAdapt::new(),
        )
    }

    #[test]
    fn encoder_skips_unchanged_frames_and_keeps_seq_for_sent_ones() {
        let mut stats = StreamStats::new("r", VideoMode::Mjpeg);
        let mut enc = test_frame_encoder();
        let a = vec![10u8; 8 * 8 * 4];
        let first = enc
            .encode(a.clone(), 8, 8, &mut stats)
            .unwrap()
            .expect("first sends");
        assert_eq!(first.seq, 0);
        // Same pixels again, inside the refresh window → skipped.
        assert!(enc.encode(a.clone(), 8, 8, &mut stats).unwrap().is_none());
        assert_eq!(stats.static_skipped, 1, "the skip is counted");
        // Changed pixels → sent, with the next seq (skips don't burn one).
        let b = vec![200u8; 8 * 8 * 4];
        let second = enc
            .encode(b, 8, 8, &mut stats)
            .unwrap()
            .expect("change sends");
        assert_eq!(second.seq, 1);
        assert!(stats.bytes > 0, "emitted bytes are counted");
    }

    #[test]
    fn encoder_resends_after_the_refresh_interval() {
        let mut stats = StreamStats::new("r", VideoMode::Mjpeg);
        let mut enc = test_frame_encoder();
        let a = vec![10u8; 8 * 8 * 4];
        enc.encode(a.clone(), 8, 8, &mut stats)
            .unwrap()
            .expect("first sends");
        enc.last_sent = Some(Instant::now() - STATIC_REFRESH);
        let refreshed = enc.encode(a, 8, 8, &mut stats).unwrap();
        assert_eq!(refreshed.expect("refresh resends").seq, 1);
    }

    #[test]
    fn a_refresh_ask_resends_an_unchanged_frame_immediately() {
        let mut stats = StreamStats::new("r", VideoMode::Mjpeg);
        let refresh = Arc::new(AtomicBool::new(false));
        let mut enc = FrameEncoder::new("r", Tune::default(), refresh.clone(), AutoAdapt::new());
        let a = vec![10u8; 8 * 8 * 4];
        enc.encode(a.clone(), 8, 8, &mut stats)
            .unwrap()
            .expect("first sends");
        assert!(
            enc.encode(a.clone(), 8, 8, &mut stats).unwrap().is_none(),
            "unchanged inside the window is skipped"
        );
        refresh.store(true, Ordering::SeqCst);
        assert!(
            enc.encode(a.clone(), 8, 8, &mut stats).unwrap().is_some(),
            "the viewer's refresh ask overrides the unchanged gate"
        );
        assert!(!refresh.load(Ordering::SeqCst), "the ask is one-shot");
    }

    #[test]
    fn tune_clamps_each_dial_into_its_lane() {
        let t = Tune {
            max_edge: Some(800),
            bitrate: Some(12_000_000),
            fps: Some(60),
        };
        assert_eq!(t.fps(), 60);
        assert_eq!(t.h264_edge(), 800);
        // Both encodings honour the Res pick now (parity), each to the 4K
        // hard cap.
        assert_eq!(t.mjpeg_edge(), 800);
        // The Rate pick maps to a JPEG quality for MJPEG.
        assert_eq!(t.jpeg_quality(), mjpeg_quality_for(12_000_000));
        let big = Tune {
            max_edge: Some(9999),
            ..Tune::default()
        };
        assert_eq!(big.h264_edge(), 3840);
        assert_eq!(big.mjpeg_edge(), 3840);
        let auto = Tune::default();
        assert_eq!(auto.fps(), target_fps());
        assert_eq!(auto.h264_edge(), h264_max_edge());
        // Untuned MJPEG defaults to HD, and untuned quality is neutral.
        assert_eq!(auto.mjpeg_edge(), mjpeg_max_edge());
        assert_eq!(auto.jpeg_quality(), JPEG_QUALITY);
    }

    #[test]
    fn rate_pill_maps_to_a_monotonic_jpeg_quality() {
        // The console's Rate pills, softest → sharpest, never decreasing.
        let q: Vec<u8> = [4, 8, 15, 25, 40]
            .iter()
            .map(|m| mjpeg_quality_for(m * 1_000_000))
            .collect();
        assert!(q.windows(2).all(|w| w[0] <= w[1]), "monotonic: {q:?}");
        assert!(*q.first().unwrap() < JPEG_QUALITY); // "Speed" softer than neutral
        assert!(*q.last().unwrap() > JPEG_QUALITY); // "Quality" sharper
    }

    #[test]
    fn adaptive_idr_relaxes_only_on_confirmed_health() {
        let fresh = |decode_fails, queue_depth| {
            Some(RecvFeedback {
                recv_fps: 30,
                decode_fails,
                queue_depth,
                at: Instant::now(),
            })
        };
        // No feedback at all → the tight floor (today's behaviour).
        assert_eq!(adaptive_idr_ms(None), IDR_MS_TIGHT);
        // Clean + draining → relax.
        assert_eq!(adaptive_idr_ms(fresh(0, 0)), IDR_MS_RELAXED);
        assert_eq!(adaptive_idr_ms(fresh(0, 8)), IDR_MS_RELAXED);
        // Any decode failure → tighten.
        assert_eq!(adaptive_idr_ms(fresh(1, 0)), IDR_MS_TIGHT);
        // A backed-up queue → tighten.
        assert_eq!(adaptive_idr_ms(fresh(0, 9)), IDR_MS_TIGHT);
        // Stale feedback (viewer went quiet) → never holds it relaxed.
        let stale = Some(RecvFeedback {
            recv_fps: 30,
            decode_fails: 0,
            queue_depth: 0,
            at: Instant::now() - (FEEDBACK_FRESH + Duration::from_secs(1)),
        });
        assert_eq!(adaptive_idr_ms(stale), IDR_MS_TIGHT);
    }

    #[test]
    fn receiver_feedback_is_recorded_latest_wins_and_clears_with_the_route() {
        let vb = VideoBridge::new();
        assert!(vb.latest_feedback("r1").is_none());
        vb.note_feedback("r1", 28, 3, 1);
        let fb = vb.latest_feedback("r1").expect("recorded");
        assert_eq!((fb.recv_fps, fb.decode_fails, fb.queue_depth), (28, 3, 1));
        // A fresher report replaces the old one.
        vb.note_feedback("r1", 30, 0, 0);
        assert_eq!(vb.latest_feedback("r1").map(|f| f.decode_fails), Some(0));
        // Tearing the route down drops its feedback (no unbounded growth).
        vb.stop("r1");
        assert!(vb.latest_feedback("r1").is_none());
    }

    #[test]
    fn fit_within_even_forces_even_edges() {
        assert_eq!(fit_within_even(3024, 1964, 1920), (1920, 1246));
        assert_eq!(fit_within_even(1919, 1081, 1920), (1918, 1080));
        assert_eq!(fit_within_even(1, 1, 1920), (2, 2));
    }

    #[test]
    fn a_frameless_session_pump_errors_at_the_deadline() {
        let stop = AtomicBool::new(false);
        // Sender stays alive and silent: the pump must give up on the
        // deadline, not on a disconnect.
        let (_tx, rx) = mpsc::channel::<(Vec<u8>, u32, u32)>();
        let mut stats = StreamStats::new("r", VideoMode::Mjpeg);
        let mut enc = StreamEncoder::Mjpeg(test_frame_encoder());
        let mut reporter = StatusReporter::new(Arc::new(|_, _| {}));
        let err = pump_frames_with_stall(
            &stop,
            30,
            &rx,
            |f: (Vec<u8>, u32, u32)| f,
            &|_| true,
            &mut enc,
            &mut stats,
            &mut reporter,
            VideoStatusState::CameraFailed,
            Some(Duration::from_millis(300)),
        )
        .expect_err("a frameless session past the deadline must end");
        assert!(err.contains("no frame within"), "{err}");
    }

    #[test]
    fn h264_stream_emits_annexb_with_a_leading_idr() {
        let mut stats = StreamStats::new("r", VideoMode::H264);
        let mut enc = H264Stream::new(
            (64, 64),
            Tune::default(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(IDR_MS_TIGHT)),
            AutoAdapt::new(),
        )
        .expect("openh264 init");
        // A 64×64 solid frame → first unit out must be a key (IDR + SPS/PPS
        // in-band), Annex-B framed.
        let rgba = vec![128u8; 64 * 64 * 4];
        let packet = enc
            .encode(rgba, 64, 64, &mut stats)
            .expect("encode")
            .expect("first frame emits");
        let VideoPacket::H264 { data, .. } = packet else {
            panic!("h264 stream emitted a jpeg");
        };
        assert!(data.starts_with(&[0, 0, 0, 1]) || data.starts_with(&[0, 0, 1]));
        assert_eq!(stats.keyframes, 1, "first unit is a key");
        assert!(stats.bytes > 0);
    }
}
