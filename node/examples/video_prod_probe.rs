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

use allmystuff_node::node_control::NodeClient;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

const IPC_HEADER_LEN: usize = 28;
const RAW_KIND: u8 = 3;

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
    mode: Option<String>,
    dump_rgba: Option<PathBuf>,
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
            mode: None,
            dump_rgba: None,
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
    rgba_bytes: u64,
    first_ts: Option<u64>,
    last_ts: Option<u64>,
    timestamp_regressions: u64,
    dimensions: BTreeSet<(u32, u32)>,
    fingerprints: BTreeSet<u64>,
    max_green_ratio: f64,
    max_black_row_ratio: f64,
    dumped: bool,
}

impl FrameStats {
    fn observe(&mut self, packet: &[u8], dump_rgba: Option<&PathBuf>) -> Result<()> {
        if packet.len() < IPC_HEADER_LEN {
            bail!("video packet is only {} bytes", packet.len());
        }
        let kind = packet[0];
        if kind != RAW_KIND {
            bail!(
                "received video kind {kind}, not native-decoded RGBA (kind 3); H.264/HEVC was not decoded by the backend"
            );
        }

        let width = le_u32(&packet[4..8]);
        let height = le_u32(&packet[8..12]);
        let ts = le_u64(&packet[20..28]);
        if width == 0 || height == 0 || width > 16_384 || height > 16_384 {
            bail!("implausible decoded dimensions {width}x{height}");
        }
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4))
            .context("decoded frame size overflow")?;
        let rgba = &packet[IPC_HEADER_LEN..];
        if rgba.len() != expected {
            bail!(
                "decoded {width}x{height} frame has {} RGBA bytes, expected {expected}",
                rgba.len()
            );
        }

        if let Some(last) = self.last_ts {
            if ts < last {
                self.timestamp_regressions += 1;
            }
        }
        self.first_ts.get_or_insert(ts);
        self.last_ts = Some(ts);
        self.frames += 1;
        self.rgba_bytes += rgba.len() as u64;
        self.dimensions.insert((width, height));

        let pixels = width as usize * height as usize;
        let stride = (pixels / 4096).max(1);
        let mut sampled = 0u64;
        let mut green = 0u64;
        let mut fingerprint = 0xcbf29ce484222325u64;
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
        self.fingerprints.insert(fingerprint);

        // This is intentionally diagnostic-only: a dark desktop or a video
        // with letterboxing can contain valid black rows.  A high ratio is
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

        if !self.dumped {
            if let Some(path) = dump_rgba {
                std::fs::write(path, rgba)
                    .with_context(|| format!("write RGBA dump {}", path.display()))?;
                let metadata = path.with_extension("rgba.json");
                std::fs::write(
                    &metadata,
                    serde_json::to_vec_pretty(&json!({
                        "width": width,
                        "height": height,
                        "format": "rgba8",
                        "timestamp_us": ts,
                        "bytes": rgba.len(),
                    }))?,
                )
                .with_context(|| format!("write RGBA metadata {}", metadata.display()))?;
                println!("saved first decoded frame to {}", path.display());
                self.dumped = true;
            }
        }
        Ok(())
    }

    fn summary(&self, elapsed: Duration) -> Value {
        json!({
            "frames": self.frames,
            "fps": self.frames as f64 / elapsed.as_secs_f64().max(0.001),
            "rgba_bytes": self.rgba_bytes,
            "dimensions": self.dimensions.iter().map(|(w, h)| format!("{w}x{h}")).collect::<Vec<_>>(),
            "first_timestamp_us": self.first_ts,
            "last_timestamp_us": self.last_ts,
            "timestamp_regressions": self.timestamp_regressions,
            "sampled_unique_frames": self.fingerprints.len(),
            "max_green_dominant_sample_ratio": self.max_green_ratio,
            "max_nearly_black_row_ratio": self.max_black_row_ratio,
        })
    }
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
    let daemon = daemon_view(&client).await?;
    let local_sinks = endpoints_from_values(
        scan.get("capabilities")
            .and_then(Value::as_array)
            .into_iter()
            .flatten(),
        "display",
        "sink",
    );
    let mut remote_sources = remote_sources_from_snapshot(&snapshot);
    for source in daemon.remote_sources {
        remote_sources.entry(source.id.clone()).or_insert(source);
    }

    if config.list {
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

    let source = choose_source(&config, &local_id, &remote_sources)?;
    let sink = choose_sink(&config, &local_sinks)?;
    let remote_id = canonical_node(&source.node);
    if remote_id == canonical_node(&local_id) {
        bail!(
            "refusing same-node display source {}: a display self-route neither captures nor crosses ICE, so it cannot be an end-to-end transport test",
            source.id
        );
    }

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
    println!(
        "production probe: {} -> {} ({} cycle(s), native backend decode)",
        source.id, sink.id, config.cycles
    );
    println!(
        "selected ICE path evidence: {}",
        serde_json::to_string_pretty(&paths)?
    );

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
        let route = client
            .request(
                "connect_route",
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
            .as_str()
            .context("connect_route did not return a route id")?
            .to_string();
        if route != expected_route {
            bail!("connect_route returned unexpected id {route} (expected {expected_route})");
        }

        let mut token = client
            .request("video_watch", json!({ "route_id": route, "decode": true }))
            .await
            .with_context(|| format!("cycle {cycle}: claim native video watch"))?
            .as_u64()
            .context("video_watch did not return a token")?;

        let exercise = exercise_cycle(
            &client,
            &config,
            cycle,
            &route,
            &mut token,
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
            .request("disconnect_route", json!({ "route_id": route }))
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
        "PASS: real remote display route crossed an authenticated selected ICE pair and produced structurally valid backend-decoded RGBA through every reopen/rewatch gate"
    );
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "source": source.display_value(),
            "sink": sink.display_value(),
            "cycles": cycle_summaries,
            "ice_paths": paths,
        }))?
    );
    Ok(())
}

