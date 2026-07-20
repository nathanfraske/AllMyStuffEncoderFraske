//! Opt-in, process-local timing for the video pipeline.
//!
//! This is deliberately a development harness, not telemetry. It is disabled
//! unless `ALLMYSTUFF_VIDEO_PROFILE=1` (or the trace variable below) is set,
//! keeps only fixed-size histograms, and writes nowhere except the local log
//! and an explicitly requested local JSONL file. No value from here is put on
//! the media, signalling, ICE, STUN, or TURN protocols.
//!
//! Timing uses this process's monotonic [`Instant`] clock. RTP/source
//! timestamps are correlation labels only: they can originate in another
//! clock domain and must never be subtracted from a local timestamp. Likewise,
//! `frame_delivery` measures the synchronous local callback, not remote paint.
//! Correlation ids follow only hook chains that already carry a local object;
//! they deliberately reset at existing pipe/message boundaries rather than
//! adding an id to any protocol.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

const DEFAULT_INTERVAL: Duration = Duration::from_secs(5);
const DEFAULT_MAX_SERIES: usize = 256;
const TRACE_QUEUE: usize = 2_048;
const DEFAULT_TRACE_EVENTS: u64 = 20_000;
const ROUTE_LABEL_BYTES: usize = 96;
const HISTOGRAM_SUB_BUCKETS: usize = 8;
const HISTOGRAM_BUCKETS: usize = 64 * HISTOGRAM_SUB_BUCKETS;

/// A wall-time boundary which the live pipeline can measure without changing
/// its wire protocol or pretending that separate clocks are synchronized.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)] // capture stages are absent in intentional viewer-only builds
pub(crate) enum Stage {
    CaptureWait,
    CaptureAge,
    ConvertBusy,
    EncoderQueueWait,
    EncodeBusy,
    EncoderOutputDelivery,
    OutboundRouteQueueWait,
    OutboundPaceWait,
    OutboundSerializeBusy,
    FrameDelivery,
    ViewerQueueWait,
    ViewerPollCadence,
    ViewerPollLockWait,
    ViewerBatchBusy,
    ViewerIpcWrite,
    OutboundPipeWait,
    OutboundPipeConnectWait,
    OutboundPipeWrite,
    InboundPipeWaitRead,
    InboundParseBusy,
    InboundDispatchBackpressure,
    InboundDispatchWait,
    DecoderQueueWait,
    DecoderPrepareBusy,
    DecoderCoalesceWait,
    /// Codec submission/output plus mandatory pixel conversion when the
    /// backend exposes those only as one fused call; callback time is removed.
    DecodeBusy,
}

