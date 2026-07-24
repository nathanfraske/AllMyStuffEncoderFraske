//! Production video-path probe for an already-running pair of AllMyStuff nodes.
//!
//! This deliberately drives the same local node-control socket as the desktop
//! client.  It does not instantiate `Mesh`, bypass authorization, inject media,
//! or emulate MyOwnMesh.  The remote screen therefore travels on the normal
//! negotiated video track over the peers' selected ICE pair; only route setup
//! and polling cross the local node-control socket.
//!
//! Run `--list` first.  A real run refuses a source hosted by this node because
//! an AllMyStuff display self-route is local session plumbing, not an ICE test.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use allmystuff_node::node_control::{NodeClient, NodeEvent};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use tokio::sync::mpsc;

const IPC_HEADER_LEN: usize = 28;
const H264_KIND: u8 = 2;
const RAW_KIND: u8 = 3;
const JS_SAFE_INTEGER_MAX: u64 = (1_u64 << 53) - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeliveryMode {
    Native,
    Compressed,
}

impl DeliveryMode {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "native" => Ok(Self::Native),
            "compressed" => Ok(Self::Compressed),
            other => bail!("invalid --delivery {other}; expected native or compressed"),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Compressed => "compressed",
        }
    }
}

#[derive(Debug)]
struct Config {
    list: bool,
    source: Option<String>,
    peer: Option<String>,
    sink: Option<String>,
    phase_secs: u64,
    cycles: u32,
    active_timeout_secs: u64,
    first_frame_timeout_secs: u64,
    rewatch: bool,
    resize_edge: Option<u32>,
    fps: Option<u32>,
    mode: Option<String>,
    dump_rgba: Option<PathBuf>,
    motion_palette: bool,
    json_out: Option<PathBuf>,
    delivery: DeliveryMode,
    allow_no_ice_proof: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            list: false,
            source: None,
            peer: None,
            sink: None,
            phase_secs: 5,
            cycles: 2,
            active_timeout_secs: 20,
            first_frame_timeout_secs: 12,
            rewatch: true,
            resize_edge: None,
            fps: None,
            mode: None,
            dump_rgba: None,
            motion_palette: false,
            json_out: None,
            delivery: DeliveryMode::Native,
            allow_no_ice_proof: false,
        }
    }
}

#[derive(Debug, Clone)]
struct Endpoint {
    id: String,
    node: String,
    label: String,
    origin: String,
    default: bool,
}