async fn exercise_cycle(
    client: &NodeClient,
    config: &Config,
    cycle: u32,
    route: &str,
    token: &mut u64,
    dump_rgba: Option<&PathBuf>,
) -> Result<Value> {
    wait_route_active(
        client,
        route,
        Duration::from_secs(config.active_timeout_secs),
    )
    .await?;

    if config.mode.is_some() || config.resize_edge.is_some() {
        client
            .request(
                "tune_route",
                json!({
                    "route_id": route,
                    "max_edge": config.resize_edge,
                    "bitrate": null,
                    "fps": null,
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
        Duration::from_secs(config.phase_secs),
        Duration::from_secs(config.first_frame_timeout_secs),
        dump_rgba,
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
            .request("video_watch", json!({ "route_id": route, "decode": true }))
            .await
            .context("recreate native decoder/watch mid-stream")?
            .as_u64()
            .context("replacement video_watch did not return a token")?;
        // This is the same refresh request a native decoder emits when its
        // first post-rewatch AU is a delta.  It remains on the existing route's
        // ICE data channel; no media bytes are placed on signaling.
        let _ = client
            .request("video_refresh", json!({ "route_id": route }))
            .await;
        let resumed = collect_frames(
            client,
            route,
            Duration::from_secs(config.phase_secs),
            Duration::from_secs(config.first_frame_timeout_secs),
            None,
        )
        .await
        .with_context(|| format!("cycle {cycle}: decoder rewatch/resume"))?;
        phases.push(json!({ "name": "native_rewatch", "stats": resumed }));
    }
    Ok(json!({ "route": route, "phases": phases }))
}

async fn collect_frames(
    client: &NodeClient,
    route: &str,
    duration: Duration,
    first_frame_timeout: Duration,
    dump_rgba: Option<&PathBuf>,
) -> Result<Value> {
    let started = Instant::now();
    let first_deadline = started + first_frame_timeout;
    let finish_after_first = duration;
    let mut first_at = None;
    let mut stats = FrameStats::default();
    loop {
        let batch = client
            .request_bytes("video_poll", json!({ "route_id": route }))
            .await
            .context("poll video IPC queue")?;
        for packet in split_batch(&batch)? {
            stats.observe(packet, dump_rgba)?;
            first_at.get_or_insert_with(Instant::now);
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
        tokio::time::sleep(Duration::from_millis(16)).await;
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
    Ok(stats.summary(first_at.unwrap_or(started).elapsed()))
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
            "--resize-edge" => {
                config.resize_edge = Some(next_arg(&mut args, "--resize-edge")?.parse()?);
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
            "--allow-no-ice-proof" => config.allow_no_ice_proof = true,
            other => bail!("unknown argument {other}; run --help"),
        }
    }
    if config.phase_secs == 0 || config.cycles == 0 {
        bail!("--seconds and --cycles must be greater than zero");
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
  --source ID               Exact advertised remote screen capability
  --peer ID                 Auto-pick the primary screen from this remote node
  --sink ID                 Local display sink (default: default local display)
  --seconds N               Seconds collected per phase (default: 5)
  --cycles N                Full disconnect/reopen cycles (default: 2)
  --no-rewatch              Skip native decoder teardown/recreate mid-stream
  --resize-edge N           Tune the live route to this maximum edge
  --mode MODE               balanced|game|studio|studio-lossless
  --active-timeout N        Route/ICE timeout seconds (default: 20)
  --first-frame-timeout N   Decode startup timeout seconds (default: 12)
  --dump-rgba PATH          Save the first raw RGBA frame + PATH.rgba.json
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
    fn raw_frame_validator_checks_geometry_and_alpha() {
        let mut packet = vec![0; IPC_HEADER_LEN + 2 * 2 * 4];
        packet[0] = RAW_KIND;
        packet[4..8].copy_from_slice(&2u32.to_le_bytes());
        packet[8..12].copy_from_slice(&2u32.to_le_bytes());
        for pixel in packet[IPC_HEADER_LEN..].chunks_exact_mut(4) {
            pixel.copy_from_slice(&[10, 20, 30, 255]);
        }
        let mut stats = FrameStats::default();
        stats.observe(&packet, None).unwrap();
        assert_eq!(stats.frames, 1);
        packet[IPC_HEADER_LEN + 3] = 0;
        assert!(stats.observe(&packet, None).is_err());
    }
}
