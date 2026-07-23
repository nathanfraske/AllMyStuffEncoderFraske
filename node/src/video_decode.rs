//! The receive half of the H.264 path: a native openh264 decoder per
//! inbound display route, for console windows whose webview can't decode
//! (no WebCodecs — Linux WebKitGTK today) or whose WebCodecs decoder
//! stalled out. It turns access units into ready-to-paint RGBA frames the
//! window blits with `putImageData` — so H.264's bandwidth and 1920-edge
//! sharpness no longer depend on the webview, and the MJPEG fallback is
//! reserved for genuinely old peers instead of half our own platforms.
//!
//! Mirrors [`crate::video::VideoBridge`]'s shape: one dedicated thread per
//! route, owning the decoder state, fed by a bounded channel. H.264 deltas
//! chain, so the queue is never thinned mid-stream — when it overflows
//! (a wedged consumer), everything is dropped at once and decoding resumes
//! at the sender's next IDR (≤2 s away), exactly the recovery the webview
//! decoder uses. Decoded frames are handed on freshest-wins: the consumer
//! holds at most one undrained picture, which is what "minimum latency"
//! means at a pull-based sink.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// One access unit headed for a route's decoder (H.264 or HEVC — the
/// stream declares itself; see [`sniff_codec`]).
pub struct Au {
    /// Presentation time in µs (from the RTP clock).
    pub ts_us: u64,
    /// Whether the daemon flagged this unit a decode entry (IDR).
    pub key: bool,
    /// Annex-B bytes.
    pub data: Vec<u8>,
}

/// Queue-only metadata for the local development profiler. It is never part
/// of [`Au`]'s public/media shape, so the daemon and every wire format remain
/// byte-for-byte untouched.
struct QueuedAu {
    au: Au,
    profile: Option<QueuedProfile>,
}

struct QueuedProfile {
    frame_id: u64,
    enqueued_at: Option<Instant>,
}

impl QueuedAu {
    fn new(au: Au, inherited_frame_id: u64) -> Self {
        let profile = crate::pipeline_profile::enabled().then(|| QueuedProfile {
            frame_id: if inherited_frame_id == 0 {
                crate::pipeline_profile::next_frame_id()
            } else {
                inherited_frame_id
            },
            enqueued_at: crate::pipeline_profile::stamp(),
        });
        Self { au, profile }
    }

    fn into_au(self) -> Au {
        self.au
    }

    fn frame_id(&self) -> u64 {
        self.profile.as_ref().map_or(0, |p| p.frame_id)
    }
}

impl std::ops::Deref for QueuedAu {
    type Target = Au;

    fn deref(&self) -> &Self::Target {
        &self.au
    }
}

impl std::ops::DerefMut for QueuedAu {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.au
    }
}

/// Record one decoder-queue residence and return the profiler bookkeeping wall
/// time, so a surrounding coalesce-wait span can subtract observer work.
fn record_decoder_queue_wait(route_id: &str, au: &mut QueuedAu) -> Duration {
    let ts_us = au.au.ts_us;
    let Some(profile) = au.profile.as_mut() else {
        return Duration::ZERO;
    };
    let Some(enqueued_at) = profile.enqueued_at.take() else {
        return Duration::ZERO;
    };
    let ended = Instant::now();
    let observer_started = Instant::now();
    crate::pipeline_profile::record_at(
        route_id,
        profile.frame_id,
        Some(ts_us),
        crate::pipeline_profile::Stage::DecoderQueueWait,
        ended.saturating_duration_since(enqueued_at),
        ended,
    );
    observer_started.elapsed()
}

struct DeliveryObservation {
    frame_ts_us: u64,
    duration: Duration,
    ended: Instant,
}

fn deliver_profiled<F>(
    on_frame: &F,
    packet: Vec<u8>,
    _route_id: &str,
    frame_id: u64,
    frame_ts_us: u64,
    delivery_spent: &mut Duration,
    observations: &mut Vec<DeliveryObservation>,
) where
    F: Fn(Vec<u8>, u64, u64),
{
    let started = crate::pipeline_profile::stamp();
    on_frame(packet, frame_id, frame_ts_us);
    if let Some(started) = started {
        let ended = Instant::now();
        observations.push(DeliveryObservation {
            frame_ts_us,
            duration: ended.saturating_duration_since(started),
            ended,
        });
        // Subtract callback plus local observation bookkeeping from decode;
        // the observations themselves are recorded only after decode ends.
        *delivery_spent = delivery_spent.saturating_add(started.elapsed());
    }
}

/// Which codec an access unit opens with. H.264/HEVC are judged from the
/// first NAL header byte of a *parameter-set-led* unit (the decode
/// entries both encoders emit with repeated VPS/SPS/PPS); AV1 has no
/// Annex-B start codes at all — it's OBUs — so it's judged from a
/// leading sequence-header OBU instead ([`sniff_codec`]). Delta units
/// return `None`: the stream's codec is a property carried key-to-key,
/// not re-judged per frame.
#[derive(Clone, Copy, PartialEq, Debug)]
enum AuCodec {
    H264,
    Hevc,
    /// AV1 (OBU bitstream). **Decode is a STUB** — see [`Av1Rung`]: the
    /// sniff and dispatch seams exist so implementing AV1 is filling the
    /// rung bodies, not hunting for the branch points. No encoder emits
    /// AV1 yet, so this arm is dormant scaffolding.
    Av1,
}

fn sniff_codec(data: &[u8]) -> Option<AuCodec> {
    // First: the Annex-B path (H.264/HEVC always lead with a start code).
    let mut i = 0usize;
    let start_byte = loop {
        if i + 3 >= data.len() {
            // No start code anywhere — not H.264/HEVC. Fall through to the
            // OBU check: AV1 access units carry no start codes.
            return sniff_av1_obu(data);
        }
        if data[i] == 0 && data[i + 1] == 0 {
            if data[i + 2] == 1 {
                break data[i + 3];
            }
            if data[i + 2] == 0 && i + 4 < data.len() && data[i + 3] == 1 {
                break data[i + 4];
            }
        }
        i += 1;
    };
    // Exact bytes, not masked types: HEVC VPS/SPS/PPS at layer 0 are
    // precisely 0x40/0x42/0x44. A masked `(b>>1)&0x3F == 32` test would
    // also catch 0x41 — an H.264 P slice with nal_ref_idc 2, the byte
    // most delta AUs lead with — and flip a healthy H.264 stream's
    // decoder on every frame. (Caught in review; the byte-exact match is
    // collision-free because H.264 types 0/2/4 never lead an AU.)
    match start_byte {
        // VPS · SPS · PPS, plus IDR_W_RADL (0x26): an H.264 SEI would
        // share that byte only with nal_ref_idc=1, which the H.264 spec
        // forbids for SEI — so a conformant stream leading with 0x26 is
        // HEVC, and a paramless HEVC IDR still reads as a decode entry
        // instead of silently starving `waiting_key` (red team round 2).
        // IDR_N_LP (0x28) stays out: it collides with a legal H.264 PPS
        // byte, and our senders always lead IDRs with parameter sets.
        0x40 | 0x42 | 0x44 | 0x26 => Some(AuCodec::Hevc),
        _ => match start_byte & 0x1F {
            5 | 7 | 8 => Some(AuCodec::H264), // IDR · SPS · PPS
            _ => None,
        },
    }
}

/// Whether this access unit is self-describing decoder entry. The media
/// lane's `key` bit is H.264-shaped, so HEVC/AV1 parameter-set-led entries
/// must be recognized from their bytes before a receiver decides to hold
/// first-frame media.
pub(crate) fn is_decode_entry(data: &[u8]) -> bool {
    sniff_codec(data).is_some()
}

/// AV1 codec detection from a start-code-less AU — the OBU-aware seam.
/// An AV1 key access unit leads with a **sequence header OBU** (our
/// encoders emit it on every key frame, the AV1 analog of repeated
/// SPS/PPS). The low-overhead OBU header first byte is
/// `forbidden(1)=0 | type(4) | extension(1) | has_size(1) |
/// reserved(1)=0`; a leading temporal-delimiter (type 2) then
/// sequence-header (type 1) is the conformant key opening. Conservative
/// on purpose: only a genuine seq-header-led opening returns `Av1`, so a
/// truncated/odd H.264 chunk that reached here (no start code found)
/// stays `None` rather than being misread. Delta AUs (no seq header)
/// return `None` — codec carries from the key, like the Annex-B path.
///
/// STUB status: correct enough to route the stream to [`Av1Rung`], which
/// then reports "not yet implemented". Full OBU parsing lives in the
/// decoder, not here — this only names the codec.
fn sniff_av1_obu(data: &[u8]) -> Option<AuCodec> {
    /// One OBU header at `data[at]`: `(obu_type, next_offset)` when the
    /// header (and its optional leb128 size) parse; `None` past the end.
    fn obu_at(data: &[u8], at: usize) -> Option<(u8, usize)> {
        let hdr = *data.get(at)?;
        if hdr & 0x80 != 0 {
            return None; // forbidden bit set — not a valid OBU
        }
        let obu_type = (hdr >> 3) & 0x0f;
        let has_ext = hdr & 0x04 != 0;
        let has_size = hdr & 0x02 != 0;
        let mut p = at + 1 + usize::from(has_ext);
        if has_size {
            // leb128 size — skip it to reach the next OBU. Accumulate in
            // u64 (not usize): the final iteration shifts by 49, which
            // overflows a 32-bit usize on riscv32/armv7 (panic in debug,
            // masked-wrong in release) — u64 is valid on every target, and
            // `checked_add` keeps the 32-bit pointer add from wrapping.
            let mut size = 0u64;
            for shift in 0..8u32 {
                let byte = *data.get(p)?;
                p += 1;
                size |= u64::from(byte & 0x7f) << (shift * 7);
                if byte & 0x80 == 0 {
                    break;
                }
            }
            p = p.checked_add(usize::try_from(size).ok()?)?;
        }
        Some((obu_type, p))
    }
    // OBU_TEMPORAL_DELIMITER = 2, OBU_SEQUENCE_HEADER = 1. A key AU opens
    // with a seq header, optionally behind a temporal delimiter.
    let (t0, next) = obu_at(data, 0)?;
    if t0 == 1 {
        return Some(AuCodec::Av1); // seq-header-led
    }
    if t0 == 2 {
        if let Some((t1, _)) = obu_at(data, next) {
            if t1 == 1 {
                return Some(AuCodec::Av1); // temporal-delimiter then seq header
            }
        }
    }
    None
}

/// Production feeds one complete access unit per sample. Keep enough startup
/// headroom for a cold hardware decoder: field profiling on Windows measured
/// about 174 ms from the first H.264 AU through NVDEC session creation and its
/// first picture, which is already eleven 60 Hz arrivals. A three-AU queue
/// therefore guaranteed an overflow before the decoder could reach steady
/// state. Sixteen AUs cover that cold start plus ordinary scheduling jitter
/// while bounding the compressed backlog to about 267 ms at 60 fps.
///
/// H.264 remains ordered. On overflow the dependent chain is discarded and
/// decoding resumes from a complete parameter-set-led key AU; a healthy
/// same-codec decoder is retained so overload recovery does not pay another
/// cold-open penalty.
const MAX_PENDING_AUS: usize = 16;