impl Stage {
    fn name(self) -> &'static str {
        match self {
            Self::CaptureWait => "capture_wait",
            Self::CaptureAge => "capture_age",
            Self::ConvertBusy => "convert_busy",
            Self::EncoderQueueWait => "encoder_queue_wait",
            Self::EncodeBusy => "encode_busy",
            Self::EncoderOutputDelivery => "encoder_output_delivery",
            Self::OutboundRouteQueueWait => "outbound_route_queue_wait",
            Self::OutboundPaceWait => "outbound_pace_wait",
            Self::OutboundSerializeBusy => "outbound_serialize_busy",
            Self::FrameDelivery => "frame_delivery",
            Self::ViewerQueueWait => "viewer_queue_wait",
            Self::ViewerPollCadence => "viewer_poll_cadence",
            Self::ViewerPollLockWait => "viewer_poll_lock_wait",
            Self::ViewerBatchBusy => "viewer_batch_busy",
            Self::ViewerIpcWrite => "viewer_ipc_write",
            Self::OutboundPipeWait => "outbound_pipe_wait",
            Self::OutboundPipeConnectWait => "outbound_pipe_connect_wait",
            Self::OutboundPipeWrite => "outbound_pipe_write",
            Self::InboundPipeWaitRead => "inbound_pipe_wait_read",
            Self::InboundParseBusy => "inbound_parse_busy",
            Self::InboundDispatchBackpressure => "inbound_dispatch_backpressure",
            Self::InboundDispatchWait => "inbound_dispatch_wait",
            Self::DecoderQueueWait => "decoder_queue_wait",
            Self::DecoderPrepareBusy => "decoder_prepare_busy",
            Self::DecoderCoalesceWait => "decoder_coalesce_wait",
            Self::DecodeBusy => "decode_busy",
        }
    }

    fn kind(self) -> StageKind {
        match self {
            Self::CaptureWait | Self::InboundPipeWaitRead | Self::ViewerPollCadence => {
                StageKind::Cadence
            }
            Self::CaptureAge => StageKind::Gauge,
            Self::EncoderQueueWait
            | Self::OutboundRouteQueueWait
            | Self::OutboundPipeWait
            | Self::InboundDispatchBackpressure
            | Self::InboundDispatchWait
            | Self::DecoderQueueWait
            | Self::DecoderCoalesceWait
            | Self::ViewerQueueWait
            | Self::ViewerPollLockWait => StageKind::QueueWait,
            Self::OutboundPaceWait => StageKind::PaceWait,
            Self::OutboundPipeConnectWait | Self::OutboundPipeWrite | Self::ViewerIpcWrite => {
                StageKind::IoWait
            }
            Self::EncoderOutputDelivery | Self::FrameDelivery => StageKind::Delivery,
            Self::ConvertBusy
            | Self::EncodeBusy
            | Self::OutboundSerializeBusy
            | Self::InboundParseBusy
            | Self::DecoderPrepareBusy
            | Self::DecodeBusy
            | Self::ViewerBatchBusy => StageKind::Busy,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum StageKind {
    Busy,
    QueueWait,
    IoWait,
    PaceWait,
    Delivery,
    Cadence,
    Gauge,
}

impl StageKind {
    fn name(self) -> &'static str {
        match self {
            Self::Busy => "busy",
            Self::QueueWait => "queue_wait",
            Self::IoWait => "io_wait",
            Self::PaceWait => "pace_wait",
            Self::Delivery => "delivery",
            Self::Cadence => "cadence",
            Self::Gauge => "gauge",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct SeriesKey {
    route_hash: u64,
    route_fingerprint: u64,
    stage: Stage,
}

#[derive(Clone)]
struct Histogram {
    buckets: [u64; HISTOGRAM_BUCKETS],
    count: u64,
    sum_ns: u128,
    max_ns: u64,
}

impl Default for Histogram {
    fn default() -> Self {
        Self {
            buckets: [0; HISTOGRAM_BUCKETS],
            count: 0,
            sum_ns: 0,
            max_ns: 0,
        }
    }
}

impl Histogram {
    fn add(&mut self, duration: Duration) {
        let ns = duration.as_nanos().min(u64::MAX as u128) as u64;
        // Eight subdivisions per nanosecond log2 octave keep percentile bounds
        // within about 12.5% while still covering arbitrary stalls in a fixed
        // 4 KiB aggregate. No per-frame sample list grows with stream length.
        let bucket = if ns == 0 {
            0
        } else {
            let exponent = (u64::BITS - 1 - ns.leading_zeros()) as usize;
            let base = 1u64 << exponent;
            let sub =
                (((ns - base) as u128 * HISTOGRAM_SUB_BUCKETS as u128) / base as u128) as usize;
            exponent * HISTOGRAM_SUB_BUCKETS + sub.min(HISTOGRAM_SUB_BUCKETS - 1)
        }
        .min(HISTOGRAM_BUCKETS - 1);
        self.buckets[bucket] = self.buckets[bucket].saturating_add(1);
        self.count = self.count.saturating_add(1);
        self.sum_ns = self.sum_ns.saturating_add(ns as u128);
        self.max_ns = self.max_ns.max(ns);
    }

    fn percentile_upper_ns(&self, percentile: u64) -> u64 {
        if self.count == 0 {
            return 0;
        }
        let wanted = self.count.saturating_mul(percentile).div_ceil(100).max(1);
        let mut seen = 0u64;
        for (index, count) in self.buckets.iter().enumerate() {
            seen = seen.saturating_add(*count);
            if seen >= wanted {
                let exponent = index / HISTOGRAM_SUB_BUCKETS;
                let sub = index % HISTOGRAM_SUB_BUCKETS;
                let base = 1u128 << exponent;
                let upper =
                    base + (base * (sub + 1) as u128).div_ceil(HISTOGRAM_SUB_BUCKETS as u128) - 1;
                return upper.min(u64::MAX as u128) as u64;
            }
        }
        self.max_ns
    }

    #[cfg(test)]
    fn reset(&mut self) {
        *self = Self::default();
    }
}

struct Series {
    route: String,
    histogram: Histogram,
}

struct State {
    series: HashMap<SeriesKey, Series>,
    next_summary: Instant,
    rejected_series: u64,
}

struct Summary {
    route: String,
    stage: Stage,
    histogram: Histogram,
}

#[derive(Serialize)]
struct TraceEvent {
    version: u8,
    pid: u32,
    monotonic_ns: u64,
    route: String,
    frame_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    frame_ts_us: Option<u64>,
    stage: Stage,
    kind: StageKind,
    duration_ns: u64,
}

struct TraceSink {
    tx: SyncSender<TraceEvent>,
    accepted: AtomicU64,
    dropped: AtomicU64,
    closed: std::sync::atomic::AtomicBool,
    max_events: u64,
}

struct Profiler {
    started: Instant,
    interval: Duration,
    max_series: usize,
    state: Mutex<State>,
    next_frame_id: AtomicU64,
    trace: Option<TraceSink>,
}

static PROFILER: LazyLock<Option<Profiler>> = LazyLock::new(Profiler::from_env);

/// Whether the development profiler is active. After the one-time lazy
/// environment check, the ordinary steady-state path is one predictable
/// branch and no clock read, allocation, atomic increment, or mutex lock.
pub(crate) fn enabled() -> bool {
    PROFILER.is_some()
}

/// Mint a process-local correlation id. Zero means profiling was disabled.
pub(crate) fn next_frame_id() -> u64 {
    PROFILER
        .as_ref()
        .map(|p| p.next_frame_id.fetch_add(1, Ordering::Relaxed))
        .unwrap_or(0)
}

/// Take a monotonic stamp only when profiling is enabled.
pub(crate) fn stamp() -> Option<Instant> {
    enabled().then(Instant::now)
}

pub(crate) fn record_since(
    route: &str,
    frame_id: u64,
    frame_ts_us: Option<u64>,
    stage: Stage,
    started: Option<Instant>,
) {
    if let Some(started) = started {
        let ended = Instant::now();
        record_at(
            route,
            frame_id,
            frame_ts_us,
            stage,
            ended.saturating_duration_since(started),
            ended,
        );
    }
}

pub(crate) fn record(
    route: &str,
    frame_id: u64,
    frame_ts_us: Option<u64>,
    stage: Stage,
    duration: Duration,
) {
    let Some(profiler) = PROFILER.as_ref() else {
        return;
    };
    let ended = Instant::now();
    profiler.record(route, frame_id, frame_ts_us, stage, duration, ended);
}

/// Record an already-snapshotted observation and its stage-end stamp. Callers
/// use this when another profiler record must happen later but must not move the
/// event's monotonic endpoint.
pub(crate) fn record_at(
    route: &str,
    frame_id: u64,
    frame_ts_us: Option<u64>,
    stage: Stage,
    duration: Duration,
    ended: Instant,
) {
    let Some(profiler) = PROFILER.as_ref() else {
        return;
    };
    profiler.record(route, frame_id, frame_ts_us, stage, duration, ended);
}

impl Profiler {
    fn from_env() -> Option<Self> {
        let trace_path = trace_path_from_env();
        if !env_truthy("ALLMYSTUFF_VIDEO_PROFILE") && trace_path.is_none() {
            return None;
        }

        let interval_ms = env_u64(
            "ALLMYSTUFF_VIDEO_PROFILE_INTERVAL_MS",
            DEFAULT_INTERVAL.as_millis() as u64,
        )
        .clamp(1_000, 60_000);
        let max_series = env_u64(
            "ALLMYSTUFF_VIDEO_PROFILE_MAX_SERIES",
            DEFAULT_MAX_SERIES as u64,
        )
        .clamp(16, 2_048) as usize;
        let started = Instant::now();
        let trace = trace_path.and_then(|path| {
            let max_events = env_u64(
                "ALLMYSTUFF_VIDEO_PROFILE_TRACE_EVENTS",
                DEFAULT_TRACE_EVENTS,
            )
            .clamp(100, 1_000_000);
            TraceSink::open(path, max_events)
        });

        tracing::warn!(
            "development video profiler enabled: process-local monotonic wall time only; decode_busy includes fused pixel conversion; frame_delivery is the local callback, not remote paint; RTP timestamps are labels, never clock arithmetic; frame ids reset at existing pipe boundaries"
        );
        Some(Self {
            started,
            interval: Duration::from_millis(interval_ms),
            max_series,
            state: Mutex::new(State {
                series: HashMap::new(),
                next_summary: started + Duration::from_millis(interval_ms),
                rejected_series: 0,
            }),
            next_frame_id: AtomicU64::new(1),
            trace,
        })
    }

    fn record(
        &self,
        route: &str,
        frame_id: u64,
        frame_ts_us: Option<u64>,
        stage: Stage,
        duration: Duration,
        ended: Instant,
    ) {
        let route_hash = hash_route(route);
        let key = SeriesKey {
            route_hash,
            route_fingerprint: fingerprint_route(route),
            stage,
        };
        let now = ended;
        let (summaries, rejected) = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(series) = state.series.get_mut(&key) {
                series.histogram.add(duration);
            } else if state.series.len() < self.max_series {
                let mut histogram = Histogram::default();
                histogram.add(duration);
                state.series.insert(
                    key,
                    Series {
                        route: bounded_route_label(route, route_hash),
                        histogram,
                    },
                );
            } else {
                state.rejected_series = state.rejected_series.saturating_add(1);
            }

            if now >= state.next_summary {
                state.next_summary = now + self.interval;
                let rejected = std::mem::take(&mut state.rejected_series);
                // Drain the bounded table every interval. This both publishes
                // the samples and evicts dead route incarnations, so a long
                // development session never permanently rejects new routes.
                let interval_series = std::mem::take(&mut state.series);
                let mut summaries = Vec::with_capacity(interval_series.len());
                for (key, series) in interval_series {
                    if series.histogram.count > 0 {
                        summaries.push(Summary {
                            route: series.route,
                            stage: key.stage,
                            histogram: series.histogram,
                        });
                    }
                }
                (Some(summaries), rejected)
            } else {
                (None, 0)
            }
        };

        if let Some(summaries) = summaries {
            for summary in summaries {
                log_summary(summary);
            }
            if rejected > 0 {
                tracing::warn!(
                    "video profile: rejected {rejected} samples after the bounded {}-series table filled",
                    self.max_series
                );
            }
            if let Some(trace) = &self.trace {
                let dropped = trace.dropped.swap(0, Ordering::Relaxed);
                if dropped > 0 {
                    tracing::warn!(
                        "video profile trace: dropped {dropped} events because the bounded writer queue was full"
                    );
                }
            }
        }

        if let Some(trace) = &self.trace {
            if !trace.reserve() {
                return;
            }
            trace.push_reserved(TraceEvent {
                version: 2,
                pid: std::process::id(),
                monotonic_ns: ended
                    .saturating_duration_since(self.started)
                    .as_nanos()
                    .min(u64::MAX as u128) as u64,
                route: bounded_route_label(route, route_hash),
                frame_id,
                frame_ts_us,
                stage,
                kind: stage.kind(),
                duration_ns: duration.as_nanos().min(u64::MAX as u128) as u64,
            });
        }
    }
}

impl TraceSink {
    fn open(path: PathBuf, max_events: u64) -> Option<Self> {
        let file = match OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) => {
                tracing::warn!(
                    "video profile trace disabled: cannot open {}: {error}",
                    path.display()
                );
                return None;
            }
        };
        let (tx, rx) = mpsc::sync_channel::<TraceEvent>(TRACE_QUEUE);
        std::thread::Builder::new()
            .name("video-profile-jsonl".into())
            .spawn(move || write_trace(file, rx))
            .ok()?;
        tracing::warn!(
            "video profile JSONL trace enabled at {} (bounded to {max_events} events)",
            path.display()
        );
        Some(Self {
            tx,
            accepted: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            closed: std::sync::atomic::AtomicBool::new(false),
            max_events,
        })
    }

    /// Reserve one of the bounded successful-enqueue slots before the caller
    /// allocates a route label or constructs an event.
    fn reserve(&self) -> bool {
        if self.closed.load(Ordering::Relaxed) {
            return false;
        }
        self.accepted
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |accepted| {
                (accepted < self.max_events).then_some(accepted + 1)
            })
            .is_ok()
    }

    fn push_reserved(&self, event: TraceEvent) {
        match self.tx.try_send(event) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.accepted.fetch_sub(1, Ordering::Relaxed);
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => {
                self.accepted.fetch_sub(1, Ordering::Relaxed);
                self.closed.store(true, Ordering::Relaxed);
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

fn write_trace(file: File, rx: mpsc::Receiver<TraceEvent>) {
    let mut writer = BufWriter::with_capacity(64 * 1024, file);
    let mut buffered = 0u8;
    loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(event) => {
                if serde_json::to_writer(&mut writer, &event).is_err()
                    || writer.write_all(b"\n").is_err()
                {
                    tracing::warn!("video profile trace writer failed; JSONL trace stopped");
                    return;
                }
                buffered = buffered.saturating_add(1);
                if buffered >= 64 {
                    if writer.flush().is_err() {
                        tracing::warn!("video profile trace flush failed; JSONL trace stopped");
                        return;
                    }
                    buffered = 0;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) if buffered > 0 => {
                if writer.flush().is_err() {
                    tracing::warn!("video profile trace flush failed; JSONL trace stopped");
                    return;
                }
                buffered = 0;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = writer.flush();
}

fn log_summary(summary: Summary) {
    let h = summary.histogram;
    let avg_ns = if h.count == 0 {
        0
    } else {
        (h.sum_ns / h.count as u128).min(u64::MAX as u128) as u64
    };
    tracing::info!(
        "video profile route={} kind={} stage={} n={} avg={:.3}ms p50<={:.3}ms p95<={:.3}ms p99<={:.3}ms max={:.3}ms",
        summary.route,
        summary.stage.kind().name(),
        summary.stage.name(),
        h.count,
        ns_ms(avg_ns),
        ns_ms(h.percentile_upper_ns(50)),
        ns_ms(h.percentile_upper_ns(95)),
        ns_ms(h.percentile_upper_ns(99)),
        ns_ms(h.max_ns),
    );
}

fn ns_ms(ns: u64) -> f64 {
    ns as f64 / 1_000_000.0
}

fn hash_route(route: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    route.hash(&mut hasher);
    hasher.finish()
}

/// A second allocation-free identity prevents a single 64-bit hash collision
/// from silently merging two route series. FNV-1a is intentionally independent
/// of `DefaultHasher`; this is an identity check, not a security primitive.
fn fingerprint_route(route: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in route.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn bounded_route_label(route: &str, hash: u64) -> String {
    if route.len() <= ROUTE_LABEL_BYTES {
        return route.to_string();
    }
    let mut end = ROUTE_LABEL_BYTES;
    while !route.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...#{hash:016x}", &route[..end])
}

fn env_truthy(key: &str) -> bool {
    std::env::var(key).is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "on" | "true" | "yes"
        )
    })
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(default)
}

fn trace_path_from_env() -> Option<PathBuf> {
    let value = std::env::var("ALLMYSTUFF_VIDEO_PROFILE_TRACE").ok()?;
    let value = value.trim();
    if value.is_empty()
        || matches!(
            value.to_ascii_lowercase().as_str(),
            "0" | "off" | "false" | "no"
        )
    {
        None
    } else if matches!(
        value.to_ascii_lowercase().as_str(),
        "1" | "on" | "true" | "yes"
    ) {
        Some(PathBuf::from("allmystuff-video-profile.jsonl"))
    } else {
        Some(PathBuf::from(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_histogram_reports_bounded_percentiles_and_resets() {
        let mut histogram = Histogram::default();
        for ms in 1..=100 {
            histogram.add(Duration::from_millis(ms));
        }
        assert_eq!(histogram.count, 100);
        assert!(histogram.percentile_upper_ns(50) >= 50_000_000);
        assert!(histogram.percentile_upper_ns(95) >= 95_000_000);
        assert_eq!(histogram.max_ns, 100_000_000);
        histogram.reset();
        assert_eq!(histogram.count, 0);
        assert_eq!(histogram.percentile_upper_ns(95), 0);
    }

    #[test]
    fn long_utf8_route_labels_are_safely_bounded_and_identifiable() {
        let route = "display/".to_string() + &"é".repeat(100);
        let label = bounded_route_label(&route, 0x1234);
        assert!(label.is_char_boundary(label.len()));
        assert!(label.contains("#0000000000001234"));
        assert!(label.len() <= ROUTE_LABEL_BYTES + 3 + 1 + 16);
    }

    #[test]
    fn stage_names_are_unique_and_stable() {
        let stages = [
            Stage::CaptureWait,
            Stage::CaptureAge,
            Stage::ConvertBusy,
            Stage::EncoderQueueWait,
            Stage::EncodeBusy,
            Stage::EncoderOutputDelivery,
            Stage::OutboundRouteQueueWait,
            Stage::OutboundPaceWait,
            Stage::OutboundSerializeBusy,
            Stage::FrameDelivery,
            Stage::ViewerQueueWait,
            Stage::ViewerPollCadence,
            Stage::ViewerPollLockWait,
            Stage::ViewerBatchBusy,
            Stage::ViewerIpcWrite,
            Stage::OutboundPipeWait,
            Stage::OutboundPipeConnectWait,
            Stage::OutboundPipeWrite,
            Stage::InboundPipeWaitRead,
            Stage::InboundParseBusy,
            Stage::InboundDispatchBackpressure,
            Stage::InboundDispatchWait,
            Stage::DecoderQueueWait,
            Stage::DecoderPrepareBusy,
            Stage::DecoderCoalesceWait,
            Stage::DecodeBusy,
        ];
        let mut names = std::collections::HashSet::new();
        for stage in stages {
            assert!(names.insert(stage.name()));
        }
    }

    #[test]
    fn default_limits_remain_bounded() {
        const {
            assert!(DEFAULT_MAX_SERIES <= 2_048);
            assert!(TRACE_QUEUE <= 4_096);
            assert!(DEFAULT_TRACE_EVENTS <= 100_000);
        }
        assert_eq!(DEFAULT_INTERVAL, Duration::from_secs(5));
    }

    #[test]
    fn profiler_rejects_new_series_after_its_fixed_cap() {
        let started = Instant::now();
        let profiler = Profiler {
            started,
            interval: Duration::from_secs(60),
            max_series: 2,
            state: Mutex::new(State {
                series: HashMap::new(),
                next_summary: started + Duration::from_secs(60),
                rejected_series: 0,
            }),
            next_frame_id: AtomicU64::new(1),
            trace: None,
        };
        for route in ["one", "two", "three"] {
            profiler.record(
                route,
                1,
                None,
                Stage::EncodeBusy,
                Duration::from_millis(1),
                Instant::now(),
            );
        }
        let state = profiler.state.lock().unwrap();
        assert_eq!(state.series.len(), 2);
        assert_eq!(state.rejected_series, 1);
    }
}