impl Endpoint {
    fn from_value(value: &Value, media: &str, flow: &str) -> Option<Self> {
        if value.get("media")?.as_str()? != media || value.get("flow")?.as_str()? != flow {
            return None;
        }
        Some(Self {
            id: value.get("id")?.as_str()?.to_string(),
            node: value.get("node")?.as_str()?.to_string(),
            label: value
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            origin: value
                .get("origin")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            default: value
                .get("default")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }

    fn display_value(&self) -> Value {
        json!({
            "id": self.id,
            "node": self.node,
            "label": self.label,
            "origin": self.origin,
            "default": self.default,
        })
    }
}

#[derive(Debug)]
struct DaemonView {
    paths: Vec<Value>,
    remote_sources: Vec<Endpoint>,
}

#[derive(Debug, Default)]
struct FrameStats {
    frames: u64,
    payload_bytes: u64,
    key_frames: u64,
    first_ts: Option<u64>,
    last_ts: Option<u64>,
    first_arrival: Option<Instant>,
    last_arrival: Option<Instant>,
    arrival_intervals_us: Vec<u64>,
    source_intervals_us: Vec<u64>,
    lag_drift_samples: Vec<(u64, i64)>,
    timestamp_regressions: u64,
    dimensions: BTreeSet<(u32, u32)>,
    fingerprints: BTreeSet<u64>,
    max_green_ratio: f64,
    max_black_row_ratio: f64,
    motion_palette: Option<MotionPaletteStats>,
    dumped: bool,
}

#[derive(Debug, Default)]
struct MotionPaletteStats {
    frames: u64,
    frames_with_both_targets: u64,
    first_decoded_palette_rgb: Option<[[u8; 3]; 4]>,
    max_orange_components: usize,
    max_purple_components: usize,
    max_orange_span_ratio: f64,
    max_purple_span_ratio: f64,
    max_orange_row_origin_spread_ratio: f64,
    max_purple_row_origin_spread_ratio: f64,
    max_orange_sample_ratio: f64,
    max_purple_sample_ratio: f64,
}

#[derive(Debug, Clone, Copy, Default)]
struct PaletteTargetStats {
    samples: usize,
    components: usize,
    span_ratio: f64,
    row_origin_spread_ratio: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MotionPaletteClass {
    Dark,
    Checker,
    Orange,
    Purple,
}

impl MotionPaletteStats {
    fn observe(&mut self, rgba: &[u8], width: usize, height: usize) {
        let (orange, purple, sampled, decoded_palette) =
            motion_palette_frame_stats(rgba, width, height);
        self.frames = self.frames.saturating_add(1);
        self.first_decoded_palette_rgb
            .get_or_insert(decoded_palette);
        self.frames_with_both_targets = self
            .frames_with_both_targets
            .saturating_add(u64::from(orange.samples > 0 && purple.samples > 0));
        self.max_orange_components = self.max_orange_components.max(orange.components);
        self.max_purple_components = self.max_purple_components.max(purple.components);
        self.max_orange_span_ratio = self.max_orange_span_ratio.max(orange.span_ratio);
        self.max_purple_span_ratio = self.max_purple_span_ratio.max(purple.span_ratio);
        self.max_orange_row_origin_spread_ratio = self
            .max_orange_row_origin_spread_ratio
            .max(orange.row_origin_spread_ratio);
        self.max_purple_row_origin_spread_ratio = self
            .max_purple_row_origin_spread_ratio
            .max(purple.row_origin_spread_ratio);
        if sampled > 0 {
            self.max_orange_sample_ratio = self
                .max_orange_sample_ratio
                .max(orange.samples as f64 / sampled as f64);
            self.max_purple_sample_ratio = self
                .max_purple_sample_ratio
                .max(purple.samples as f64 / sampled as f64);
        }
    }

    fn summary(&self) -> Value {
        json!({
            "palette": {
                "dark_rgb": [38, 42, 54],
                "checker_rgb": [112, 128, 144],
                "orange_rgb": [255, 165, 0],
                "purple_rgb": [147, 112, 219],
                "classification": "nearest per-frame decoded palette color",
                "calibration": "channel-wise affine fit from the known half-dark, half-checker background medians",
            },
            "first_decoded_palette_rgb": self.first_decoded_palette_rgb,
            "sample_grid": "up to 128x64",
            "frames": self.frames,
            "frames_with_both_targets": self.frames_with_both_targets,
            "max_orange_components": self.max_orange_components,
            "max_purple_components": self.max_purple_components,
            "max_orange_span_ratio": self.max_orange_span_ratio,
            "max_purple_span_ratio": self.max_purple_span_ratio,
            "max_orange_row_origin_spread_ratio": self.max_orange_row_origin_spread_ratio,
            "max_purple_row_origin_spread_ratio": self.max_purple_row_origin_spread_ratio,
            "max_orange_sample_ratio": self.max_orange_sample_ratio,
            "max_purple_sample_ratio": self.max_purple_sample_ratio,
        })
    }
}

fn nearest_motion_palette(
    r: u8,
    g: u8,
    b: u8,
    palette: &[(MotionPaletteClass, [u8; 3]); 4],
) -> MotionPaletteClass {
    let rgb = [i32::from(r), i32::from(g), i32::from(b)];
    palette
        .iter()
        .min_by_key(|(_, target)| {
            rgb.iter()
                .zip(target.iter().map(|value| i32::from(*value)))
                .map(|(actual, expected)| {
                    let delta = actual - expected;
                    delta * delta
                })
                .sum::<i32>()
        })
        .map(|(class, _)| *class)
        .unwrap_or(MotionPaletteClass::Dark)
}

fn motion_palette_from_samples(samples: &[[u8; 3]]) -> [(MotionPaletteClass, [u8; 3]); 4] {
    const SOURCE_DARK: [i32; 3] = [38, 42, 54];
    const SOURCE_CHECKER: [i32; 3] = [112, 128, 144];
    const SOURCE_ORANGE: [i32; 3] = [255, 165, 0];
    const SOURCE_PURPLE: [i32; 3] = [147, 112, 219];

    if samples.is_empty() {
        return [
            (
                MotionPaletteClass::Dark,
                SOURCE_DARK.map(|value| value as u8),
            ),
            (
                MotionPaletteClass::Checker,
                SOURCE_CHECKER.map(|value| value as u8),
            ),
            (
                MotionPaletteClass::Orange,
                SOURCE_ORANGE.map(|value| value as u8),
            ),
            (
                MotionPaletteClass::Purple,
                SOURCE_PURPLE.map(|value| value as u8),
            ),
        ];
    }

    // The generator background is a known, evenly alternating two-color
    // checkerboard. Sorting by measured brightness and taking the component
    // medians of each half recovers its decoded dark/checker centers without
    // imposing a guessed color-distance threshold. The two moving rectangles
    // occupy only a small minority of the known test image.
    let mut ordered = samples.to_vec();
    ordered.sort_unstable_by_key(|rgb| u16::from(rgb[0]) + u16::from(rgb[1]) + u16::from(rgb[2]));
    let split = ordered.len() / 2;
    let (low, high) = ordered.split_at(split.max(1).min(ordered.len()));
    let dark = component_median(low);
    let checker = component_median(if high.is_empty() { low } else { high });

    let transform = |source: [i32; 3]| {
        std::array::from_fn(|channel| {
            let source_span = SOURCE_CHECKER[channel] - SOURCE_DARK[channel];
            let decoded_span = i32::from(checker[channel]) - i32::from(dark[channel]);
            let decoded = i32::from(dark[channel])
                + (source[channel] - SOURCE_DARK[channel]) * decoded_span / source_span;
            decoded.clamp(0, 255) as u8
        })
    };

    [
        (MotionPaletteClass::Dark, dark),
        (MotionPaletteClass::Checker, checker),
        (MotionPaletteClass::Orange, transform(SOURCE_ORANGE)),
        (MotionPaletteClass::Purple, transform(SOURCE_PURPLE)),
    ]
}

fn component_median(samples: &[[u8; 3]]) -> [u8; 3] {
    std::array::from_fn(|channel| {
        let mut values: Vec<_> = samples.iter().map(|rgb| rgb[channel]).collect();
        values.sort_unstable();
        values[values.len() / 2]
    })
}

fn motion_palette_frame_stats(
    rgba: &[u8],
    width: usize,
    height: usize,
) -> (PaletteTargetStats, PaletteTargetStats, usize, [[u8; 3]; 4]) {
    if width == 0 || height == 0 {
        return (
            PaletteTargetStats::default(),
            PaletteTargetStats::default(),
            0,
            [[0; 3]; 4],
        );
    }
    // Match the existing structural probe's 128-column by 64-row sampling
    // budget, but keep a regular grid so disconnected color regions and row
    // origin changes remain observable.
    let cols = width.min(128);
    let rows = height.min(64);
    let mut samples = Vec::with_capacity(cols * rows);
    for row in 0..rows {
        let y = if rows <= 1 {
            0
        } else {
            row * (height - 1) / (rows - 1)
        };
        for col in 0..cols {
            let x = if cols <= 1 {
                0
            } else {
                col * (width - 1) / (cols - 1)
            };
            let p = (y * width + x) * 4;
            samples.push([rgba[p], rgba[p + 1], rgba[p + 2]]);
        }
    }
    let palette = motion_palette_from_samples(&samples);
    let grid: Vec<_> = samples
        .iter()
        .map(|rgb| nearest_motion_palette(rgb[0], rgb[1], rgb[2], &palette))
        .collect();
    (
        palette_target_stats(&grid, cols, rows, MotionPaletteClass::Orange),
        palette_target_stats(&grid, cols, rows, MotionPaletteClass::Purple),
        grid.len(),
        palette.map(|(_, rgb)| rgb),
    )
}

fn palette_target_stats(
    grid: &[MotionPaletteClass],
    cols: usize,
    rows: usize,
    target: MotionPaletteClass,
) -> PaletteTargetStats {
    let mut samples = 0usize;
    let mut min_x = cols;
    let mut max_x = 0usize;
    let mut row_origins = Vec::new();
    for row in 0..rows {
        let mut row_min = cols;
        for col in 0..cols {
            if grid[row * cols + col] != target {
                continue;
            }
            samples += 1;
            min_x = min_x.min(col);
            max_x = max_x.max(col);
            row_min = row_min.min(col);
        }
        if row_min != cols {
            row_origins.push(row_min);
        }
    }
    if samples == 0 {
        return PaletteTargetStats::default();
    }

    let mut visited = vec![false; grid.len()];
    let mut components = 0usize;
    for start in 0..grid.len() {
        if visited[start] || grid[start] != target {
            continue;
        }
        components += 1;
        visited[start] = true;
        let mut stack = vec![start];
        while let Some(index) = stack.pop() {
            let row = index / cols;
            let col = index % cols;
            if row > 0 {
                visit_palette_neighbor(index - cols, grid, &mut visited, target, &mut stack);
            }
            if row + 1 < rows {
                visit_palette_neighbor(index + cols, grid, &mut visited, target, &mut stack);
            }
            if col > 0 {
                visit_palette_neighbor(index - 1, grid, &mut visited, target, &mut stack);
            }
            if col + 1 < cols {
                visit_palette_neighbor(index + 1, grid, &mut visited, target, &mut stack);
            }
        }
    }

    let row_origin_min = row_origins.iter().copied().min().unwrap_or(0);
    let row_origin_max = row_origins.iter().copied().max().unwrap_or(0);
    PaletteTargetStats {
        samples,
        components,
        span_ratio: (max_x - min_x + 1) as f64 / cols.max(1) as f64,
        row_origin_spread_ratio: (row_origin_max - row_origin_min) as f64
            / cols.saturating_sub(1).max(1) as f64,
    }
}

fn visit_palette_neighbor(
    index: usize,
    grid: &[MotionPaletteClass],
    visited: &mut [bool],
    target: MotionPaletteClass,
    stack: &mut Vec<usize>,
) {
    if !visited[index] && grid[index] == target {
        visited[index] = true;
        stack.push(index);
    }
}

impl FrameStats {
    fn observe(
        &mut self,
        packet: &[u8],
        delivery: DeliveryMode,
        observed_at: Instant,
        dump_rgba: Option<&PathBuf>,
        motion_palette: bool,
    ) -> Result<()> {
        if packet.len() < IPC_HEADER_LEN {
            bail!("video packet is only {} bytes", packet.len());
        }
        let kind = packet[0];
        let expected_kind = match delivery {
            DeliveryMode::Native => RAW_KIND,
            DeliveryMode::Compressed => H264_KIND,
        };
        if kind != expected_kind {
            bail!(
                "received video kind {kind}, expected kind {expected_kind} for {} delivery",
                delivery.name()
            );
        }

        let width = le_u32(&packet[4..8]);
        let height = le_u32(&packet[8..12]);
        let ts = le_u64(&packet[20..28]);
        if delivery == DeliveryMode::Native
            && (width == 0 || height == 0 || width > 16_384 || height > 16_384)
        {
            bail!("implausible decoded dimensions {width}x{height}");
        }
        let payload = &packet[IPC_HEADER_LEN..];
        if delivery == DeliveryMode::Native {
            let expected = (width as usize)
                .checked_mul(height as usize)
                .and_then(|n| n.checked_mul(4))
                .context("decoded frame size overflow")?;
            if payload.len() != expected {
                bail!(
                    "decoded {width}x{height} frame has {} RGBA bytes, expected {expected}",
                    payload.len()
                );
            }
        } else if payload.is_empty() {
            bail!("compressed H.264 frame has an empty payload");
        }

        if let Some(last) = self.last_ts {
            if ts < last {
                self.timestamp_regressions += 1;
            } else {
                self.source_intervals_us.push(ts - last);
            }
        }
        if let Some(last) = self.last_arrival {
            self.arrival_intervals_us
                .push(observed_at.saturating_duration_since(last).as_micros() as u64);
        }
        let first_ts = *self.first_ts.get_or_insert(ts);
        let first_arrival = *self.first_arrival.get_or_insert(observed_at);
        let arrival_elapsed_us = observed_at
            .saturating_duration_since(first_arrival)
            .as_micros()
            .min(u128::from(u64::MAX)) as u64;
        let source_elapsed_us = ts.saturating_sub(first_ts);
        let drift_us = i128::from(arrival_elapsed_us) - i128::from(source_elapsed_us);
        self.lag_drift_samples.push((
            arrival_elapsed_us,
            drift_us.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64,
        ));
        self.first_ts.get_or_insert(ts);
        self.last_ts = Some(ts);
        self.last_arrival = Some(observed_at);
        self.frames += 1;
        self.payload_bytes += payload.len() as u64;
        self.key_frames += u64::from((packet[1] & 1) == 1);
        if width != 0 && height != 0 {
            self.dimensions.insert((width, height));
        }

        let mut fingerprint = 0xcbf29ce484222325u64;
        if delivery == DeliveryMode::Native {
            let rgba = payload;
            let pixels = width as usize * height as usize;
            let stride = (pixels / 4096).max(1);
            let mut sampled = 0u64;
            let mut green = 0u64;
            for index in (0..pixels).step_by(stride) {
                let p = index * 4;
                let (r, g, b, a) = (rgba[p], rgba[p + 1], rgba[p + 2], rgba[p + 3]);
                if a != 255 {
                    bail!("decoded RGBA alpha is {a}, expected 255 at pixel {index}");
                }
                if g.saturating_sub(r) > 64 && g.saturating_sub(b) > 64 {
                    green += 1;
                }
                for byte in [r, g, b] {
                    fingerprint ^= byte as u64;
                    fingerprint = fingerprint.wrapping_mul(0x100000001b3);
                }
                sampled += 1;
            }
            if sampled != 0 {
                self.max_green_ratio = self.max_green_ratio.max(green as f64 / sampled as f64);
            }

            // This is intentionally diagnostic-only: a dark desktop or a video
            // with letterboxing can contain valid black rows. A high ratio is
            // printed for visual follow-up, but never fails a production stream.
            let row_step = (height as usize / 64).max(1);
            let col_step = (width as usize / 128).max(1);
            let mut rows = 0u64;
            let mut black_rows = 0u64;
            for y in (0..height as usize).step_by(row_step) {
                let mut cols = 0u64;
                let mut black = 0u64;
                for x in (0..width as usize).step_by(col_step) {
                    let p = (y * width as usize + x) * 4;
                    if rgba[p] < 8 && rgba[p + 1] < 8 && rgba[p + 2] < 8 {
                        black += 1;
                    }
                    cols += 1;
                }
                if cols != 0 && black * 100 >= cols * 95 {
                    black_rows += 1;
                }
                rows += 1;
            }
            if rows != 0 {
                self.max_black_row_ratio = self
                    .max_black_row_ratio
                    .max(black_rows as f64 / rows as f64);
            }
            if motion_palette {
                self.motion_palette.get_or_insert_default().observe(
                    rgba,
                    width as usize,
                    height as usize,
                );
            }
        } else {
            let stride = (payload.len() / 4096).max(1);
            for byte in payload.iter().step_by(stride) {
                fingerprint ^= *byte as u64;
                fingerprint = fingerprint.wrapping_mul(0x100000001b3);
            }
        }
        self.fingerprints.insert(fingerprint);

        if delivery == DeliveryMode::Native && !self.dumped {
            if let Some(path) = dump_rgba {
                std::fs::write(path, payload)
                    .with_context(|| format!("write RGBA dump {}", path.display()))?;
                let metadata = path.with_extension("rgba.json");
                std::fs::write(
                    &metadata,
                    serde_json::to_vec_pretty(&json!({
                        "width": width,
                        "height": height,
                        "format": "rgba8",
                        "timestamp_us": ts,
                        "bytes": payload.len(),
                    }))?,
                )
                .with_context(|| format!("write RGBA metadata {}", metadata.display()))?;
                println!("saved first decoded frame to {}", path.display());
                self.dumped = true;
            }
        }
        Ok(())
    }

    fn summary(&self, elapsed: Duration, delivery: DeliveryMode) -> Value {
        let lag_drift_ms = lag_drift_summary(&self.lag_drift_samples);
        json!({
            "delivery": delivery.name(),
            "frames": self.frames,
            "fps": self.frames as f64 / elapsed.as_secs_f64().max(0.001),
            "payload_bytes": self.payload_bytes,
            "rgba_bytes": if delivery == DeliveryMode::Native { Some(self.payload_bytes) } else { None },
            "key_frames": self.key_frames,
            "dimensions": self.dimensions.iter().map(|(w, h)| format!("{w}x{h}")).collect::<Vec<_>>(),
            "first_timestamp_us": self.first_ts,
            "last_timestamp_us": self.last_ts,
            "source_span_ms": self.first_ts.zip(self.last_ts).map(|(first, last)| last.saturating_sub(first) as f64 / 1000.0),
            "timestamp_regressions": self.timestamp_regressions,
            "arrival_interval_ms": duration_summary_ms(&self.arrival_intervals_us),
            "source_interval_ms": duration_summary_ms(&self.source_intervals_us),
            "lag_drift_ms": lag_drift_ms,
            "sampled_unique_frames": self.fingerprints.len(),
            "max_green_dominant_sample_ratio": if delivery == DeliveryMode::Native { Some(self.max_green_ratio) } else { None },
            "max_nearly_black_row_ratio": if delivery == DeliveryMode::Native { Some(self.max_black_row_ratio) } else { None },
            "motion_palette": self.motion_palette.as_ref().map(MotionPaletteStats::summary),
        })
    }
}

fn percentile(sorted: &[u64], numerator: usize, denominator: usize) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let index = (sorted.len() - 1)
        .saturating_mul(numerator)
        .div_ceil(denominator);
    sorted.get(index).copied()
}

fn duration_summary_ms(samples_us: &[u64]) -> Value {
    let mut sorted = samples_us.to_vec();
    sorted.sort_unstable();
    let avg = if sorted.is_empty() {
        None
    } else {
        Some(sorted.iter().map(|value| *value as f64).sum::<f64>() / sorted.len() as f64 / 1000.0)
    };
    json!({
        "samples": sorted.len(),
        "avg": avg,
        "p50": percentile(&sorted, 50, 100).map(|value| value as f64 / 1000.0),
        "p95": percentile(&sorted, 95, 100).map(|value| value as f64 / 1000.0),
        "p99": percentile(&sorted, 99, 100).map(|value| value as f64 / 1000.0),
        "max": sorted.last().map(|value| *value as f64 / 1000.0),
    })
}

fn lag_drift_summary(samples: &[(u64, i64)]) -> Value {
    let first = samples.first().map(|(_, drift)| *drift).unwrap_or(0);
    let final_value = samples.last().map(|(_, drift)| *drift).unwrap_or(0);
    let min = samples.iter().map(|(_, drift)| *drift).min().unwrap_or(0);
    let max = samples.iter().map(|(_, drift)| *drift).max().unwrap_or(0);

    // Ordinary least squares over elapsed arrival time. A positive slope means
    // the receiver is losing ground against the source clock. No pass/fail
    // threshold is embedded here; the A/B protocol compares confidence-backed
    // distributions and leaves policy limits to the release owner.
    let n = samples.len() as f64;
    let (slope_ms_per_s, r_squared) = if samples.len() < 2 {
        (None, None)
    } else {
        let mean_x = samples
            .iter()
            .map(|(x, _)| *x as f64 / 1_000_000.0)
            .sum::<f64>()
            / n;
        let mean_y = samples.iter().map(|(_, y)| *y as f64 / 1000.0).sum::<f64>() / n;
        let mut covariance = 0.0;
        let mut variance_x = 0.0;
        let mut variance_y = 0.0;
        for (x, y) in samples {
            let dx = *x as f64 / 1_000_000.0 - mean_x;
            let dy = *y as f64 / 1000.0 - mean_y;
            covariance += dx * dy;
            variance_x += dx * dx;
            variance_y += dy * dy;
        }
        if variance_x <= f64::EPSILON {
            (None, None)
        } else {
            let slope = covariance / variance_x;
            let r2 = if variance_y <= f64::EPSILON {
                1.0
            } else {
                (covariance * covariance / (variance_x * variance_y)).clamp(0.0, 1.0)
            };
            (Some(slope), Some(r2))
        }
    };
    json!({
        "first": first as f64 / 1000.0,
        "final": final_value as f64 / 1000.0,
        "net_growth": (final_value - first) as f64 / 1000.0,
        "range": (max - min) as f64 / 1000.0,
        "slope_ms_per_s": slope_ms_per_s,
        "r_squared": r_squared,
    })
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let config = parse_args()?;
    let client = NodeClient::new().context("resolve AllMyStuff node-control socket")?;
    let scan = client
        .request("scan_self", Value::Null)
        .await
        .context("query running AllMyStuff node (is allmystuff-serve running?)")?;
    let local_id = scan
        .get("node_id")
        .and_then(Value::as_str)
        .context("scan_self response has no node_id")?
        .to_string();
    let snapshot = wait_ready(&client, Duration::from_secs(10)).await?;
    let local_sinks = endpoints_from_values(
        scan.get("capabilities")
            .and_then(Value::as_array)
            .into_iter()
            .flatten(),
        "display",
        "sink",
    );
    if config.list {
        let daemon = daemon_view(&client).await?;
        let mut remote_sources = remote_sources_from_snapshot(&snapshot);
        for source in daemon.remote_sources {
            remote_sources.entry(source.id.clone()).or_insert(source);
        }
        let listing = json!({
            "local_node": local_id,
            "local_display_sinks": local_sinks.iter().map(Endpoint::display_value).collect::<Vec<_>>(),
            "remote_screen_sources": remote_sources.values().map(Endpoint::display_value).collect::<Vec<_>>(),
            "selected_ice_paths": daemon.paths,
            "note": "A source with this local_node is intentionally not a transport test.",
        });
        println!("{}", serde_json::to_string_pretty(&listing)?);
        return Ok(());
    }

    // Match the GUI's viewer pump instead of sleeping blindly between polls.
    // The event stream is local node-control IPC. It carries only readiness
    // notifications and never changes the mesh, signaling, ICE, STUN, or TURN
    // protocols.
    let (event_tx, mut event_rx) = mpsc::channel(256);
    let event_client = NodeClient::new().context("resolve event-stream node-control socket")?;
    let event_task = tokio::spawn(async move { event_client.subscribe_events(event_tx).await });

    let source = if let Some(id) = config.source.as_deref() {
        exact_source(id)?
    } else {
        let daemon = daemon_view(&client).await?;
        let mut remote_sources = remote_sources_from_snapshot(&snapshot);
        for source in daemon.remote_sources {
            remote_sources.entry(source.id.clone()).or_insert(source);
        }
        choose_source(&config, &local_id, &remote_sources)?
    };
    let sink = choose_sink(&config, &local_sinks)?;
    let remote_id = canonical_node(&source.node);
    if remote_id == canonical_node(&local_id) {
        bail!(
            "refusing same-node display source {}: a display self-route neither captures nor crosses ICE, so it cannot be an end-to-end transport test",
            source.id
        );
    }

    let (paths, path_proof) = if config.source.is_some() {
        println!(
            "exact-source mode: peer inventory skipped; successful route activation and media delivery are the authenticated application data-plane proof"
        );
        (
            Vec::new(),
            "exact_route_activation_and_media_delivery".to_string(),
        )
    } else {
        let paths = wait_for_ice_path(
            &client,
            &remote_id,
            Duration::from_secs(config.active_timeout_secs),
        )
        .await?;
        if paths.is_empty() && !config.allow_no_ice_proof {
            bail!(
                "peer {remote_id} has no authenticated ACTIVE selected ICE pair; use --allow-no-ice-proof only for node-control debugging, never as a release gate"
            );
        }
        (paths, "selected_authenticated_ice_pair".to_string())
    };
    println!(
        "production probe: {} -> {} ({} cycle(s), {} delivery)",
        source.id,
        sink.id,
        config.cycles,
        config.delivery.name()
    );
    if !paths.is_empty() {
        println!(
            "selected ICE path evidence: {}",
            serde_json::to_string_pretty(&paths)?
        );
    }

    let mut cycle_summaries = Vec::new();
    let mut dumped = false;
    for cycle in 1..=config.cycles {
        let expected_route = format!("route:{}→{}", source.id, sink.id);
        let before = client
            .request("session_snapshot", Value::Null)
            .await
            .context("check for a pre-existing display route")?;
        if route_state(&before, &expected_route).is_some_and(|state| state != "torn_down") {
            bail!(
                "route {expected_route} is already live; close its console before probing so the harness cannot steal or disconnect a user's stream"
            );
        }
        let handle = client
            .request(
                "connect_route_handle",
                json!({
                    "from": source.id,
                    "to": sink.id,
                    "media": "display",
                    "video": ["h264"],
                    "session": null,
                }),
            )
            .await
            .with_context(|| format!("cycle {cycle}: offer production display route"))?
            .as_object()
            .context("connect_route_handle did not return an object")?
            .clone();
        let route = handle
            .get("route_id")
            .and_then(Value::as_str)
            .context("connect_route_handle did not return a route id")?
            .to_string();
        let generation = handle
            .get("generation")
            .and_then(Value::as_u64)
            .filter(|value| (1..=JS_SAFE_INTEGER_MAX).contains(value))
            .context(
                "connect_route_handle returned a generation that JavaScript cannot round-trip",
            )?;
        if route != expected_route {
            bail!(
                "connect_route_handle returned unexpected id {route} (expected {expected_route})"
            );
        }

        let mut token = client
            .request(
                "video_watch",
                json!({
                    "route_id": route,
                    "decode": config.delivery == DeliveryMode::Native
                }),
            )
            .await
            .with_context(|| format!("cycle {cycle}: claim video watch"))?
            .as_u64()
            .filter(|value| (1..=JS_SAFE_INTEGER_MAX).contains(value))
            .context("video_watch returned a token that JavaScript cannot round-trip")?;

        let exercise = exercise_cycle(
            &client,
            &config,
            cycle,
            &route,
            &mut token,
            &mut event_rx,
            if dumped {
                None
            } else {
                config.dump_rgba.as_ref()
            },
        )
        .await;

        // Teardown is attempted even when validation failed, so a probe never
        // deliberately leaves a capture route running on either endpoint.
        let _ = client
            .request(
                "video_unwatch",
                json!({ "route_id": route, "token": token }),
            )
            .await;
        let _ = client
            .request(
                "disconnect_route",
                json!({ "route_id": route, "generation": generation }),
            )
            .await;

        let summary = exercise.with_context(|| format!("cycle {cycle} failed"))?;
        dumped |= config.dump_rgba.is_some();
        println!(
            "cycle {cycle} passed: {}",
            serde_json::to_string_pretty(&summary)?
        );
        cycle_summaries.push(summary);
        wait_route_gone(&client, &route, Duration::from_secs(5)).await;
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    println!(
        "PASS: real remote display route activated on the authenticated application data plane and produced structurally valid {} video through every requested gate",
        config.delivery.name()
    );
    let report = json!({
        "delivery": config.delivery.name(),
        "source": source.display_value(),
        "sink": sink.display_value(),
        "cycles": cycle_summaries,
        "ice_paths": paths,
        "path_proof": path_proof,
        "peer_inventory_skipped": config.source.is_some(),
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    if let Some(path) = &config.json_out {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create report directory {}", parent.display()))?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(&report)?)
            .with_context(|| format!("write JSON report {}", path.display()))?;
        println!("saved JSON report to {}", path.display());
    }
    event_task.abort();
    Ok(())
}

async fn exercise_cycle(
    client: &NodeClient,
    config: &Config,
    cycle: u32,
    route: &str,
    token: &mut u64,
    events: &mut mpsc::Receiver<NodeEvent>,
    dump_rgba: Option<&PathBuf>,
) -> Result<Value> {
    wait_route_active(
        client,
        route,
        Duration::from_secs(config.active_timeout_secs),
    )
    .await?;

    if config.mode.is_some() || config.resize_edge.is_some() || config.fps.is_some() {
        client
            .request(
                "tune_route",
                json!({
                    "route_id": route,
                    "max_edge": config.resize_edge,
                    "bitrate": null,
                    "fps": config.fps,
                    "game": config.mode.as_deref() == Some("game"),
                    "mode": config.mode,
                }),
            )
            .await
            .context("send in-band video tune over the existing ICE data path")?;
    }

    let first = collect_frames(
        client,
        route,
        *token,
        Duration::from_secs(config.phase_secs),
        Duration::from_secs(config.first_frame_timeout_secs),
        config.delivery,
        events,
        dump_rgba,
        config.motion_palette,
    )
    .await
    .with_context(|| format!("cycle {cycle}: initial stream"))?;

    let mut phases = vec![json!({ "name": "initial", "stats": first })];
    if config.rewatch {
        client
            .request(
                "video_unwatch",
                json!({ "route_id": route, "token": *token }),
            )
            .await
            .context("drop native decoder/watch")?;
        tokio::time::sleep(Duration::from_millis(150)).await;
        *token = client
            .request(
                "video_watch",
                json!({
                    "route_id": route,
                    "decode": config.delivery == DeliveryMode::Native
                }),
            )
            .await
            .context("recreate decoder/watch mid-stream")?
            .as_u64()
            .filter(|value| (1..=JS_SAFE_INTEGER_MAX).contains(value))
            .context(
                "replacement video_watch returned a token that JavaScript cannot round-trip",
            )?;
        // This is the same refresh request a native decoder emits when its
        // first post-rewatch AU is a delta.  It remains on the existing route's
        // ICE data channel; no media bytes are placed on signaling.
        let _ = client
            .request("video_refresh", json!({ "route_id": route }))
            .await;
        let resumed = collect_frames(
            client,
            route,
            *token,
            Duration::from_secs(config.phase_secs),
            Duration::from_secs(config.first_frame_timeout_secs),
            config.delivery,
            events,
            None,
            config.motion_palette,
        )
        .await
        .with_context(|| format!("cycle {cycle}: decoder rewatch/resume"))?;
        phases.push(json!({ "name": "viewer_rewatch", "stats": resumed }));
    }
    Ok(json!({ "route": route, "phases": phases }))
}

#[allow(clippy::too_many_arguments)]
async fn collect_frames(
    client: &NodeClient,
    route: &str,
    token: u64,
    duration: Duration,
    first_frame_timeout: Duration,
    delivery: DeliveryMode,
    events: &mut mpsc::Receiver<NodeEvent>,
    dump_rgba: Option<&PathBuf>,
    motion_palette: bool,
) -> Result<Value> {
    let started = Instant::now();
    let first_deadline = started + first_frame_timeout;
    let finish_after_first = duration;
    let mut first_at = None;
    let mut stats = FrameStats::default();
    let mut polls = 0u64;
    let mut nonempty_polls = 0u64;
    let mut max_packets_per_poll = 0usize;
    let mut poll_us = Vec::new();
    let mut ready_events = 0u64;
    let mut watchdog_wakes = 0u64;
    let mut feedback_reports = Vec::new();
    let mut feedback_started = started;
    let mut feedback_frames = 0u64;
    let mut watchdog = tokio::time::interval(Duration::from_millis(16));
    watchdog.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let poll_started = Instant::now();
        let batch = client
            .request_bytes("video_poll", json!({ "route_id": route, "token": token }))
            .await
            .context("poll video IPC queue")?;
        let observed_at = Instant::now();
        polls += 1;
        poll_us.push(
            observed_at
                .saturating_duration_since(poll_started)
                .as_micros()
                .min(u128::from(u64::MAX)) as u64,
        );
        let packets = split_batch(&batch)?;
        if !packets.is_empty() {
            nonempty_polls += 1;
            max_packets_per_poll = max_packets_per_poll.max(packets.len());
        }
        for packet in packets {
            stats.observe(packet, delivery, observed_at, dump_rgba, motion_palette)?;
            first_at.get_or_insert(observed_at);
        }
        let feedback_elapsed = observed_at.saturating_duration_since(feedback_started);
        if feedback_elapsed >= Duration::from_secs(2) {
            let interval_frames = stats.frames.saturating_sub(feedback_frames);
            let elapsed_micros = feedback_elapsed.as_micros().max(1);
            let recv_fps = (u128::from(interval_frames)
                .saturating_mul(1_000_000)
                .saturating_add(elapsed_micros / 2)
                / elapsed_micros)
                .min(u128::from(u32::MAX)) as u32;
            let report_started = Instant::now();
            client
                .request(
                    "video_feedback",
                    json!({
                        "route_id": route,
                        "watcher_token": token,
                        "recv_fps": recv_fps,
                        "decode_fails": 0,
                        "queue_depth": 0,
                    }),
                )
                .await
                .context("report viewer flow to the remote encoder")?;
            feedback_reports.push(json!({
                "interval_ms": feedback_elapsed.as_secs_f64() * 1000.0,
                "frames": interval_frames,
                "recv_fps": recv_fps,
                "round_trip_ms": report_started.elapsed().as_secs_f64() * 1000.0,
            }));
            feedback_started = Instant::now();
            feedback_frames = stats.frames;
        }
        if let Some(first) = first_at {
            if first.elapsed() >= finish_after_first {
                break;
            }
        } else if Instant::now() >= first_deadline {
            let snapshot = client.request("session_snapshot", Value::Null).await.ok();
            bail!(
                "no decoded frame within {}s; route state: {}",
                first_frame_timeout.as_secs(),
                snapshot
                    .as_ref()
                    .and_then(|s| route_state(s, route))
                    .unwrap_or_else(|| "absent".to_string())
            );
        }
        loop {
            tokio::select! {
                _ = watchdog.tick() => {
                    watchdog_wakes += 1;
                    break;
                }
                event = events.recv() => {
                    match event {
                        Some(NodeEvent::Emit { event, payload })
                            if event == "allmystuff://video-ready"
                                && payload.as_str() == Some(route) =>
                        {
                            ready_events += 1;
                            break;
                        }
                        Some(_) => {}
                        None => {
                            // Keep the production watchdog behavior if the
                            // diagnostic subscriber itself disappears.
                            watchdog.tick().await;
                            watchdog_wakes += 1;
                            break;
                        }
                    }
                }
            }
        }
    }
    if stats.frames < 3 {
        bail!("only {} decoded frames arrived", stats.frames);
    }
    if stats.timestamp_regressions != 0 {
        bail!(
            "{} decoded timestamp regression(s) observed",
            stats.timestamp_regressions
        );
    }
    let first_frame_ms =
        first_at.map(|first| first.saturating_duration_since(started).as_secs_f64() * 1000.0);
    Ok(json!({
        "frames": stats.summary(first_at.unwrap_or(started).elapsed(), delivery),
        "viewer_pump": {
            "first_frame_ms": first_frame_ms,
            "polls": polls,
            "nonempty_polls": nonempty_polls,
            "empty_polls": polls.saturating_sub(nonempty_polls),
            "max_packets_per_poll": max_packets_per_poll,
            "poll_round_trip_ms": duration_summary_ms(&poll_us),
            "ready_events": ready_events,
            "watchdog_wakes": watchdog_wakes,
            "feedback_reports": feedback_reports,
        }
    }))
}

fn split_batch(batch: &[u8]) -> Result<Vec<&[u8]>> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset < batch.len() {
        if batch.len() - offset < 4 {
            bail!("truncated video batch length at byte {offset}");
        }
        let len = le_u32(&batch[offset..offset + 4]) as usize;
        offset += 4;
        if len == 0 || len > batch.len() - offset {
            bail!("invalid video packet length {len} at byte {}", offset - 4);
        }
        out.push(&batch[offset..offset + len]);
        offset += len;
    }
    Ok(out)
}

async fn wait_ready(client: &NodeClient, timeout: Duration) -> Result<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let snapshot = client
            .request("session_snapshot", Value::Null)
            .await
            .context("query session snapshot")?;
        if snapshot
            .get("ready")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Ok(snapshot);
        }
        if Instant::now() >= deadline {
            bail!("AllMyStuff node did not become mesh-ready within {timeout:?}");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_route_active(client: &NodeClient, route: &str, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let snapshot = client
            .request("session_snapshot", Value::Null)
            .await
            .context("query route state")?;
        match route_state(&snapshot, route).as_deref() {
            Some("active") => return Ok(()),
            Some("rejected") => bail!("route {route} was rejected"),
            Some("torn_down") => bail!("route {route} was torn down before streaming"),
            _ => {}
        }
        if Instant::now() >= deadline {
            bail!(
                "route {route} did not become active within {timeout:?} (last state: {})",
                route_state(&snapshot, route).unwrap_or_else(|| "absent".to_string())
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_route_gone(client: &NodeClient, route: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let gone = client
            .request("session_snapshot", Value::Null)
            .await
            .ok()
            .and_then(|s| route_state(&s, route))
            .is_none_or(|state| state == "torn_down");
        if gone {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn route_state(snapshot: &Value, route: &str) -> Option<String> {
    let live = snapshot
        .get("routes")?
        .as_array()?
        .iter()
        .find(|candidate| candidate.pointer("/route/id").and_then(Value::as_str) == Some(route))?;
    let state = live.get("state")?;
    state
        .as_str()
        .or_else(|| state.get("state").and_then(Value::as_str))
        .map(str::to_string)
}

async fn wait_for_ice_path(
    client: &NodeClient,
    peer: &str,
    timeout: Duration,
) -> Result<Vec<Value>> {
    let deadline = Instant::now() + timeout;
    loop {
        let view = daemon_view(client).await?;
        let paths = view
            .paths
            .into_iter()
            .filter(|path| {
                path.get("peer")
                    .and_then(Value::as_str)
                    .is_some_and(|id| canonical_node(id) == canonical_node(peer))
                    && path.get("status").and_then(Value::as_str) == Some("active")
                    && path
                        .get("authenticated")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    && path.get("selected_pair").is_some_and(Value::is_object)
            })
            .collect::<Vec<_>>();
        if !paths.is_empty() || Instant::now() >= deadline {
            return Ok(paths);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn daemon_view(client: &NodeClient) -> Result<DaemonView> {
    let networks = client
        .request("mesh_networks", Value::Null)
        .await
        .context("query joined mesh networks")?;
    let mut paths = Vec::new();
    let mut sources = BTreeMap::new();
    for network in networks
        .get("networks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(config_id) = network.get("config_id").and_then(Value::as_str) else {
            continue;
        };
        let Ok(view) = client
            .request("mesh_peers", json!({ "network": config_id }))
            .await
        else {
            continue;
        };
        for peer in view
            .get("peers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(endpoints) = peer.pointer("/capabilities/extra/endpoints") {
                for source in endpoints_from_values(
                    endpoints.as_array().into_iter().flatten(),
                    "display",
                    "source",
                ) {
                    sources.entry(source.id.clone()).or_insert(source);
                }
            }
            paths.push(json!({
                "network": config_id,
                "network_id": network.get("network_id"),
                "network_label": network.get("label"),
                "peer": peer.get("device_id"),
                "status": peer.get("status"),
                "authenticated": peer.get("authenticated"),
                "selected_pair": peer.get("selected_pair"),
                "rtt_ms": peer.get("rtt_ms"),
                "needs_turn": peer.get("needs_turn"),
            }));
        }
    }
    Ok(DaemonView {
        paths,
        remote_sources: sources.into_values().collect(),
    })
}

fn remote_sources_from_snapshot(snapshot: &Value) -> BTreeMap<String, Endpoint> {
    let mut out = BTreeMap::new();
    for peer in snapshot
        .get("peers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for source in endpoints_from_values(
            peer.get("capabilities")
                .and_then(Value::as_array)
                .into_iter()
                .flatten(),
            "display",
            "source",
        ) {
            out.entry(source.id.clone()).or_insert(source);
        }
    }
    out
}

fn endpoints_from_values<'a>(
    values: impl Iterator<Item = &'a Value>,
    media: &str,
    flow: &str,
) -> Vec<Endpoint> {
    values
        .filter_map(|value| Endpoint::from_value(value, media, flow))
        .collect()
}

fn choose_source(
    config: &Config,
    local_id: &str,
    sources: &BTreeMap<String, Endpoint>,
) -> Result<Endpoint> {
    if let Some(id) = &config.source {
        return sources
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow!("source {id} is not an advertised remote display source"));
    }
    let mut candidates = sources
        .values()
        .filter(|source| {
            config
                .peer
                .as_ref()
                .is_none_or(|peer| canonical_node(&source.node) == canonical_node(peer))
                && canonical_node(&source.node) != canonical_node(local_id)
        })
        .cloned()
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        bail!("no remote screen source matched; run --list and pass --source <capability-id>");
    }
    let nodes = candidates
        .iter()
        .map(|source| canonical_node(&source.node))
        .collect::<BTreeSet<_>>();
    if nodes.len() != 1 {
        bail!(
            "{} remote machines advertise screens; pass --peer <node-id> or --source <capability-id>",
            nodes.len()
        );
    }
    candidates.sort_by_key(|source| {
        (
            source.origin != "screen",
            !source.id.ends_with(":screen"),
            !source.default,
            source.id.clone(),
        )
    });
    Ok(candidates.remove(0))
}

fn exact_source(id: &str) -> Result<Endpoint> {
    let (node, capability) = id
        .split_once(':')
        .ok_or_else(|| anyhow!("exact source {id} has no capability suffix"))?;
    if node.is_empty() || !capability.starts_with("screen") {
        bail!("exact source {id} is not a screen capability");
    }
    Ok(Endpoint {
        id: id.to_string(),
        node: node.to_string(),
        label: String::new(),
        origin: "screen".to_string(),
        default: capability == "screen",
    })
}

fn choose_sink(config: &Config, sinks: &[Endpoint]) -> Result<Endpoint> {
    if let Some(id) = &config.sink {
        return sinks
            .iter()
            .find(|sink| sink.id == *id)
            .cloned()
            .ok_or_else(|| anyhow!("sink {id} is not a local display sink"));
    }
    sinks
        .iter()
        .find(|sink| sink.default)
        .or_else(|| sinks.first())
        .cloned()
        .context("this node advertises no local display sink")
}

fn canonical_node(value: &str) -> String {
    let node = value.split_once(':').map(|(node, _)| node).unwrap_or(value);
    match node.rsplit_once('-') {
        Some((bare, suffix))
            if suffix.len() == 5
                && suffix.chars().all(|c| c.is_ascii_alphanumeric())
                && bare.len() >= 32 =>
        {
            bare.to_string()
        }
        _ => node.to_string(),
    }
}

fn parse_args() -> Result<Config> {
    let mut config = Config::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            "--list" => config.list = true,
            "--source" => config.source = Some(next_arg(&mut args, "--source")?),
            "--peer" => config.peer = Some(next_arg(&mut args, "--peer")?),
            "--sink" => config.sink = Some(next_arg(&mut args, "--sink")?),
            "--seconds" => {
                config.phase_secs = next_arg(&mut args, "--seconds")?.parse()?;
            }
            "--cycles" => config.cycles = next_arg(&mut args, "--cycles")?.parse()?,
            "--active-timeout" => {
                config.active_timeout_secs = next_arg(&mut args, "--active-timeout")?.parse()?;
            }
            "--first-frame-timeout" => {
                config.first_frame_timeout_secs =
                    next_arg(&mut args, "--first-frame-timeout")?.parse()?;
            }
            "--no-rewatch" => config.rewatch = false,
            "--delivery" => {
                config.delivery = DeliveryMode::parse(&next_arg(&mut args, "--delivery")?)?;
            }
            "--resize-edge" => {
                config.resize_edge = Some(next_arg(&mut args, "--resize-edge")?.parse()?);
            }
            "--fps" => {
                let fps = next_arg(&mut args, "--fps")?.parse::<u32>()?;
                if !(1..=240).contains(&fps) {
                    bail!("--fps must be between 1 and 240");
                }
                config.fps = Some(fps);
            }
            "--mode" => {
                let mode = next_arg(&mut args, "--mode")?;
                if !matches!(
                    mode.as_str(),
                    "balanced" | "game" | "studio" | "studio-lossless"
                ) {
                    bail!("invalid --mode {mode}");
                }
                config.mode = Some(mode);
            }
            "--dump-rgba" => {
                config.dump_rgba = Some(PathBuf::from(next_arg(&mut args, "--dump-rgba")?));
            }
            "--motion-palette" => config.motion_palette = true,
            "--json-out" => {
                config.json_out = Some(PathBuf::from(next_arg(&mut args, "--json-out")?));
            }
            "--allow-no-ice-proof" => config.allow_no_ice_proof = true,
            other => bail!("unknown argument {other}; run --help"),
        }
    }
    if config.phase_secs == 0 || config.cycles == 0 {
        bail!("--seconds and --cycles must be greater than zero");
    }
    if config.motion_palette && config.delivery != DeliveryMode::Native {
        bail!("--motion-palette requires --delivery native");
    }
    Ok(config)
}

fn next_arg(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String> {
    args.next().ok_or_else(|| anyhow!("{flag} needs a value"))
}

fn print_help() {
    println!(
        r#"Production AllMyStuff video probe

USAGE
  cargo run --manifest-path node/Cargo.toml --example video_prod_probe -- --list
  cargo run --release --manifest-path node/Cargo.toml --example video_prod_probe -- [OPTIONS]

The probe attaches to the already-running local allmystuff-serve process. Both
machines must already be joined/authorized and running the build under test.
It refuses a same-machine source because that route does not cross ICE.

OPTIONS
  --list                    Print remote screen sources and selected ICE pairs
  --source ID               Exact remote screen capability; skips peer inventory
  --peer ID                 Auto-pick the primary screen from this remote node
  --sink ID                 Local display sink (default: default local display)
  --seconds N               Seconds collected per phase (default: 5)
  --cycles N                Full disconnect/reopen cycles (default: 2)
  --delivery MODE           native|compressed (default: native)
  --no-rewatch              Skip watcher/decoder teardown and recreation
  --resize-edge N           Tune the live route to this maximum edge
  --fps N                   Request an explicit 1..240 capture cadence
  --mode MODE               balanced|game|studio|studio-lossless
  --active-timeout N        Route/ICE timeout seconds (default: 20)
  --first-frame-timeout N   Decode startup timeout seconds (default: 12)
  --dump-rgba PATH          Save the first raw RGBA frame + PATH.rgba.json
  --motion-palette          Score the deterministic orange/purple motion pattern
  --json-out PATH           Write the final machine-readable report
  --allow-no-ice-proof      Diagnostic only; never use this for a release gate
"#
    );
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().expect("four-byte slice"))
}

fn le_u64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes.try_into().expect("eight-byte slice"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_batch_rejects_partial_and_zero_packets() {
        assert!(split_batch(&[1, 0, 0]).is_err());
        assert!(split_batch(&[0, 0, 0, 0]).is_err());
        assert!(split_batch(&[4, 0, 0, 0, 1, 2]).is_err());
    }

    #[test]
    fn split_batch_preserves_packet_boundaries() {
        let bytes = [2, 0, 0, 0, 1, 2, 3, 0, 0, 0, 4, 5, 6];
        assert_eq!(
            split_batch(&bytes).unwrap(),
            vec![&[1, 2][..], &[4, 5, 6][..]]
        );
    }

    #[test]
    fn canonical_node_strips_only_display_suffix() {
        let bare = "abcdefghijklmnopqrstuvwxyz234567abcdefghijklmnopqrst";
        assert_eq!(canonical_node(&format!("{bare}-A12BC:screen")), bare);
        assert_eq!(canonical_node("friendly-name:screen"), "friendly-name");
    }

    #[test]
    fn exact_source_accepts_screen_ids_without_inventory() {
        let source = exact_source("node-ABCDE:screen:42").unwrap();
        assert_eq!(source.node, "node-ABCDE");
        assert_eq!(source.id, "node-ABCDE:screen:42");
        assert_eq!(source.origin, "screen");
    }

    #[test]
    fn exact_source_rejects_non_screen_ids() {
        assert!(exact_source("node-ABCDE:terminal").is_err());
        assert!(exact_source("missing-suffix").is_err());
    }

    #[test]
    fn raw_frame_validator_checks_geometry_and_alpha() {
        let mut packet = vec![0; IPC_HEADER_LEN + 2 * 2 * 4];
        packet[0] = RAW_KIND;
        packet[4..8].copy_from_slice(&2u32.to_le_bytes());
        packet[8..12].copy_from_slice(&2u32.to_le_bytes());
        for pixel in packet[IPC_HEADER_LEN..].chunks_exact_mut(4) {
            pixel.copy_from_slice(&[10, 20, 30, 255]);
        }
        let mut stats = FrameStats::default();
        stats
            .observe(&packet, DeliveryMode::Native, Instant::now(), None, false)
            .unwrap();
        assert_eq!(stats.frames, 1);
        packet[IPC_HEADER_LEN + 3] = 0;
        assert!(stats
            .observe(&packet, DeliveryMode::Native, Instant::now(), None, false)
            .is_err());
    }

    #[test]
    fn compressed_frame_validator_rejects_native_and_empty_payloads() {
        let mut packet = vec![0; IPC_HEADER_LEN + 4];
        packet[0] = H264_KIND;
        packet[1] = 1;
        packet[4..8].copy_from_slice(&2u32.to_le_bytes());
        packet[8..12].copy_from_slice(&2u32.to_le_bytes());
        packet[IPC_HEADER_LEN..].copy_from_slice(&[0, 0, 0, 1]);
        let mut stats = FrameStats::default();
        stats
            .observe(
                &packet,
                DeliveryMode::Compressed,
                Instant::now(),
                None,
                false,
            )
            .unwrap();
        assert_eq!(stats.frames, 1);
        packet.truncate(IPC_HEADER_LEN);
        assert!(stats
            .observe(
                &packet,
                DeliveryMode::Compressed,
                Instant::now(),
                None,
                false,
            )
            .is_err());
        packet.push(1);
        packet[0] = RAW_KIND;
        assert!(stats
            .observe(
                &packet,
                DeliveryMode::Compressed,
                Instant::now(),
                None,
                false,
            )
            .is_err());
    }

    #[test]
    fn lag_drift_slope_distinguishes_backlog_growth() {
        let stable = lag_drift_summary(&[(0, 0), (1_000_000, 100), (2_000_000, 0)]);
        let growing = lag_drift_summary(&[(0, 0), (1_000_000, 10_000), (2_000_000, 20_000)]);
        assert!(
            stable["slope_ms_per_s"].as_f64().unwrap().abs() < 0.1,
            "{stable}"
        );
        assert_eq!(growing["slope_ms_per_s"].as_f64().unwrap(), 10.0);
        assert_eq!(growing["r_squared"].as_f64().unwrap(), 1.0);
    }

    fn palette_frame(width: usize, height: usize) -> Vec<u8> {
        let mut rgba = vec![0u8; width * height * 4];
        for row in 0..height {
            for col in 0..width {
                let rgb = if ((row / 8) + (col / 8)) % 2 == 0 {
                    [112, 128, 144]
                } else {
                    [38, 42, 54]
                };
                let p = (row * width + col) * 4;
                rgba[p..p + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
        rgba
    }

    fn paint_rect(
        rgba: &mut [u8],
        width: usize,
        x: std::ops::Range<usize>,
        y: std::ops::Range<usize>,
        rgb: [u8; 3],
    ) {
        for row in y {
            for col in x.clone() {
                let p = (row * width + col) * 4;
                rgba[p..p + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
    }

    #[test]
    fn motion_palette_clean_targets_are_single_components() {
        let (width, height) = (128usize, 64usize);
        let mut rgba = palette_frame(width, height);
        paint_rect(&mut rgba, width, 10..30, 10..20, [255, 165, 0]);
        paint_rect(&mut rgba, width, 80..100, 30..40, [147, 112, 219]);
        let (orange, purple, sampled, decoded_palette) =
            motion_palette_frame_stats(&rgba, width, height);
        assert_eq!(sampled, width * height);
        assert_eq!(
            decoded_palette,
            [
                [38, 42, 54],
                [112, 128, 144],
                [255, 165, 0],
                [147, 112, 219]
            ]
        );
        assert_eq!((orange.samples, orange.components), (200, 1));
        assert_eq!((purple.samples, purple.components), (200, 1));
        assert_eq!(orange.row_origin_spread_ratio, 0.0);
        assert_eq!(purple.row_origin_spread_ratio, 0.0);
        assert_eq!(orange.span_ratio, 20.0 / 128.0);
        assert_eq!(purple.span_ratio, 20.0 / 128.0);
    }

    #[test]
    fn motion_palette_exposes_split_and_trailing_regions_without_a_policy_threshold() {
        let (width, height) = (128usize, 64usize);
        let mut split = palette_frame(width, height);
        paint_rect(&mut split, width, 10..30, 10..15, [255, 165, 0]);
        paint_rect(&mut split, width, 40..60, 16..21, [255, 165, 0]);
        let (split_orange, _, _, _) = motion_palette_frame_stats(&split, width, height);
        assert_eq!(split_orange.components, 2);
        assert_eq!(split_orange.span_ratio, 50.0 / 128.0);
        assert_eq!(split_orange.row_origin_spread_ratio, 30.0 / 127.0);

        let mut trailing = palette_frame(width, height);
        paint_rect(&mut trailing, width, 10..30, 10..20, [255, 165, 0]);
        paint_rect(&mut trailing, width, 30..50, 14..16, [255, 165, 0]);
        let (trailing_orange, _, _, _) = motion_palette_frame_stats(&trailing, width, height);
        assert_eq!(trailing_orange.components, 1);
        assert_eq!(trailing_orange.span_ratio, 40.0 / 128.0);
        assert!(trailing_orange.samples > 200);
    }

    #[test]
    fn motion_palette_calibrates_the_measured_decode_transform() {
        let (width, height) = (128usize, 64usize);
        let mut rgba = palette_frame(width, height);
        for pixel in rgba.chunks_exact_mut(4) {
            let source = [pixel[0], pixel[1], pixel[2]];
            for channel in 0..3 {
                pixel[channel] = (i32::from(source[channel]) * 3 / 2 + 10).clamp(0, 255) as u8;
            }
        }
        paint_rect(&mut rgba, width, 10..30, 10..20, [255, 240, 10]);
        paint_rect(&mut rgba, width, 80..100, 30..40, [230, 178, 255]);

        let (orange, purple, sampled, decoded_palette) =
            motion_palette_frame_stats(&rgba, width, height);
        assert_eq!(sampled, width * height);
        assert_eq!(decoded_palette[0], [67, 73, 91]);
        assert_eq!(decoded_palette[1], [178, 202, 226]);
        assert_eq!((orange.samples, orange.components), (200, 1));
        assert_eq!((purple.samples, purple.components), (200, 1));
    }
}