/// The opt-in paced-slice experiment feeds multiple samples per AU. Retain its
/// sample-sized bound only in that explicit test mode; non-test production has
/// pacing disabled and always uses [`MAX_PENDING_AUS`].
#[cfg(all(windows, feature = "host"))]
const MAX_PENDING_PACED_SAMPLES: usize = 48;

fn pending_capacity() -> usize {
    #[cfg(all(windows, feature = "host"))]
    if crate::video::paced_slices_enabled() {
        return MAX_PENDING_PACED_SAMPLES;
    }
    MAX_PENDING_AUS
}

/// Idle boundary for paced NVDEC chunks. NVDEC treats END_OF_PICTURE
/// literally for both H.264 and HEVC, so same-timestamp samples must be
/// reassembled before either parser is fed. The sender may spread a whole AU
/// across roughly one 60 Hz frame and TURN can add jitter above its nominal
/// 8 ms inter-chunk cap, so 50 ms is the conservative last-picture fallback.
/// An active stream does not pay that timeout: the following RTP timestamp
/// closes the prior AU immediately. Each same-timestamp arrival resets it.
#[cfg(all(windows, feature = "host"))]
const NVDEC_CHUNK_IDLE: Duration = Duration::from_millis(50);

/// How often each decoder logs its dial-in line (matches the encode side).
const STATS_EVERY: Duration = Duration::from_secs(5);

/// A low-latency IP stream should surface pictures continuously. Thirty
/// successfully accepted access units with no picture is enough to distinguish
/// normal one-frame parser delay from a wedged/unsupported decoder.
const ZERO_OUTPUT_AU_LIMIT: u32 = 30;

struct RouteDecode {
    tx: mpsc::SyncSender<QueuedAu>,
    /// Set on queue overflow; the thread dumps to the next key unit.
    need_key: Arc<AtomicBool>,
    queue_stats: Arc<QueueStats>,
    /// A complete key AU returned by a full whole-AU queue. Keeping only this
    /// self-contained recovery point is safe; dependent deltas are never
    /// reordered or latest-wins thinned.
    recovery_entry: Arc<Mutex<Option<QueuedAu>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// Route-local queue instrumentation. The counters are appended to the
/// existing debug/opt-in decoder stats line; ordinary logs gain no chatter.
struct QueueStats {
    capacity: usize,
    pending: AtomicUsize,
    high_water: AtomicUsize,
    overflows: AtomicU64,
}

impl QueueStats {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            pending: AtomicUsize::new(0),
            high_water: AtomicUsize::new(0),
            overflows: AtomicU64::new(0),
        }
    }

    /// Reserve accounting before `try_send`, preventing a fast receiver from
    /// underflowing the depth counter before the sender records success.
    fn reserve_send(&self) -> usize {
        self.pending.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn sent(&self, reserved_depth: usize) {
        // The consumer can dequeue between reservation and send completion;
        // clamp the diagnostic high-water to the physical channel capacity.
        self.high_water
            .fetch_max(reserved_depth.min(self.capacity), Ordering::Relaxed);
    }

    fn send_failed(&self) {
        let before = self.pending.fetch_sub(1, Ordering::Relaxed);
        debug_assert!(before > 0, "decoder queue accounting underflow");
    }

    fn received(&self) {
        let before = self.pending.fetch_sub(1, Ordering::Relaxed);
        debug_assert!(before > 0, "decoder queue accounting underflow");
    }

    fn overflowed(&self) {
        self.overflows.fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> (usize, usize, u64) {
        (
            self.pending.load(Ordering::Relaxed),
            self.high_water.load(Ordering::Relaxed),
            self.overflows.load(Ordering::Relaxed),
        )
    }
}

impl Drop for RouteDecode {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[derive(Default)]
pub struct DecodeBridge {
    routes: Mutex<HashMap<String, RouteDecode>>,
}

impl DecodeBridge {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one access unit to `route_id`'s decoder, starting it on first
    /// use. `on_frame` receives each decoded picture as a ready IPC packet
    /// (see [`raw_ipc_packet`]); `on_glitch` fires when the decoder loses
    /// its place (corrupt unit, dumped backlog) so the caller can ask the
    /// sender to re-key. Both are only captured when this call starts the
    /// thread. A full queue dumps wholesale and re-keys — see module docs.
    pub fn feed<F, G>(&self, route_id: &str, au: Au, on_frame: F, on_glitch: G)
    where
        F: Fn(Vec<u8>) + Send + 'static,
        G: Fn(Option<u64>) + Send + 'static,
    {
        self.feed_profiled(
            route_id,
            au,
            0,
            move |packet, _frame_id, _frame_ts_us| on_frame(packet),
            on_glitch,
        )
    }

    /// Feed with a process-local profiler id inherited from the binary media
    /// pipe. The id never appears in [`Au`] or any serialized media shape.
    pub fn feed_profiled<F, G>(
        &self,
        route_id: &str,
        au: Au,
        profile_id: u64,
        on_frame: F,
        on_glitch: G,
    ) where
        F: Fn(Vec<u8>, u64, u64) + Send + 'static,
        G: Fn(Option<u64>) + Send + 'static,
    {
        let mut routes = self.routes.lock();
        let mut au = au;
        let mut profile_id = profile_id;
        let mut restarting = false;
        if let Some(entry) = routes.get_mut(route_id) {
            let reserved_depth = entry.queue_stats.reserve_send();
            match entry.tx.try_send(QueuedAu::new(au, profile_id)) {
                Ok(()) => {
                    entry.queue_stats.sent(reserved_depth);
                    return;
                }
                Err(mpsc::TrySendError::Full(returned)) => {
                    entry.queue_stats.send_failed();
                    entry.queue_stats.overflowed();
                    // Deltas past a full queue are useless without their
                    // predecessors. The decoder thread drops its whole stale
                    // backlog and requests a fresh key unit. In production's
                    // whole-AU framing, preserve a returned SPS/PPS-led entry
                    // so recovery need not wait on another round trip.
                    if entry.queue_stats.capacity == MAX_PENDING_AUS
                        && (returned.key || is_decode_entry(&returned.data))
                    {
                        *entry.recovery_entry.lock() = Some(returned);
                    }
                    entry.need_key.store(true, Ordering::SeqCst);
                    return;
                }
                Err(mpsc::TrySendError::Disconnected(returned)) => {
                    entry.queue_stats.send_failed();
                    // A panicked/returned decoder used to leave a permanent
                    // tombstone in `routes`: every later feed failed against
                    // the dead receiver and the display could never restart.
                    // The restarted queue represents this same AU. Preserve
                    // its inherited correlation id when profiling is active.
                    profile_id = returned.frame_id();
                    au = returned.into_au();
                    restarting = true;
                }
            }
        }
        if restarting {
            drop(routes.remove(route_id));
            tracing::warn!("native video decoder for {route_id} exited; restarting");
        }

        // Every fresh decoder starts in `waiting_key`, including the ordinary
        // first start when a user enables native decode mid-stream. Ask for a
        // clean entry whenever that first AU is a delta; limiting this to the
        // dead-worker restart path leaves Game/GDR's infinite GOP black until
        // an unrelated refresh happens.
        let request_key = !(au.key || is_decode_entry(&au.data));
        let request_ts_us = request_key.then_some(au.ts_us);
        let capacity = pending_capacity();
        let (tx, rx) = mpsc::sync_channel::<QueuedAu>(capacity);
        let queue_stats = Arc::new(QueueStats::new(capacity));
        // The receiver is live in this stack frame and the new queue is empty,
        // so the initial unit cannot fail or block.
        let reserved_depth = queue_stats.reserve_send();
        match tx.try_send(QueuedAu::new(au, profile_id)) {
            Ok(()) => queue_stats.sent(reserved_depth),
            Err(_) => {
                queue_stats.send_failed();
                return;
            }
        }
        let need_key = Arc::new(AtomicBool::new(false));
        let recovery_entry = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let id = route_id.to_string();
        let (nk, st) = (need_key.clone(), stop.clone());
        let (qs, recovery) = (queue_stats.clone(), recovery_entry.clone());
        let thread = std::thread::spawn(move || {
            if request_key {
                // Policy-v1 recovery is driven by timestamped frame-health
                // feedback. `None` is reserved for legacy callers and would
                // suppress both that report and the legacy Refresh fallback.
                on_glitch(request_ts_us);
            }
            run_decode(&st, &nk, &qs, &recovery, &id, rx, on_frame, on_glitch);
        });
        tracing::info!("native video decoder started for {route_id}");
        routes.insert(
            route_id.to_string(),
            RouteDecode {
                tx,
                need_key,
                queue_stats,
                recovery_entry,
                stop,
                thread: Some(thread),
            },
        );
    }

    /// Whether `route_id` currently has a live decoder.
    #[cfg(test)]
    pub fn is_running(&self, route_id: &str) -> bool {
        self.routes.lock().get(route_id).is_some_and(|route| {
            route
                .thread
                .as_ref()
                .is_some_and(|thread| !thread.is_finished())
        })
    }

    pub fn stop(&self, route_id: &str) {
        let retired = self.routes.lock().remove(route_id);
        if let Some(retired) = retired {
            retired.stop.store(true, Ordering::SeqCst);
            // The start line names the decode path in use; the stop is
            // routine teardown (every tab switch in native mode).
            tracing::debug!("native H.264 decoder stopped for {route_id}");
            // Route removal is the synchronization point for successor feeds.
            // Join the retired driver worker off the node-control path so a
            // slow decode call cannot block codec switching or another route.
            std::thread::spawn(move || drop(retired));
        }
    }
}

/// H.264 native-decode ladder. NVIDIA viewers take the driver's NVDEC path;
/// OpenH264 remains the portable floor and the automatic recovery rung after
/// an NVDEC initialization or runtime failure. `ALLMYSTUFF_H264_DECODER`
/// (`nvdec` | `openh264`) pins the first rung for field A/B diagnostics.
enum H264Rung {
    #[cfg(all(windows, feature = "host"))]
    Nvdec(crate::nvdec::NvdecH264),
    Software(openh264::decoder::Decoder),
}

/// Route-local open counts for the deterministic software bridge tests. This
/// makes decoder-session retention observable without adding any production
/// hook or depending on global environment variables.
#[cfg(test)]
static TEST_H264_SOFTWARE_OPENS: std::sync::LazyLock<Mutex<HashMap<String, usize>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

#[cfg(all(windows, feature = "host"))]
#[derive(Debug)]
enum FreshNvdecError {
    Open(String),
    Decode(crate::nvdec::NvdecError),
}

#[cfg(all(windows, feature = "host"))]
impl FreshNvdecError {
    fn is_bitstream_corrupt(&self) -> bool {
        matches!(self, Self::Decode(e) if e.is_bitstream_corrupt())
    }
}

#[cfg(all(windows, feature = "host"))]
impl std::fmt::Display for FreshNvdecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open(e) => f.write_str(e),
            Self::Decode(e) => std::fmt::Display::fmt(e, f),
        }
    }
}

/// Route-local runtime ladder state. One NVDEC delta error may just be a lost
/// packet, so the next key gets one fresh hardware session. If that fresh
/// session also fails on its first dependent picture, keep the route on
/// OpenH264 instead of cycling NVDEC→re-key forever. Any successful hardware
/// delta clears the strike.
#[derive(Default)]
struct H264RuntimePolicy {
    delta_failures: u8,
}

impl H264RuntimePolicy {
    const DEMOTE_AFTER: u8 = 2;

    #[cfg(all(windows, feature = "host"))]
    fn note_delta_failure(&mut self) -> u8 {
        self.delta_failures = self.delta_failures.saturating_add(1);
        self.delta_failures
    }

    #[cfg(all(windows, feature = "host"))]
    fn note_delta_success(&mut self) {
        self.delta_failures = 0;
    }

    #[cfg(all(windows, feature = "host"))]
    fn demote(&mut self) {
        self.delta_failures = Self::DEMOTE_AFTER;
    }

    fn requires_software(&self) -> bool {
        self.delta_failures >= Self::DEMOTE_AFTER
    }

    fn reset(&mut self) {
        self.delta_failures = 0;
    }
}

impl H264Rung {
    fn software_decoder() -> Result<openh264::decoder::Decoder, String> {
        openh264::decoder::Decoder::with_api_config(
            openh264::OpenH264API::from_source(),
            openh264::decoder::DecoderConfig::new(),
        )
        .map_err(|e| format!("OpenH264: {e}"))
    }

    fn software() -> Result<Self, String> {
        Self::software_decoder().map(Self::Software)
    }

    fn open(route_id: &str) -> Result<Self, String> {
        // Deterministic decoder-bridge tests must not depend on whether the
        // machine running them happens to have an NVIDIA adapter/driver. Keep
        // the seam route-local so parallel tests and production selection are
        // untouched.
        #[cfg(test)]
        if route_id.starts_with("test-software-") {
            *TEST_H264_SOFTWARE_OPENS
                .lock()
                .entry(route_id.to_owned())
                .or_default() += 1;
            return Self::software();
        }

        let force = std::env::var("ALLMYSTUFF_H264_DECODER")
            .map(|v| v.trim().to_ascii_lowercase())
            .unwrap_or_default();
        if matches!(force.as_str(), "openh264" | "software" | "sw") {
            return Self::software();
        }
        if !matches!(force.as_str(), "" | "nvdec") {
            tracing::warn!(
                "ALLMYSTUFF_H264_DECODER={force} isn't a rung (nvdec | openh264); using the ladder"
            );
        }

        #[cfg(all(windows, feature = "host"))]
        {
            match crate::nvdec::NvdecH264::open() {
                Ok(dec) => Ok(Self::Nvdec(dec)),
                Err(nvdec) => {
                    tracing::warn!(
                        "H.264 NVDEC unavailable for {route_id} ({nvdec}); falling back to OpenH264 software"
                    );
                    Self::software()
                        .map_err(|software| format!("NVDEC: {nvdec}; OpenH264: {software}"))
                }
            }
        }
        #[cfg(not(all(windows, feature = "host")))]
        {
            if force == "nvdec" {
                tracing::warn!(
                    "H.264 NVDEC requested for {route_id}, but this build has no NVDEC rung; using OpenH264 software"
                );
            }
            Self::software()
        }
    }

    fn label(&self) -> &'static str {
        match self {
            #[cfg(all(windows, feature = "host"))]
            Self::Nvdec(dec) => dec.label(),
            Self::Software(_) => "OpenH264 (software)",
        }
    }
}

fn decode_openh264_packet(
    dec: &mut openh264::decoder::Decoder,
    au: &[u8],
    ts_us: u64,
) -> Result<Option<(Vec<u8>, usize, usize)>, String> {
    use openh264::formats::YUVSource as _;

    let Some(yuv) = dec.decode(au).map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    let (w, h) = yuv.dimensions();
    if w == 0 || h == 0 {
        return Ok(None);
    }
    let mut packet = raw_ipc_packet(ts_us, w as u32, h as u32);
    yuv.write_rgba8(&mut packet[crate::mesh::VIDEO_IPC_HEADER_LEN..]);
    Ok(Some((packet, w, h)))
}

#[cfg(all(windows, feature = "host"))]
fn emit_nv_frames<F>(
    pics: Vec<crate::nvdec::NvFrame>,
    on_frame: &F,
    route_id: &str,
    frame_id: u64,
    delivery_spent: &mut Duration,
    observations: &mut Vec<DeliveryObservation>,
) -> (u32, Option<(usize, usize)>)
where
    F: Fn(Vec<u8>, u64, u64),
{
    let mut emitted = 0u32;
    let mut dims = None;
    for f in pics {
        let (w, h) = (f.width as usize, f.height as usize);
        if w == 0 || h == 0 {
            continue;
        }
        let mut packet = raw_ipc_packet(f.ts_us, f.width, f.height);
        crate::nvdec::nv12_to_rgba(
            &f.nv12,
            w,
            h,
            &mut packet[crate::mesh::VIDEO_IPC_HEADER_LEN..],
        );
        emitted += 1;
        dims = Some((w, h));
        deliver_profiled(
            on_frame,
            packet,
            route_id,
            frame_id,
            f.ts_us,
            delivery_spent,
            observations,
        );
    }
    (emitted, dims)
}

/// The HEVC hardware ladder: NVDEC where the NVIDIA driver lives (the
/// proven rung, CUDA-warm on every field pair to date), else D3D11VA —
/// the vendor-neutral `ID3D11VideoDecoder` rung that makes an AMD/Intel/
/// iGPU viewer a full Studio·Lossless citizen. `ALLMYSTUFF_HEVC_DECODER`
/// (`nvdec` | `d3d11va`) pins a rung for A/B runs and demos; both rungs
/// speak the same seam, so the bridge below can't tell them apart.
#[cfg(all(windows, feature = "host"))]
enum HevcRung {
    Nvdec(crate::nvdec::NvdecHevc),
    // Boxed: the DXVA session carries its parser stores inline and would
    // otherwise dwarf the enum every H.264 route also instantiates.
    Dxva(Box<crate::d3d11va::D3d11vaHevc>),
}

#[cfg(all(windows, feature = "host"))]
struct HevcDecodeError {
    message: String,
    known_corrupt: bool,
}

#[cfg(all(windows, feature = "host"))]
impl std::fmt::Display for HevcDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

#[cfg(all(windows, feature = "host"))]
#[derive(Default)]
struct HevcRuntimePolicy {
    prefer_dxva: bool,
}

#[cfg(all(windows, feature = "host"))]
impl HevcRuntimePolicy {
    fn demote_from_nvdec(&mut self, nvdec_pinned: bool) -> bool {
        if nvdec_pinned {
            return false;
        }
        self.prefer_dxva = true;
        true
    }

    fn reset(&mut self) {
        self.prefer_dxva = false;
    }
}

#[cfg(all(windows, feature = "host"))]
impl HevcRung {
    fn dxva() -> Result<Self, String> {
        crate::d3d11va::D3d11vaHevc::open().map(|d| Self::Dxva(Box::new(d)))
    }

    fn open() -> Result<Self, String> {
        let force = std::env::var("ALLMYSTUFF_HEVC_DECODER")
            .map(|v| v.trim().to_ascii_lowercase())
            .unwrap_or_default();
        match force.as_str() {
            "nvdec" => crate::nvdec::NvdecHevc::open().map(Self::Nvdec),
            "d3d11va" | "dxva" => Self::dxva(),
            other => {
                if !other.is_empty() {
                    tracing::warn!(
                        "ALLMYSTUFF_HEVC_DECODER={other} isn't a rung (nvdec | d3d11va); using the ladder"
                    );
                }
                crate::nvdec::NvdecHevc::open()
                    .map(Self::Nvdec)
                    .or_else(|nv| Self::dxva().map_err(|dx| format!("NVDEC: {nv}; D3D11VA: {dx}")))
            }
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Nvdec(d) => d.label(),
            Self::Dxva(d) => d.label(),
        }
    }

    fn is_nvdec(&self) -> bool {
        matches!(self, Self::Nvdec(_))
    }

    fn decode(
        &mut self,
        au: &[u8],
        ts_us: u64,
    ) -> Result<Vec<crate::nvdec::NvFrame>, HevcDecodeError> {
        match self {
            Self::Nvdec(d) => d.decode_typed(au, ts_us).map_err(|e| HevcDecodeError {
                known_corrupt: e.is_bitstream_corrupt(),
                message: e.to_string(),
            }),
            Self::Dxva(d) => d.decode(au, ts_us).map_err(|e| HevcDecodeError {
                known_corrupt: false,
                message: e,
            }),
        }
    }
}

/// The AV1 hardware ladder — **STUB**, the twin of [`HevcRung`]. When AV1
/// lands, both boxes support it: NVDEC AV1 (`codec = 11`, Blackwell/Ada
/// decode; every recent NVIDIA), D3D11VA AV1 (`AV1 VLD Profile0`, RDNA
/// and Intel Xe), and a `dav1d` software floor for viewers without
/// hardware. The dispatch shape is here so implementation fills the rung
/// bodies (`crate::nvdec::NvdecAv1`, `crate::d3d11va::D3d11vaAv1`) rather
/// than re-deriving the ladder. `ALLMYSTUFF_AV1_DECODER` will pin a rung
/// exactly as HEVC's dial does. Today `open` reports the honest "not yet
/// implemented" and the bridge falls soft (re-key ask), so a stray AV1
/// stream never crashes a viewer — it just doesn't paint.
#[cfg(all(windows, feature = "host"))]
enum Av1Rung {
    Nvdec(crate::nvdec::NvdecAv1),
    Dxva(Box<crate::d3d11va::D3d11vaAv1>),
}

#[cfg(all(windows, feature = "host"))]
impl Av1Rung {
    fn open() -> Result<Self, String> {
        let force = std::env::var("ALLMYSTUFF_AV1_DECODER")
            .map(|v| v.trim().to_ascii_lowercase())
            .unwrap_or_default();
        match force.as_str() {
            "nvdec" => crate::nvdec::NvdecAv1::open().map(Self::Nvdec),
            "d3d11va" | "dxva" => {
                crate::d3d11va::D3d11vaAv1::open().map(|d| Self::Dxva(Box::new(d)))
            }
            _ => crate::nvdec::NvdecAv1::open()
                .map(Self::Nvdec)
                .or_else(|nv| {
                    crate::d3d11va::D3d11vaAv1::open()
                        .map(|d| Self::Dxva(Box::new(d)))
                        .map_err(|dx| format!("NVDEC-AV1: {nv}; D3D11VA-AV1: {dx}"))
                }),
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Nvdec(d) => d.label(),
            Self::Dxva(d) => d.label(),
        }
    }

    fn decode(&mut self, au: &[u8], ts_us: u64) -> Result<Vec<crate::nvdec::NvFrame>, String> {
        match self {
            Self::Nvdec(d) => d.decode(au, ts_us),
            Self::Dxva(d) => d.decode(au, ts_us),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_decode<F, G>(
    stop: &AtomicBool,
    need_key: &AtomicBool,
    queue_stats: &QueueStats,
    recovery_entry: &Mutex<Option<QueuedAu>>,
    route_id: &str,
    rx: mpsc::Receiver<QueuedAu>,
    on_frame: F,
    on_glitch: G,
) where
    F: Fn(Vec<u8>, u64, u64),
    G: Fn(Option<u64>),
{
    // The decode thread is the viewer's media plane — same priority/EcoQoS
    // treatment as the host's capture and encode threads, so a loaded
    // viewer box doesn't stutter the picture it's watching.
    crate::os_perf::boost_media_thread();

    // The route's decoder, whichever codec the stream declared at its
    // last key unit. H.264 = NVDEC → OpenH264; HEVC = NVDEC → D3D11VA
    // (Windows: nvcuvid where the NVIDIA driver lives, else the vendor-neutral
    // `ID3D11VideoDecoder` any GPU driver exposes — AMD/Intel/iGPU viewers
    // included).
    enum Active {
        H264(H264Rung),
        #[cfg(all(windows, feature = "host"))]
        Hevc(HevcRung),
        /// AV1 — STUB rung (see [`Av1Rung`]); the arm exists so the codec
        /// threads through decode cleanly, and reports "not implemented".
        #[cfg(all(windows, feature = "host"))]
        Av1(Av1Rung),
    }
    let mut decoder: Option<Active> = None;
    let mut stream_codec = AuCodec::H264;
    // Decode entry is a key unit; deltas before one can't decode.
    let mut waiting_key = true;
    let mut last_err: Option<Instant> = None;
    let (mut frames, mut spent, mut out_dims, mut since) =
        (0u32, Duration::ZERO, (0usize, 0usize), Instant::now());
    // Compressed bytes fed this window — the wire layer's bandwidth at
    // the decoder's door (the nv12/rgba layers derive from frames×dims).
    let mut in_bytes = 0u64;
    let mut deferred_au: Option<QueuedAu> = None;
    #[cfg(all(windows, feature = "host"))]
    let mut logged_nvdec_coalesce = false;
    let mut h264_runtime = H264RuntimePolicy::default();
    #[cfg(all(windows, feature = "host"))]
    let mut hevc_runtime = HevcRuntimePolicy::default();
    #[cfg(all(windows, feature = "host"))]
    let hevc_nvdec_pinned = std::env::var("ALLMYSTUFF_HEVC_DECODER")
        .is_ok_and(|v| v.trim().eq_ignore_ascii_case("nvdec"));
    let mut zero_output_since: Option<Instant> = None;
    let mut zero_output_aus = 0u32;
    let mut zero_output_bytes = 0u64;
    let mut zero_output_last_ts_us: Option<u64> = None;
    let mut last_queue_reset: Option<Instant> = None;

    while !stop.load(Ordering::SeqCst) {
        // A bounded wait keeps the stop flag responsive on a quiet stream.
        let mut au = if let Some(au) = deferred_au.take() {
            au
        } else {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(au) => {
                    queue_stats.received();
                    au
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if zero_output_aus > 0
                        && zero_output_since.is_some_and(|t| t.elapsed() >= STATS_EVERY)
                    {
                        tracing::warn!(
                            "video decoder for {route_id} accepted {zero_output_aus} AU(s) / {zero_output_bytes} bytes for {STATS_EVERY:?} but produced no picture; retiring the rung"
                        );
                        #[cfg(all(windows, feature = "host"))]
                        match decoder.as_ref() {
                            Some(Active::H264(H264Rung::Nvdec(_))) => h264_runtime.demote(),
                            Some(Active::Hevc(rung)) if rung.is_nvdec() => {
                                let _ = hevc_runtime.demote_from_nvdec(hevc_nvdec_pinned);
                            }
                            _ => {}
                        }
                        decoder = None;
                        waiting_key = true;
                        on_glitch(zero_output_last_ts_us);
                        zero_output_since = None;
                        zero_output_aus = 0;
                        zero_output_bytes = 0;
                        zero_output_last_ts_us = None;
                    }
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        };
        let _ = record_decoder_queue_wait(route_id, &mut au);
        let decoder_prepare_started = crate::pipeline_profile::stamp();
        in_bytes += au.data.len() as u64;
        if need_key.swap(false, Ordering::SeqCst) {
            // The feeder overflowed: drain the stale backlog and wait for
            // the sender's next IDR — same recovery as a decode error. A
            // complete key AU already in hand/queue is retained as the only
            // safe newest-wins unit; dependent deltas stay ordered or drop as
            // a chain, never individually.
            let lost_ts_us = au.ts_us;
            let mut saved_entry = if queue_stats.capacity == MAX_PENDING_AUS
                && (au.key || is_decode_entry(&au.data))
            {
                Some(au)
            } else {
                None
            };
            while let Ok(candidate) = rx.try_recv() {
                queue_stats.received();
                if queue_stats.capacity == MAX_PENDING_AUS
                    && (candidate.key || is_decode_entry(&candidate.data))
                {
                    saved_entry = Some(candidate);
                }
            }
            if let Some(returned_entry) = recovery_entry.lock().take() {
                saved_entry = Some(returned_entry);
            }
            if last_queue_reset.is_none_or(|t| t.elapsed() >= STATS_EVERY) {
                let recovery = if saved_entry.is_some() {
                    "retaining the newest complete key entry"
                } else {
                    "requesting a key"
                };
                tracing::warn!(
                    "video decoder queue overflow for {route_id}; dropped stale access units and {recovery}"
                );
                last_queue_reset = Some(Instant::now());
            }
            // The compressed dependency chain is stale, not the decoder
            // session. Keeping a healthy same-codec decoder avoids a cold
            // NVDEC/OpenH264 rebuild on every overload. A key AU below can
            // still morph codecs or resize through the ordinary sniff/sequence
            // path, which performs the required targeted rebuild itself.
            waiting_key = true;
            zero_output_since = None;
            zero_output_aus = 0;
            zero_output_bytes = 0;
            zero_output_last_ts_us = None;
            if let Some(entry) = saved_entry {
                // Re-enter immediately from the newest complete SPS/PPS/key
                // unit. It is processed before any later queued delta.
                deferred_au = Some(entry);
            } else {
                // Name the AU whose chain was abandoned. Policy-v1 peers use
                // the existing VideoFeedback data-channel message to produce
                // an immediate GDR wave/IDR; `None` accidentally suppressed
                // that request and made recovery wait for the periodic key.
                on_glitch(Some(lost_ts_us));
            }
            continue;
        }
        // A parameter-set-led unit is a decode entry in both codecs and
        // carries the stream's codec declaration — trusted over `au.key`,
        // whose daemon-side derivation is H.264-shaped and blind to HEVC.
        let sniffed = sniff_codec(&au.data);
        let is_key = au.key || sniffed.is_some();
        if waiting_key && !is_key {
            continue;
        }
        if let Some(c) = sniffed {
            if stream_codec != c {
                // Codec morph mid-route (posture change): the old decoder
                // has nothing valid to say about the new stream.
                stream_codec = c;
                decoder = None;
                h264_runtime.reset();
                #[cfg(all(windows, feature = "host"))]
                hevc_runtime.reset();
                zero_output_since = None;
                zero_output_aus = 0;
                zero_output_bytes = 0;
                zero_output_last_ts_us = None;
            }
        }
        if decoder.is_none() {
            let built = match stream_codec {
                AuCodec::H264 => {
                    let opened = if h264_runtime.requires_software() {
                        tracing::warn!(
                            "H.264 decoder for {route_id}: repeated NVDEC delta failures; keeping this route on OpenH264 software"
                        );
                        H264Rung::software()
                    } else {
                        H264Rung::open(route_id)
                    };
                    opened.map(|dec| {
                        tracing::info!("H.264 decoder for {route_id}: {}", dec.label());
                        Active::H264(dec)
                    })
                }
                #[cfg(all(windows, feature = "host"))]
                AuCodec::Hevc => {
                    let opened = if hevc_runtime.prefer_dxva {
                        tracing::warn!(
                            "HEVC decoder for {route_id}: prior NVDEC runtime failure; keeping this route on D3D11VA"
                        );
                        HevcRung::dxva()
                    } else {
                        HevcRung::open()
                    };
                    opened.map(|dec| {
                        // The one line that says which glass this stream
                        // crosses — the cross-vendor story in the log.
                        tracing::info!("HEVC decoder for {route_id}: {}", dec.label());
                        Active::Hevc(dec)
                    })
                }
                #[cfg(not(all(windows, feature = "host")))]
                AuCodec::Hevc => Err("no HEVC decoder on this platform".to_string()),
                // AV1 — STUB: opens the ladder, which reports not-yet-
                // implemented; the bridge falls soft. Dormant until an
                // encoder emits AV1.
                #[cfg(all(windows, feature = "host"))]
                AuCodec::Av1 => Av1Rung::open().map(|dec| {
                    tracing::info!("AV1 decoder for {route_id}: {}", dec.label());
                    Active::Av1(dec)
                }),
                #[cfg(not(all(windows, feature = "host")))]
                AuCodec::Av1 => Err("no AV1 decoder on this platform".to_string()),
            };
            match built {
                Ok(d) => decoder = Some(d),
                Err(e) => {
                    // Init trouble is permanent for this stream — say
                    // so once a window, keep draining so the route
                    // isn't backed up behind us.
                    if last_err.is_none_or(|t| t.elapsed() >= STATS_EVERY) {
                        last_err = Some(Instant::now());
                        tracing::warn!("decoder init for {route_id} failed: {e}");
                    }
                    on_glitch(Some(au.ts_us));
                    continue;
                }
            }
        }

        crate::pipeline_profile::record_since(
            route_id,
            au.frame_id(),
            Some(au.ts_us),
            crate::pipeline_profile::Stage::DecoderPrepareBusy,
            decoder_prepare_started,
        );

        // NVDEC interprets END_OF_PICTURE literally in both codecs and would
        // display every paced slice as a partial picture. Collect
        // same-timestamp samples until the next timestamp or the sender's
        // bounded pacing window expires, then submit exactly one complete AU
        // to the driver. D3D11VA owns its own HEVC slice assembly and must keep
        // receiving the chunks independently.
        #[cfg(all(windows, feature = "host"))]
        if crate::video::paced_slices_enabled()
            && matches!(
                decoder.as_ref(),
                Some(Active::H264(H264Rung::Nvdec(_)))
                    | Some(Active::Hevc(HevcRung::Nvdec(_)))
                    | Some(Active::Av1(Av1Rung::Nvdec(_)))
            )
        {
            let coalesce_started = crate::pipeline_profile::stamp();
            let mut observer_spent = Duration::ZERO;
            let mut deadline = Instant::now() + NVDEC_CHUNK_IDLE;
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                match rx.recv_timeout(remaining) {
                    Ok(mut next) if next.ts_us == au.ts_us => {
                        queue_stats.received();
                        observer_spent = observer_spent
                            .saturating_add(record_decoder_queue_wait(route_id, &mut next));
                        in_bytes += next.data.len() as u64;
                        au.key |= next.key;
                        au.data.extend_from_slice(&next.data);
                        deadline = Instant::now() + NVDEC_CHUNK_IDLE;
                    }
                    Ok(next) => {
                        queue_stats.received();
                        deferred_au = Some(next);
                        break;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            if let Some(started) = coalesce_started {
                let ended = Instant::now();
                crate::pipeline_profile::record_at(
                    route_id,
                    au.frame_id(),
                    Some(au.ts_us),
                    crate::pipeline_profile::Stage::DecoderCoalesceWait,
                    ended
                        .saturating_duration_since(started)
                        .saturating_sub(observer_spent),
                    ended,
                );
            }
            if !logged_nvdec_coalesce {
                logged_nvdec_coalesce = true;
                tracing::info!(
                    "NVDEC for {route_id} ({stream_codec:?}): coalescing paced same-timestamp samples ({} ms idle boundary)",
                    NVDEC_CHUNK_IDLE.as_millis()
                );
            }
            // Overflow may have been signaled while we were collecting the
            // train. Apply the same wholesale freshness reset before decode.
            if need_key.swap(false, Ordering::SeqCst) {
                let lost_ts_us = au.ts_us;
                while let Ok(_dropped) = rx.try_recv() {
                    queue_stats.received();
                }
                recovery_entry.lock().take();
                if last_queue_reset.is_none_or(|t| t.elapsed() >= STATS_EVERY) {
                    tracing::warn!(
                        "video decoder queue overflow for {route_id} while coalescing; dropped stale access units and requesting a key"
                    );
                    last_queue_reset = Some(Instant::now());
                }
                deferred_au = None;
                waiting_key = true;
                zero_output_since = None;
                zero_output_aus = 0;
                zero_output_bytes = 0;
                zero_output_last_ts_us = None;
                on_glitch(Some(lost_ts_us));
                continue;
            }
        }

        let dec = decoder.as_mut().expect("decoder was initialized above");
        let t0 = Instant::now();
        let profile_id = au.frame_id();
        let mut delivery_spent = Duration::ZERO;
        let mut delivery_observations = Vec::new();
        let frames_before = frames;
        let mut broke: Option<String> = None;
        let mut zero_output_failure = false;
        match dec {
            Active::H264(rung) => match rung {
                H264Rung::Software(dec) => {
                    match decode_openh264_packet(dec, &au.data, au.ts_us) {
                        Ok(picture) => {
                            // A clean key re-arms the stream even when this is
                            // a headers-only chunk and no picture surfaced yet.
                            waiting_key = false;
                            if let Some((packet, w, h)) = picture {
                                spent += t0.elapsed();
                                frames += 1;
                                out_dims = (w, h);
                                deliver_profiled(
                                    &on_frame,
                                    packet,
                                    route_id,
                                    profile_id,
                                    au.ts_us,
                                    &mut delivery_spent,
                                    &mut delivery_observations,
                                );
                            }
                        }
                        Err(e) => broke = Some(format!("H.264/OpenH264: {e}")),
                    }
                }
                #[cfg(all(windows, feature = "host"))]
                H264Rung::Nvdec(hw) => match hw.decode_typed(&au.data, au.ts_us) {
                    Ok(pics) => {
                        waiting_key = false;
                        let (n, dims) = emit_nv_frames(
                            pics,
                            &on_frame,
                            route_id,
                            profile_id,
                            &mut delivery_spent,
                            &mut delivery_observations,
                        );
                        if !is_key && n > 0 {
                            h264_runtime.note_delta_success();
                        }
                        if let Some(dims) = dims {
                            frames += n;
                            out_dims = dims;
                            spent += t0.elapsed();
                        }
                    }
                    Err(first) if first.is_bitstream_corrupt() => {
                        // The driver positively identified this picture/AU as
                        // damaged. Do not retry the same known-bad bytes through
                        // fresh hardware or OpenH264: either could conceal and
                        // paint them. The common `broke` path resets and asks
                        // for a clean entry without blaming the hardware rung.
                        broke = Some(format!("H.264/NVDEC bitstream corruption: {first}"));
                    }
                    Err(first) if is_key => {
                        // A resize/retune intentionally invalidates the old
                        // NVDEC session. Rebuild and retry this SAME key once;
                        // only a second failure earns software demotion.
                        let retry = crate::nvdec::NvdecH264::open()
                            .map_err(FreshNvdecError::Open)
                            .and_then(|mut fresh| {
                                fresh
                                    .decode_typed(&au.data, au.ts_us)
                                    .map(|pics| (fresh, pics))
                                    .map_err(FreshNvdecError::Decode)
                            });
                        match retry {
                            Ok((fresh, pics)) => {
                                waiting_key = false;
                                let (n, dims) = emit_nv_frames(
                                    pics,
                                    &on_frame,
                                    route_id,
                                    profile_id,
                                    &mut delivery_spent,
                                    &mut delivery_observations,
                                );
                                if let Some(dims) = dims {
                                    frames += n;
                                    out_dims = dims;
                                    spent += t0.elapsed();
                                }
                                *rung = H264Rung::Nvdec(fresh);
                                tracing::info!(
                                    "H.264 NVDEC session for {route_id} rebuilt at a key unit after: {first}"
                                );
                            }
                            Err(retry) if retry.is_bitstream_corrupt() => {
                                broke = Some(format!(
                                    "H.264/NVDEC fresh-session bitstream corruption: {retry}"
                                ));
                            }
                            Err(retry) => match H264Rung::software_decoder() {
                                Ok(mut software) => {
                                    match decode_openh264_packet(&mut software, &au.data, au.ts_us)
                                    {
                                        Ok(picture) => {
                                            waiting_key = false;
                                            if let Some((packet, w, h)) = picture {
                                                frames += 1;
                                                out_dims = (w, h);
                                                spent += t0.elapsed();
                                                deliver_profiled(
                                                    &on_frame,
                                                    packet,
                                                    route_id,
                                                    profile_id,
                                                    au.ts_us,
                                                    &mut delivery_spent,
                                                    &mut delivery_observations,
                                                );
                                            }
                                            *rung = H264Rung::Software(software);
                                            h264_runtime.demote();
                                            tracing::warn!(
                                            "H.264 NVDEC failed twice for {route_id} ({first}; retry: {retry}); continuing on OpenH264 software"
                                        );
                                        }
                                        Err(software) => {
                                            broke = Some(format!(
                                            "H.264 NVDEC: {first}; fresh NVDEC: {retry}; OpenH264: {software}"
                                        ));
                                        }
                                    }
                                }
                                Err(software) => {
                                    broke = Some(format!(
                                        "H.264 NVDEC: {first}; fresh NVDEC: {retry}; OpenH264 init: {software}"
                                    ));
                                }
                            },
                        }
                    }
                    Err(e) => {
                        let failures = h264_runtime.note_delta_failure();
                        tracing::warn!(
                            "H.264 NVDEC delta failed for {route_id} (strike {failures}/{}); requesting a key before {}: {e}",
                            H264RuntimePolicy::DEMOTE_AFTER,
                            if h264_runtime.requires_software() {
                                "software demotion"
                            } else {
                                "one fresh hardware retry"
                            }
                        );
                        broke = Some(format!("H.264/NVDEC: {e}"));
                    }
                },
            },
            #[cfg(all(windows, feature = "host"))]
            Active::Hevc(rung) => match rung.decode(&au.data, au.ts_us) {
                Ok(pics) => {
                    waiting_key = false;
                    let (n, dims) = emit_nv_frames(
                        pics,
                        &on_frame,
                        route_id,
                        profile_id,
                        &mut delivery_spent,
                        &mut delivery_observations,
                    );
                    if let Some(dims) = dims {
                        frames += n;
                        out_dims = dims;
                        spent += t0.elapsed();
                    }
                }
                Err(first) if first.known_corrupt => {
                    // Keep the NVIDIA ladder selected, but discard this known
                    // damaged entry/dependent picture and use common re-key
                    // recovery. Retrying the same bytes on D3D11VA could paint
                    // another decoder's unobservable concealment.
                    broke = Some(format!("HEVC/NVDEC bitstream corruption: {first}"));
                }
                Err(first) => {
                    let was_nvdec = rung.is_nvdec();
                    let demoted = was_nvdec && hevc_runtime.demote_from_nvdec(hevc_nvdec_pinned);
                    if demoted && is_key {
                        // The entry AU is still in hand: step to the
                        // vendor-neutral hardware rung and retry it instead of
                        // dropping the only clean recovery point.
                        let retry = HevcRung::dxva().and_then(|mut fresh| {
                            fresh
                                .decode(&au.data, au.ts_us)
                                .map(|pics| (fresh, pics))
                                .map_err(|e| e.to_string())
                        });
                        match retry {
                            Ok((fresh, pics)) => {
                                waiting_key = false;
                                let (n, dims) = emit_nv_frames(
                                    pics,
                                    &on_frame,
                                    route_id,
                                    profile_id,
                                    &mut delivery_spent,
                                    &mut delivery_observations,
                                );
                                if let Some(dims) = dims {
                                    frames += n;
                                    out_dims = dims;
                                    spent += t0.elapsed();
                                }
                                *rung = fresh;
                                tracing::warn!(
                                    "HEVC NVDEC runtime failure for {route_id} ({first}); continuing on D3D11VA hardware"
                                );
                            }
                            Err(retry) => {
                                broke =
                                    Some(format!("HEVC/NVDEC: {first}; D3D11VA retry: {retry}"));
                            }
                        }
                    } else {
                        if demoted {
                            tracing::warn!(
                                "HEVC NVDEC runtime failure for {route_id}; requesting a key before D3D11VA demotion: {first}"
                            );
                        } else if was_nvdec && hevc_nvdec_pinned {
                            tracing::warn!(
                                "HEVC NVDEC runtime failure for {route_id}; A/B pin keeps the NVDEC rung: {first}"
                            );
                        }
                        broke = Some(format!("HEVC: {first}"));
                    }
                }
            },
            // AV1 shares HEVC's NV12→RGBA output shape (the rung returns
            // `NvFrame`), so the paint path is identical — only the rung
            // body differs. STUB today: `decode` returns Err, the stream
            // re-keys, nothing paints. Fill `Av1Rung`'s bodies to light
            // this up.
            #[cfg(all(windows, feature = "host"))]
            Active::Av1(dec) => match dec.decode(&au.data, au.ts_us) {
                Ok(pics) => {
                    waiting_key = false;
                    let mut emitted = false;
                    for f in pics {
                        let (w, h) = (f.width as usize, f.height as usize);
                        if w == 0 || h == 0 {
                            continue;
                        }
                        let mut packet = raw_ipc_packet(f.ts_us, f.width, f.height);
                        crate::nvdec::nv12_to_rgba(
                            &f.nv12,
                            w,
                            h,
                            &mut packet[crate::mesh::VIDEO_IPC_HEADER_LEN..],
                        );
                        frames += 1;
                        emitted = true;
                        out_dims = (w, h);
                        deliver_profiled(
                            &on_frame,
                            packet,
                            route_id,
                            profile_id,
                            f.ts_us,
                            &mut delivery_spent,
                            &mut delivery_observations,
                        );
                    }
                    if emitted {
                        spent += t0.elapsed();
                    }
                }
                Err(e) => broke = Some(format!("AV1: {e}")),
            },
        }
        let decode_ended = Instant::now();
        crate::pipeline_profile::record_at(
            route_id,
            profile_id,
            Some(au.ts_us),
            crate::pipeline_profile::Stage::DecodeBusy,
            decode_ended
                .saturating_duration_since(t0)
                .saturating_sub(delivery_spent),
            decode_ended,
        );
        for observation in delivery_observations {
            crate::pipeline_profile::record_at(
                route_id,
                profile_id,
                Some(observation.frame_ts_us),
                crate::pipeline_profile::Stage::FrameDelivery,
                observation.duration,
                observation.ended,
            );
        }
        if broke.is_none() {
            if frames > frames_before {
                zero_output_since = None;
                zero_output_aus = 0;
                zero_output_bytes = 0;
                zero_output_last_ts_us = None;
            } else {
                zero_output_since.get_or_insert_with(Instant::now);
                zero_output_aus = zero_output_aus.saturating_add(1);
                zero_output_bytes = zero_output_bytes.saturating_add(au.data.len() as u64);
                zero_output_last_ts_us = Some(au.ts_us);
                if zero_output_aus >= ZERO_OUTPUT_AU_LIMIT {
                    zero_output_failure = true;
                    broke = Some(format!(
                        "decoder accepted {zero_output_aus} AU(s) / {zero_output_bytes} bytes without producing a picture"
                    ));
                }
            }
        }
        if let Some(e) = broke {
            if zero_output_failure {
                #[cfg(all(windows, feature = "host"))]
                match decoder.as_ref() {
                    Some(Active::H264(H264Rung::Nvdec(_))) => h264_runtime.demote(),
                    Some(Active::Hevc(rung)) if rung.is_nvdec() => {
                        let _ = hevc_runtime.demote_from_nvdec(hevc_nvdec_pinned);
                    }
                    _ => {}
                }
            }
            // Corrupt bitstream (a lost unit upstream): drop the decoder,
            // re-enter at the next IDR. Rate-limited — at frame rate this
            // would otherwise be a log flood.
            if last_err.is_none_or(|t| t.elapsed() >= STATS_EVERY) {
                last_err = Some(Instant::now());
                tracing::warn!("video decode for {route_id} failed ({e}); awaiting a key unit");
            }
            decoder = None;
            waiting_key = true;
            zero_output_since = None;
            zero_output_aus = 0;
            zero_output_bytes = 0;
            zero_output_last_ts_us = None;
            // Frame health: name the AU that broke — a capable sender
            // heals with a wave instead of a keyframe wall.
            on_glitch(Some(au.ts_us));
        }
        let elapsed = since.elapsed();
        if elapsed >= STATS_EVERY && frames > 0 {
            // Bandwidth at each viewer layer: `wire` = compressed bytes
            // into the decoder, `nv12` = the decoder's picture output,
            // `rgba` = what crosses the IPC boundary to the window —
            // the field log's answer to "where does the bandwidth go".
            let secs = elapsed.as_secs_f64();
            let px = frames as f64 * out_dims.0 as f64 * out_dims.1 as f64;
            let (queue_pending, queue_hwm, queue_overflows) = queue_stats.snapshot();
            #[cfg(all(windows, feature = "host"))]
            let nvdec_diag = {
                let s = crate::nvdec::status_counters();
                format!(
                    " · NVDEC status q={} clean={} concealed={} corrupt={} unsettled={} api_err={}",
                    s.queries, s.clean, s.concealed, s.corrupt, s.unsettled, s.api_errors
                )
            };
            #[cfg(not(all(windows, feature = "host")))]
            let nvdec_diag = String::new();
            let line = format!(
                "video decode {route_id}: {:.1} fps · {:.1} ms/frame · {}×{} (native) · wire {:.1} → nv12 {:.0} → rgba {:.0} Mbps · queue {queue_pending}/{} hwm={queue_hwm} overflow={queue_overflows}{nvdec_diag}",
                frames as f64 / secs,
                spent.as_secs_f64() * 1000.0 / frames as f64,
                out_dims.0,
                out_dims.1,
                in_bytes as f64 * 8.0 / secs / 1e6,
                px * 1.5 * 8.0 / secs / 1e6,
                px * 4.0 * 8.0 / secs / 1e6,
                queue_stats.capacity,
            );
            if crate::video::stats_to_info() {
                tracing::info!("{line}");
            } else {
                tracing::debug!("{line}");
            }
            (frames, spent, in_bytes, since) = (0, Duration::ZERO, 0, Instant::now());
        }
    }
}

/// An IPC packet (kind 3 — see the wire-format comment in `mesh.rs`) with
/// the RGBA payload area zeroed, sized for `w`×`h`. The decoder writes its
/// pixels straight into the tail, so the only copy on the decode path is
/// the YUV→RGBA conversion itself.
fn raw_ipc_packet(ts_us: u64, w: u32, h: u32) -> Vec<u8> {
    let len = (w as usize) * (h as usize) * 4;
    let mut out = crate::mesh::video_ipc_header(3, 0, [w, h, 0, 0], ts_us, len);
    out.resize(crate::mesh::VIDEO_IPC_HEADER_LEN + len, 0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded_h264_recovery_chain() -> (Vec<u8>, Vec<Vec<u8>>, Vec<u8>) {
        use openh264::encoder::{Encoder, EncoderConfig, IntraFramePeriod};
        use openh264::formats::{RgbSliceU8, YUVBuffer};

        fn encode(encoder: &mut Encoder, shade: u8) -> Vec<u8> {
            let rgb = vec![shade; 64 * 64 * 3];
            let yuv = YUVBuffer::from_rgb8_source(RgbSliceU8::new(&rgb, (64, 64)));
            let bytes = encoder.encode(&yuv).expect("encode test AU").to_vec();
            assert!(!bytes.is_empty(), "frame skipping is disabled");
            bytes
        }

        let config = EncoderConfig::new()
            .skip_frames(false)
            .scene_change_detect(false)
            .intra_frame_period(IntraFramePeriod::from_num_frames(1_000));
        let mut encoder = Encoder::with_api_config(openh264::OpenH264API::from_source(), config)
            .expect("H.264 test encoder");

        let initial_key = encode(&mut encoder, 20);
        assert!(is_decode_entry(&initial_key), "first AU is a decode entry");
        let deltas = (0..=MAX_PENDING_AUS)
            .map(|i| encode(&mut encoder, 30u8.saturating_add(i as u8)))
            .collect::<Vec<_>>();
        assert!(
            deltas.iter().all(|au| !is_decode_entry(au)),
            "the overflow backlog is one dependent delta chain"
        );
        encoder.force_intra_frame();
        let recovery_key = encode(&mut encoder, 90);
        assert!(
            is_decode_entry(&recovery_key),
            "forced IDR is a self-describing recovery entry"
        );
        (initial_key, deltas, recovery_key)
    }

    fn packet_ts_us(packet: &[u8]) -> u64 {
        u64::from_le_bytes(packet[20..28].try_into().expect("timestamp field"))
    }

    fn reset_test_software_open_count(route_id: &str) {
        TEST_H264_SOFTWARE_OPENS.lock().remove(route_id);
    }

    fn test_software_open_count(route_id: &str) -> usize {
        TEST_H264_SOFTWARE_OPENS
            .lock()
            .get(route_id)
            .copied()
            .unwrap_or_default()
    }

    #[test]
    fn native_whole_au_queue_covers_cold_start_and_accounting_is_bounded() {
        // 16 arrivals are ~267 ms at 60 Hz: enough for the measured cold
        // NVDEC first-picture path without restoring the old 48-AU/800 ms
        // latency allowance.
        assert_eq!(MAX_PENDING_AUS, 16);
        let stats = QueueStats::new(MAX_PENDING_AUS);
        for expected in 1..=MAX_PENDING_AUS {
            let reserved = stats.reserve_send();
            stats.sent(reserved);
            assert_eq!(stats.snapshot().0, expected);
        }
        // A failed seventeenth reservation is rolled back and counted without
        // ever reporting a high-water beyond the physical channel.
        stats.reserve_send();
        stats.send_failed();
        stats.overflowed();
        assert_eq!(stats.snapshot(), (MAX_PENDING_AUS, MAX_PENDING_AUS, 1));
        for _ in 0..MAX_PENDING_AUS {
            stats.received();
        }
        assert_eq!(stats.snapshot(), (0, MAX_PENDING_AUS, 1));
    }

    /// Force the real bridge through its full-channel path while the software
    /// decoder's first delivery is deliberately blocked. A complete key that
    /// loses the `try_send` race must be retained, stale deltas must never
    /// paint, and recovery must not ask the sender for yet another key.
    #[test]
    fn full_queue_retains_returned_key_and_resumes_without_glitch() {
        const ROUTE: &str = "test-software-overflow-retained";
        assert_eq!(pending_capacity(), MAX_PENDING_AUS);
        reset_test_software_open_count(ROUTE);
        let (initial_key, deltas, recovery_key) = encoded_h264_recovery_chain();
        let bridge = DecodeBridge::new();
        let (frame_tx, frame_rx) = mpsc::channel::<Vec<u8>>();
        let (blocked_tx, blocked_rx) = mpsc::sync_channel::<()>(0);
        let (release_tx, release_rx) = mpsc::sync_channel::<()>(0);
        let (glitch_tx, glitch_rx) = mpsc::channel::<Option<u64>>();
        let blocked_once = Arc::new(AtomicBool::new(false));
        let sink_blocked = blocked_once.clone();

        bridge.feed(
            ROUTE,
            Au {
                ts_us: 1,
                key: true,
                data: initial_key,
            },
            move |packet| {
                let _ = frame_tx.send(packet);
                if !sink_blocked.swap(true, Ordering::SeqCst) {
                    let _ = blocked_tx.send(());
                    let _ = release_rx.recv();
                }
            },
            move |lost| {
                let _ = glitch_tx.send(lost);
            },
        );
        blocked_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("first decoded frame reached the blocked sink");
        let first = frame_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("initial decoded frame");
        assert_eq!(packet_ts_us(&first), 1);

        for (index, data) in deltas.iter().take(MAX_PENDING_AUS).enumerate() {
            bridge.feed(
                ROUTE,
                Au {
                    ts_us: 10_000 + index as u64,
                    key: false,
                    data: data.clone(),
                },
                |_| {},
                |_| {},
            );
        }
        let recovery_ts = 90_000;
        bridge.feed(
            ROUTE,
            Au {
                ts_us: recovery_ts,
                key: true,
                data: recovery_key,
            },
            |_| {},
            |_| {},
        );
        {
            let routes = bridge.routes.lock();
            let route = routes.get(ROUTE).expect("route remains live");
            assert!(route.need_key.load(Ordering::SeqCst));
            assert_eq!(route.queue_stats.snapshot().2, 1);
            let saved = route.recovery_entry.lock();
            assert_eq!(saved.as_ref().map(|entry| entry.ts_us), Some(recovery_ts));
        }

        release_tx.send(()).expect("release decoder sink");
        let recovered = frame_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("retained key decoded immediately after overflow");
        assert_eq!(packet_ts_us(&recovered), recovery_ts);
        assert_eq!(
            test_software_open_count(ROUTE),
            1,
            "overflow recovery retains the healthy decoder session"
        );
        assert!(matches!(
            glitch_rx.recv_timeout(Duration::from_millis(150)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        let routes = bridge.routes.lock();
        assert_eq!(
            routes
                .get(ROUTE)
                .expect("route remains live")
                .queue_stats
                .snapshot()
                .0,
            0,
            "overflow reset drains the stale dependency chain"
        );
        drop(routes);
        bridge.stop(ROUTE);
    }

    /// If a full queue contains no complete entry, recovery must name the
    /// abandoned frame, hold subsequent deltas, and resume on the next key.
    /// This is the timestamped media-plane feedback path that prevents a
    /// Game/GDR stream from remaining black indefinitely.
    #[test]
    fn full_delta_queue_requests_key_then_resumes() {
        const ROUTE: &str = "test-software-overflow-rekey";
        assert_eq!(pending_capacity(), MAX_PENDING_AUS);
        reset_test_software_open_count(ROUTE);
        let (initial_key, deltas, recovery_key) = encoded_h264_recovery_chain();
        let bridge = DecodeBridge::new();
        let (frame_tx, frame_rx) = mpsc::channel::<Vec<u8>>();
        let (blocked_tx, blocked_rx) = mpsc::sync_channel::<()>(0);
        let (release_tx, release_rx) = mpsc::sync_channel::<()>(0);
        let (glitch_tx, glitch_rx) = mpsc::channel::<Option<u64>>();
        let blocked_once = Arc::new(AtomicBool::new(false));
        let sink_blocked = blocked_once.clone();

        bridge.feed(
            ROUTE,
            Au {
                ts_us: 1,
                key: true,
                data: initial_key,
            },
            move |packet| {
                let _ = frame_tx.send(packet);
                if !sink_blocked.swap(true, Ordering::SeqCst) {
                    let _ = blocked_tx.send(());
                    let _ = release_rx.recv();
                }
            },
            move |lost| {
                let _ = glitch_tx.send(lost);
            },
        );
        blocked_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("first decoded frame reached the blocked sink");
        let first = frame_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("initial decoded frame");
        assert_eq!(packet_ts_us(&first), 1);

        let first_stale_ts = 20_000;
        for (index, data) in deltas.iter().take(MAX_PENDING_AUS).enumerate() {
            bridge.feed(
                ROUTE,
                Au {
                    ts_us: first_stale_ts + index as u64,
                    key: false,
                    data: data.clone(),
                },
                |_| {},
                |_| {},
            );
        }
        bridge.feed(
            ROUTE,
            Au {
                ts_us: 30_000,
                key: false,
                data: deltas[MAX_PENDING_AUS].clone(),
            },
            |_| {},
            |_| {},
        );
        release_tx.send(()).expect("release decoder sink");
        assert_eq!(
            glitch_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("overflow asks for a key"),
            Some(first_stale_ts)
        );
        assert!(matches!(
            frame_rx.recv_timeout(Duration::from_millis(150)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));

        let recovery_ts = 100_000;
        bridge.feed(
            ROUTE,
            Au {
                ts_us: recovery_ts,
                key: true,
                data: recovery_key,
            },
            |_| {},
            |_| {},
        );
        let recovered = frame_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("next key resumes the same route");
        assert_eq!(packet_ts_us(&recovered), recovery_ts);
        assert_eq!(
            test_software_open_count(ROUTE),
            1,
            "re-key recovery retains the healthy decoder session"
        );
        bridge.stop(ROUTE);
    }

    /// HEVC has no portable software decoder in this crate, but its queue
    /// recovery contract is codec-agnostic and can still be proven without a
    /// GPU: a self-describing HEVC entry returned by a full whole-AU queue is
    /// retained locally for the decoder thread rather than discarded.
    #[test]
    fn full_queue_preserves_hevc_recovery_entry_without_hardware() {
        let bridge = DecodeBridge::new();
        let (tx, rx) = mpsc::sync_channel::<QueuedAu>(MAX_PENDING_AUS);
        let stats = Arc::new(QueueStats::new(MAX_PENDING_AUS));
        for index in 0..MAX_PENDING_AUS {
            let reserved = stats.reserve_send();
            assert!(
                tx.try_send(QueuedAu::new(
                    Au {
                        ts_us: index as u64,
                        key: false,
                        data: vec![0, 0, 1, 0x41, 0x9a],
                    },
                    0,
                ))
                .is_ok(),
                "fill synthetic queue"
            );
            stats.sent(reserved);
        }
        let need_key = Arc::new(AtomicBool::new(false));
        let recovery_entry = Arc::new(Mutex::new(None));
        bridge.routes.lock().insert(
            "hevc-overflow".into(),
            RouteDecode {
                tx,
                need_key: need_key.clone(),
                queue_stats: stats.clone(),
                recovery_entry: recovery_entry.clone(),
                stop: Arc::new(AtomicBool::new(false)),
                thread: None,
            },
        );

        let hevc_entry = vec![0, 0, 1, 0x40, 0x01, 0xaa];
        bridge.feed(
            "hevc-overflow",
            Au {
                ts_us: 77,
                key: false,
                data: hevc_entry.clone(),
            },
            |_| {},
            |_| {},
        );
        assert!(need_key.load(Ordering::SeqCst));
        assert_eq!(stats.snapshot(), (MAX_PENDING_AUS, MAX_PENDING_AUS, 1));
        {
            let saved = recovery_entry.lock();
            let saved = saved.as_ref().expect("HEVC entry retained");
            assert_eq!(saved.ts_us, 77);
            assert_eq!(saved.data, hevc_entry);
            assert_eq!(sniff_codec(&saved.data), Some(AuCodec::Hevc));
        }
        bridge.stop("hevc-overflow");
        drop(rx);
    }

    /// The codec sniff's three-way branch, including the AV1 OBU seam:
    /// H.264/HEVC key units are detected from their start-code-led NAL
    /// byte, an AV1 sequence-header OBU (start-code-less) is detected as
    /// AV1, and — critically — an H.264/HEVC stream is NEVER misread as
    /// AV1 (the OBU branch only fires when no start code exists at all).
    #[test]
    fn sniff_routes_h264_hevc_and_av1_obu() {
        // H.264 IDR (00 00 01 65): type 5.
        assert_eq!(sniff_codec(&[0, 0, 1, 0x65, 0x88]), Some(AuCodec::H264));
        // H.264 SPS (67), PPS (68).
        assert_eq!(sniff_codec(&[0, 0, 1, 0x67, 0x42]), Some(AuCodec::H264));
        // HEVC VPS (0x40), SPS (0x42), PPS (0x44).
        assert_eq!(sniff_codec(&[0, 0, 1, 0x40, 0x01]), Some(AuCodec::Hevc));
        assert_eq!(sniff_codec(&[0, 0, 1, 0x42, 0x01]), Some(AuCodec::Hevc));
        // An H.264 delta P-slice (0x41) leads no key AU → None (codec
        // carries from the key) and NEVER trips the AV1 branch.
        assert_eq!(sniff_codec(&[0, 0, 1, 0x41, 0x9a]), None);
        // AV1 sequence-header OBU: has_size set, type 1 → obu byte 0x0a
        // (000 0001 0 1: type=1, ext=0, has_size=1), leb128 size 3, payload.
        let seq = [0x0a, 0x03, 0x00, 0x00, 0x00];
        assert_eq!(sniff_codec(&seq), Some(AuCodec::Av1));
        // AV1 temporal delimiter (type 2, obu 0x12) then seq header.
        let td_seq = [0x12, 0x00, 0x0a, 0x03, 0x00, 0x00, 0x00];
        assert_eq!(sniff_codec(&td_seq), Some(AuCodec::Av1));
        // An AV1 delta (a lone frame OBU type 6, obu 0x32) is not a key —
        // no seq header → None, codec carries from the key.
        assert_eq!(sniff_codec(&[0x32, 0x02, 0x10, 0x00]), None);
        // Random start-code-less bytes that aren't a valid OBU opening
        // stay None (the forbidden bit / wrong type guards).
        assert_eq!(sniff_codec(&[0xff, 0xff, 0xff, 0xff]), None);
    }

    /// A decoder thread can return after a callback failure or panic. Its
    /// sender used to remain in the route map forever, making every future
    /// display feed hit `Disconnected` without recreating the decoder.
    #[test]
    fn disconnected_decoder_route_restarts_on_next_feed() {
        let bridge = DecodeBridge::new();
        let (dead_tx, dead_rx) = mpsc::sync_channel(1);
        drop(dead_rx);
        bridge.routes.lock().insert(
            "dead-route".into(),
            RouteDecode {
                tx: dead_tx,
                need_key: Arc::new(AtomicBool::new(false)),
                queue_stats: Arc::new(QueueStats::new(1)),
                recovery_entry: Arc::new(Mutex::new(None)),
                stop: Arc::new(AtomicBool::new(false)),
                thread: None,
            },
        );

        let (glitch_tx, glitch_rx) = mpsc::channel();
        bridge.feed(
            "dead-route",
            Au {
                ts_us: 1,
                key: false,
                data: vec![0, 0, 1, 0x41, 0x9a],
            },
            |_| {},
            move |lost| {
                let _ = glitch_tx.send(lost);
            },
        );

        assert_eq!(
            glitch_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Some(1),
            "a restarted delta stream asks for a fresh key"
        );
        assert!(bridge.is_running("dead-route"));
        bridge.stop("dead-route");
        assert!(!bridge.is_running("dead-route"));
    }

    /// Enabling native decode while a route is already flowing commonly makes
    /// the first AU a delta. The decoder deliberately waits for a key, so the
    /// bridge must actively request one even though this is a first start (not
    /// merely resurrection of a disconnected worker). This is essential for
    /// Game/GDR streams, whose normal GOP has no periodic IDR.
    #[test]
    fn fresh_decoder_started_on_delta_requests_key() {
        let bridge = DecodeBridge::new();
        let (glitch_tx, glitch_rx) = mpsc::channel();
        bridge.feed(
            "fresh-delta-route",
            Au {
                ts_us: 1,
                key: false,
                data: vec![0, 0, 1, 0x41, 0x9a],
            },
            |_| {},
            move |lost| {
                let _ = glitch_tx.send(lost);
            },
        );

        assert_eq!(
            glitch_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Some(1),
            "a newly-created decoder starting on a delta asks for a fresh key"
        );
        assert!(bridge.is_running("fresh-delta-route"));
        bridge.stop("fresh-delta-route");
    }

    #[cfg(all(windows, feature = "host"))]
    #[test]
    fn repeated_nvdec_delta_failure_demotes_until_route_restart() {
        let mut policy = H264RuntimePolicy::default();
        assert!(!policy.requires_software());
        assert_eq!(policy.note_delta_failure(), 1);
        assert!(
            !policy.requires_software(),
            "one fresh NVDEC retry is allowed"
        );
        policy.note_delta_success();
        assert!(
            !policy.requires_software(),
            "a healthy dependent picture clears the strike"
        );
        assert_eq!(policy.note_delta_failure(), 1);
        assert_eq!(policy.note_delta_failure(), 2);
        assert!(policy.requires_software());
        policy.reset();
        assert!(
            !policy.requires_software(),
            "a new route gets the hardware ladder again"
        );
    }

    #[cfg(all(windows, feature = "host"))]
    #[test]
    fn hevc_runtime_failure_demotes_unless_nvdec_is_pinned() {
        let mut policy = HevcRuntimePolicy::default();
        assert!(policy.demote_from_nvdec(false));
        assert!(policy.prefer_dxva);

        policy.reset();
        assert!(!policy.prefer_dxva);

        assert!(!policy.demote_from_nvdec(true));
        assert!(
            !policy.prefer_dxva,
            "an explicit NVDEC A/B pin must preserve the selected rung"
        );
    }

    /// Encode a couple of frames with the encoder the send side uses, feed
    /// them through the bridge, and check real RGBA frames come out — the
    /// whole loop the two ends of a route rely on, no hardware involved.
    #[test]
    fn decodes_what_the_encoder_produces() {
        use openh264::encoder::Encoder;
        use openh264::formats::{RgbSliceU8, YUVBuffer};

        let mut enc = Encoder::with_api_config(
            openh264::OpenH264API::from_source(),
            openh264::encoder::EncoderConfig::new(),
        )
        .expect("encoder");
        let bridge = DecodeBridge::new();
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();

        for shade in [40u8, 200u8] {
            let rgb = vec![shade; 64 * 64 * 3];
            let yuv = YUVBuffer::from_rgb8_source(RgbSliceU8::new(&rgb, (64, 64)));
            let stream = enc.encode(&yuv).expect("encode");
            let data = stream.to_vec();
            if data.is_empty() {
                continue;
            }
            let tx = tx.clone();
            bridge.feed(
                "r1",
                Au {
                    ts_us: 0,
                    key: shade == 40, // first unit out of a fresh encoder is the IDR
                    data,
                },
                move |packet| {
                    let _ = tx.send(packet);
                },
                |_| {},
            );
        }

        let packet = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("a decoded frame");
        assert_eq!(packet[0], 3, "kind 3 = raw RGBA");
        let w = u32::from_le_bytes(packet[4..8].try_into().unwrap());
        let h = u32::from_le_bytes(packet[8..12].try_into().unwrap());
        assert_eq!((w, h), (64, 64));
        assert_eq!(
            packet.len(),
            crate::mesh::VIDEO_IPC_HEADER_LEN + 64 * 64 * 4
        );
        // Alpha is opaque all the way through (the canvas blits it as-is).
        assert_eq!(packet[crate::mesh::VIDEO_IPC_HEADER_LEN + 3], 255);
        assert!(bridge.is_running("r1"));
        bridge.stop("r1");
        assert!(!bridge.is_running("r1"));
    }

    fn assert_h264_route_decodes_resolution_change(route_id: &str) {
        use openh264::encoder::Encoder;
        use openh264::formats::{RgbSliceU8, YUVBuffer};

        let bridge = DecodeBridge::new();
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        for (seq, (w, h)) in [(1u64, (64usize, 64usize)), (2, (96usize, 80usize))] {
            let mut enc = Encoder::with_api_config(
                openh264::OpenH264API::from_source(),
                openh264::encoder::EncoderConfig::new(),
            )
            .expect("encoder");
            let rgb = vec![(seq * 70) as u8; w * h * 3];
            let yuv = YUVBuffer::from_rgb8_source(RgbSliceU8::new(&rgb, (w, h)));
            let data = enc.encode(&yuv).expect("encode key").to_vec();
            let sink = tx.clone();
            bridge.feed(
                route_id,
                Au {
                    ts_us: seq * 20_000,
                    key: true,
                    data,
                },
                move |packet| {
                    let _ = sink.send(packet);
                },
                |_| {},
            );
        }

        let mut dims = Vec::new();
        for _ in 0..2 {
            let packet = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("frame after each geometry");
            dims.push((
                u32::from_le_bytes(packet[4..8].try_into().unwrap()),
                u32::from_le_bytes(packet[8..12].try_into().unwrap()),
            ));
        }
        assert_eq!(dims, [(64, 64), (96, 80)]);
        bridge.stop(route_id);
    }

    /// A retune can change display geometry without changing the route ID.
    /// NVDEC deliberately rejects that on an existing session; the bridge must
    /// rebuild hardware and retry the same key instead of permanently
    /// demoting or waiting for another IDR.
    #[test]
    fn h264_route_rebuilds_across_resolution_change() {
        assert_h264_route_decodes_resolution_change("resize-route");
    }

    /// The same reconfiguration contract has a deterministic portable gate:
    /// OpenH264 accepts the new SPS on the existing route/session and paints
    /// the new dimensions without teardown or another key request.
    #[test]
    fn h264_software_route_reconfigures_in_place_across_resolution_change() {
        const ROUTE: &str = "test-software-resize-route";
        reset_test_software_open_count(ROUTE);
        assert_h264_route_decodes_resolution_change(ROUTE);
        assert_eq!(
            test_software_open_count(ROUTE),
            1,
            "software decoder reconfigures without rebuilding the route"
        );
    }

    /// Experimental pacing's receiver contract on the real NVIDIA rung. Feed
    /// multi-slice H.264 as separate same-timestamp samples with sub-idle gaps;
    /// the following timestamp must close/defer cleanly, and the final static
    /// picture must close on the bounded idle fallback. Run in a fresh test
    /// process with `ALLMYSTUFF_PACED_SLICES=1`.
    #[cfg(all(windows, feature = "host"))]
    #[test]
    fn h264_nvdec_bridge_coalesces_paced_samples_and_final_idle() {
        if !crate::video::paced_slices_enabled() {
            eprintln!("SKIP: set ALLMYSTUFF_PACED_SLICES=1 for the paced NVDEC bridge gate");
            return;
        }
        let _warm = match crate::nvdec::NvdecH264::open() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("SKIP: NVDEC H.264 unavailable: {e}");
                return;
            }
        };

        use openh264::encoder::{
            BitRate, Encoder, EncoderConfig, FrameRate, RateControlMode, UsageType,
        };
        use openh264::formats::{RgbSliceU8, YUVBuffer};
        let (w, h) = (640usize, 480usize);
        let config = EncoderConfig::new()
            .usage_type(UsageType::ScreenContentRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .bitrate(BitRate::from_bps(8_000_000))
            .max_frame_rate(FrameRate::from_hz(60.0))
            .max_slice_len(4 * 1024);
        let mut enc = Encoder::with_api_config(openh264::OpenH264API::from_source(), config)
            .expect("H.264 encoder");
        let mut aus = Vec::new();
        for frame in 0..12u32 {
            let mut rgb = vec![0u8; w * h * 3];
            for (i, px) in rgb.chunks_exact_mut(3).enumerate() {
                let stripe = ((i / w) as u32 + frame * 5) % 48 < 24;
                let texture = ((i as u32).wrapping_mul(29) >> 4) as u8;
                px.copy_from_slice(&[
                    if stripe { 190 } else { 35 },
                    texture,
                    255u8.wrapping_sub(texture),
                ]);
            }
            let yuv = YUVBuffer::from_rgb8_source(RgbSliceU8::new(&rgb, (w, h)));
            let data = enc.encode(&yuv).expect("encode paced H.264").to_vec();
            if !data.is_empty() {
                aus.push(data);
            }
            if aus.len() == 3 {
                break;
            }
        }
        assert_eq!(aus.len(), 3, "three encoded access units");

        let bridge = DecodeBridge::new();
        let (frame_tx, frame_rx) = mpsc::channel::<Vec<u8>>();
        let mut saw_multi = false;
        let final_ts = 30_000u64;
        for (index, data) in aus.into_iter().enumerate() {
            let ts_us = (index as u64 + 1) * 10_000;
            let chunks = crate::video::split_annexb_paced(&data, 4 * 1024);
            saw_multi |= chunks.len() > 1;
            for range in chunks {
                let sink = frame_tx.clone();
                bridge.feed(
                    "paced-h264-route",
                    Au {
                        ts_us,
                        key: is_decode_entry(&data),
                        data: data[range].to_vec(),
                    },
                    move |packet| {
                        let _ = sink.send(packet);
                    },
                    |_| {},
                );
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        assert!(saw_multi, "the encoder produced multi-sample access units");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut timestamps = Vec::new();
        while Instant::now() < deadline && !timestamps.contains(&final_ts) {
            match frame_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(packet) => timestamps.push(u64::from_le_bytes(
                    packet[20..28].try_into().expect("timestamp field"),
                )),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        bridge.stop("paced-h264-route");
        assert!(
            timestamps.contains(&10_000),
            "next timestamp closed the first AU"
        );
        assert!(
            timestamps.contains(&20_000),
            "deferred timestamp decoded in order"
        );
        assert!(
            timestamps.contains(&final_ts),
            "the static final AU closed on the {} ms idle boundary; got {timestamps:?}",
            NVDEC_CHUNK_IDLE.as_millis()
        );
    }

    /// HEVC through the whole bridge on the real hardware rungs: NVENC
    /// lossless AUs fed with `key: false` on purpose — the daemon's key
    /// flag is H.264-shaped and must never be load-bearing for HEVC; the
    /// sniff carries the entry. 640×360 codes with CTB padding (384
    /// rows), so the display crop is exercised too. Skips (passing)
    /// without the NVIDIA rungs.
    #[cfg(all(windows, feature = "host"))]
    #[test]
    fn hevc_stream_decodes_through_bridge() {
        let paced = crate::video::paced_slices_enabled();
        let (w, h) = (640u32, 360u32);
        let (wu, hu) = (w as usize, h as usize);
        let mut gpu = match crate::gpu_pipeline::GpuConvert::new(w, h, w, h) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("SKIP: GPU convert unavailable: {e}");
                return;
            }
        };
        let mut enc =
            match crate::nvenc::NvencH264::open_lossless_hevc_on_device(&gpu.device(), w, h, 60) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("SKIP: NVENC HEVC unavailable: {e}");
                    return;
                }
            };
        // Availability probe, held open through the test: paying cuInit
        // here keeps the bridge thread's lazy open fast, the same warm
        // state a live viewer reaches after its first session.
        let _warm = match crate::nvdec::NvdecHevc::open() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("SKIP: NVDEC unavailable: {e}");
                return;
            }
        };
        let bridge = DecodeBridge::new();
        let got = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let mut bgra = vec![0u8; wu * hu * 4];
        let tex = gpu.bgra_texture_from(&bgra, w, h).expect("tex");
        let mut saw_multi = false;
        for i in 0..30u64 {
            for (j, v) in bgra.iter_mut().enumerate() {
                *v = ((j as u64).wrapping_add(i * 11) % 251) as u8;
            }
            gpu.update_bgra(&tex, &bgra, w, h);
            let (slot, nv12) = gpu.convert(&tex).expect("convert").expect("slot");
            // Periodic IDRs, like the live stream's adaptive cadence: if
            // the bridge's bounded queue ever dumps (slow first open),
            // the stream carries its own re-entry points.
            let out = enc
                .encode_texture(&nv12, i.is_multiple_of(10))
                .expect("encode");
            gpu.release(slot);
            for (d, _) in out.units {
                let chunks = crate::video::split_annexb_paced(&d, crate::video::PACE_SLICE_BYTES);
                saw_multi |= chunks.len() > 1;
                for range in chunks {
                    let sink = got.clone();
                    bridge.feed(
                        "route-hevc",
                        Au {
                            ts_us: i * 16_667,
                            key: false,
                            data: d[range].to_vec(),
                        },
                        move |p| sink.lock().push(p),
                        |_| {},
                    );
                }
            }
            // Stay under the bounded queue — the decoder runs ~1 ms/frame.
            std::thread::sleep(Duration::from_millis(4));
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while got.lock().len() < 30 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        bridge.stop("route-hevc");
        let packets = got.lock();
        if paced {
            assert!(saw_multi, "NVENC emitted a paced multi-sample HEVC AU");
        }
        // ≥18: allows one dumped queue window at start-up (≤12 units)
        // healed by the next periodic key — the bridge's designed
        // recovery — while still proving a sustained decoded stream.
        assert!(packets.len() >= 18, "decoded packets: {}", packets.len());
        let expect = crate::mesh::VIDEO_IPC_HEADER_LEN + wu * hu * 4;
        assert!(packets.iter().all(|p| p.len() == expect), "packet shape");
        let pw = u32::from_le_bytes(packets[0][4..8].try_into().unwrap());
        let ph = u32::from_le_bytes(packets[0][8..12].try_into().unwrap());
        assert_eq!((pw, ph), (w, h), "display-cropped dimensions");
        let body = &packets[5][crate::mesh::VIDEO_IPC_HEADER_LEN..];
        assert!(body.iter().any(|&b| b > 8), "pixels arrived");
    }
}
