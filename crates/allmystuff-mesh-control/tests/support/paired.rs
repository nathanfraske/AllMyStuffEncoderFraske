//! Shared custody, source vectors and observations for the two opt-in probes.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use allmystuff_mesh_control::contract::verify_wire_status;
use allmystuff_mesh_control::realtime::{
    Encoding, FlowDirection, FlowSpec, RealtimePolicy, RealtimeSession, RtpKind,
};
use allmystuff_mesh_control::transport::round_trip;
use allmystuff_mesh_control::{
    ControlClient, Endpoint, EventContract, Response, StatusProbe, STATUS_ACK_LIMIT,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::BufReader;
use tokio::sync::mpsc;

pub fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("paired harness must supply {name}"))
}

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn number(value: &Value, key: &str) -> u64 {
    value
        .get(key)
        .and_then(Value::as_u64)
        .expect("explicit finite TEST policy field")
}

pub fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value
        .get(key)
        .and_then(Value::as_str)
        .expect("explicit TEST text field")
}

pub struct Unit {
    pub data: Vec<u8>,
    pub duration_us: u32,
    pub ts_us: u64,
    pub sha256: String,
}

pub struct Context {
    pub root: PathBuf,
    pub owner: String,
    pub network: String,
    pub mode: String,
    pub policy: Value,
    pub video: Vec<Unit>,
    pub audio: Vec<Unit>,
    pub a: Arc<ControlClient>,
    pub b: Arc<ControlClient>,
}

pub struct SessionOwner {
    pub session: RealtimeSession,
    events: mpsc::Receiver<Value>,
}

impl SessionOwner {
    pub async fn close(self) -> allmystuff_mesh_control::realtime::Result<()> {
        let result = self.session.close().await;
        drop(self.events);
        result
    }
}

impl Context {
    pub fn load() -> Self {
        let root = PathBuf::from(required("AMS_MESH_PAIRED_ROOT"));
        let owner = required("AMS_MESH_PAIRED_OWNER");
        assert!(
            root.is_absolute() && root.is_dir(),
            "owned paired root required"
        );
        let custody: Value =
            serde_json::from_slice(&std::fs::read(root.join("custody.json")).unwrap()).unwrap();
        assert!(custody.get("root").and_then(Value::as_str) == root.to_str());
        assert!(custody.get("owner").and_then(Value::as_str) == Some(owner.as_str()));
        let endpoint = |side: &str| {
            let name = required(&format!("AMS_MESH_PAIRED_ENDPOINT_{side}"));
            #[cfg(unix)]
            {
                let path = PathBuf::from(name);
                assert_eq!(
                    path,
                    root.join(format!("state-{}", side.to_ascii_lowercase()))
                        .join("s")
                );
                Endpoint::Path(path)
            }
            #[cfg(not(unix))]
            {
                assert_eq!(
                    name,
                    format!("ams-mp-{owner}-{}", side.to_ascii_lowercase())
                );
                Endpoint::Name(name)
            }
        };
        let a = Arc::new(ControlClient::new(endpoint("A")));
        let b = Arc::new(ControlClient::new(endpoint("B")));
        let network = required("AMS_MESH_PAIRED_NETWORK");
        assert_eq!(network, format!("ams-mp-{owner}"));
        let mode = required("AMS_MESH_PAIRED_MODE");
        assert!(matches!(mode.as_str(), "direct" | "relay-lab"));
        let policy: Value = serde_json::from_str(&required("AMS_MESH_PAIRED_POLICY")).unwrap();
        let manifest_path = PathBuf::from(required("AMS_MESH_PAIRED_VECTORS"));
        assert_eq!(manifest_path, root.join("vectors/vectors.json"));
        let manifest: Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        assert_eq!(number(&manifest, "version"), 1);
        let read_units = |kind: &str, count: usize| {
            let entries = manifest.get(kind).and_then(Value::as_array).unwrap();
            assert_eq!(entries.len(), count);
            entries
                .iter()
                .map(|entry| {
                    let leaf = text(entry, "file");
                    assert!(
                        PathBuf::from(leaf)
                            .file_name()
                            .and_then(|name| name.to_str())
                            == Some(leaf)
                    );
                    let path = root.join("vectors").join(leaf);
                    assert!(!path.symlink_metadata().unwrap().file_type().is_symlink());
                    let data = std::fs::read(path).unwrap();
                    assert_eq!(u64::try_from(data.len()).unwrap(), number(entry, "bytes"));
                    let sha256 = digest(&data);
                    assert_eq!(sha256, text(entry, "sha256"));
                    Unit {
                        data,
                        duration_us: u32::try_from(number(entry, "duration_us")).unwrap(),
                        ts_us: number(entry, "ts_us"),
                        sha256,
                    }
                })
                .collect()
        };
        let video = read_units("video", 3);
        let audio = read_units("audio", 6);
        Self {
            root,
            owner,
            network,
            mode,
            policy,
            video,
            audio,
            a,
            b,
        }
    }

    pub fn timeout(&self) -> Duration {
        Duration::from_secs(number(&self.policy, "test_timeout_secs"))
    }

    pub fn realtime_policy(&self) -> RealtimePolicy {
        let policy = &self.policy["client_policy"];
        RealtimePolicy {
            max_flows: usize::try_from(number(policy, "max_flows")).unwrap(),
            max_inbound_pipes: usize::try_from(number(policy, "max_inbound_pipes")).unwrap(),
            max_frame_bytes: usize::try_from(number(policy, "max_frame_bytes")).unwrap(),
            max_retained_bytes: usize::try_from(number(policy, "max_retained_bytes")).unwrap(),
            operation_timeout: Duration::from_millis(number(policy, "operation_timeout_ms")),
            io_timeout: Duration::from_millis(number(policy, "io_timeout_ms")),
        }
    }

    pub fn flow(&self, peer: &str, label: &str, direction: FlowDirection, video: bool) -> FlowSpec {
        FlowSpec {
            network: self.network.clone(),
            peer: peer.to_owned(),
            label: label.to_owned(),
            direction,
            encoding: encoding(video),
        }
    }

    pub async fn session(&self, client: &Arc<ControlClient>) -> SessionOwner {
        let (tx, events) =
            mpsc::channel(usize::try_from(number(&self.policy, "event_capacity")).unwrap());
        // Keep the unused ordinary-event receiver alive for the session. The
        // selected candidate adapter forwards only channel_inbound; this pair
        // registers no ordinary channel and carries units on binary pipes.
        // Session owns its EventSession; receiver custody travels with it in
        // the returned wrapper. It must not be dropped at this helper's return.
        let event = client
            .subscribe_events_for_contract(EventContract::CandidateV1Db7818e, tx)
            .await
            .unwrap();
        SessionOwner {
            session: RealtimeSession::new(Arc::clone(client), event, self.realtime_policy())
                .unwrap(),
            events,
        }
    }

    pub fn restart_marker(&self) {
        use std::io::Write;
        let writing = self.root.join("restart-ready-writing");
        let mut marker = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&writing)
            .unwrap();
        marker.write_all(self.owner.as_bytes()).unwrap();
        marker.flush().unwrap();
        drop(marker);
        std::fs::rename(writing, self.root.join("restart-ready")).unwrap();
    }
}

pub fn encoding(video: bool) -> Encoding {
    if video {
        Encoding {
            rtp_kind: RtpKind::Video,
            mime: "video/H264".to_owned(),
            clock_rate: 90000,
            channels: 0,
        }
    } else {
        Encoding {
            rtp_kind: RtpKind::Audio,
            mime: "audio/opus".to_owned(),
            clock_rate: 48000,
            channels: 2,
        }
    }
}

pub async fn raw(client: &ControlClient, request: Value, timeout: Duration) -> Response {
    let socket = client
        .connect()
        .await
        .expect("connect only owned paired endpoint");
    let (reader, mut writer) = tokio::io::split(socket);
    let mut reader = BufReader::new(reader);
    let line = serde_json::to_string(&request).unwrap() + "\n";
    round_trip(
        &mut reader,
        &mut writer,
        &line,
        timeout,
        Some(STATUS_ACK_LIMIT),
    )
    .await
    .expect("private setup response, remote payload omitted")
}

pub async fn ready(client: &ControlClient, network: &str) -> String {
    loop {
        match client.probe_status().await {
            StatusProbe::Answered(response) => {
                let evidence = verify_wire_status(&response, EventContract::CandidateV1Db7818e)
                    .expect("selected candidate Status");
                assert_eq!(evidence.joined_networks, vec![network.to_owned()]);
                let advert = evidence
                    .realtime
                    .expect("actual registered realtime advert");
                assert!(advert.supported);
                for video in [true, false] {
                    let family = encoding(video);
                    let kind = if video { "video" } else { "audio" };
                    assert!(advert.encodings.iter().any(|item| item.kind == kind
                        && item.mime.eq_ignore_ascii_case(&family.mime)
                        && item.clock_rate == family.clock_rate
                        && item.channels == family.channels));
                }
                let identity = raw(
                    client,
                    json!({"op":"identity_show"}),
                    Duration::from_secs(5),
                )
                .await;
                assert!(identity.ok);
                let public_id = identity
                    .data
                    .as_ref()
                    .and_then(|data| data.get("pubkey"))
                    .and_then(Value::as_str)
                    .expect("public endpoint identity");
                assert_eq!(public_id.len(), 52, "source public_id is base32 Ed25519");
                assert!(public_id
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || (b'2'..=b'7').contains(&byte)));
                return public_id.to_owned();
            }
            StatusProbe::Absent | StatusProbe::Unavailable(_) => {
                tokio::time::sleep(Duration::from_millis(50)).await
            }
        }
    }
}

pub async fn connect(context: &Context, peer: &str) {
    let timeout = context.realtime_policy().operation_timeout;
    let wait_ms = u64::try_from(timeout.as_millis())
        .unwrap()
        .checked_sub(1000)
        .unwrap();
    let response = raw(
        &context.a,
        json!({"op":"network_connect_peer", "network":context.network,
                       "peer":peer, "pin":false, "wait_ms":wait_ms}),
        timeout,
    )
    .await;
    assert!(
        response.ok
            && response
                .data
                .as_ref()
                .and_then(|data| data.get("active"))
                .and_then(Value::as_bool)
                == Some(true),
        "one explicit private dial must reach ACTIVE; no retry on an ambiguous write"
    );
}

pub async fn endpoint_ready(context: &Context) {
    let identity = ready(&context.a, &context.network).await;
    let expected = format!("127.0.0.1:{}", required("AMS_MESH_PAIRED_SIGNALING_PORT"));
    loop {
        let response = raw(
            &context.a,
            json!({"op":"services_status"}),
            Duration::from_secs(5),
        )
        .await;
        assert!(response.ok, "owned service observation must answer");
        let status = response
            .data
            .as_ref()
            .and_then(|data| data.get("status"))
            .unwrap();
        if status["signaling"]["running"] == Value::Bool(true) {
            assert_eq!(
                status["signaling"]["listen"].as_str(),
                Some(expected.as_str())
            );
            assert_eq!(status["stun"]["running"], Value::Bool(false));
            if context.mode == "relay-lab" {
                if status["turn"]["running"] != Value::Bool(true) {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
                let turn = format!("127.0.0.1:{}", required("AMS_MESH_PAIRED_TURN_PORT"));
                assert_eq!(status["turn"]["listen"].as_str(), Some(turn.as_str()));
                assert!(status["turn"]["tcp_listen"].is_null());
                assert!(status["turn"]["tls_proxy_listen"].is_null());
            } else {
                assert_eq!(status["turn"]["running"], Value::Bool(false));
            }
            emit(json!({"scenario":"endpoint-ready", "public_id":identity,
                        "private_signaling_listen":expected, "running":true,
                        "mode":context.mode, "owned_turn_running":context.mode == "relay-lab"}));
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Require the daemon's actual selected pair on both endpoints. Available
/// candidate counts or a relay configuration alone do not establish the path.
pub async fn selected_paths(context: &Context, a_id: &str, b_id: &str) -> Value {
    async fn selected(context: &Context, client: &ControlClient, peer_id: &str) -> Value {
        tokio::time::timeout(context.realtime_policy().operation_timeout, async {
            loop {
                let response = raw(
                    client,
                    json!({"op":"peers_list", "network":context.network}),
                    Duration::from_secs(5),
                )
                .await;
                assert!(response.ok, "private selected-pair observation must answer");
                let peers = response
                    .data
                    .as_ref()
                    .and_then(|data| data.get("peers"))
                    .and_then(Value::as_array)
                    .unwrap();
                if let Some(peer) = peers.iter().find(|peer| {
                    peer.get("device_id")
                        .and_then(Value::as_str)
                        .is_some_and(|id| {
                            id == peer_id
                                || id
                                    .strip_prefix(peer_id)
                                    .is_some_and(|tail| tail.starts_with('-'))
                        })
                }) {
                    if let Some(pair) = peer.get("selected_pair").filter(|pair| pair.is_object()) {
                        let expected = if context.mode == "relay-lab" {
                            "relay"
                        } else {
                            "host"
                        };
                        assert_eq!(
                            pair.get("local").and_then(Value::as_str),
                            Some(expected),
                            "actual local selected ICE kind"
                        );
                        assert_eq!(
                            pair.get("remote").and_then(Value::as_str),
                            Some(expected),
                            "actual remote selected ICE kind"
                        );
                        return json!({"peer":peer_id, "selected_pair":pair});
                    }
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("finite actual selected ICE pair observation")
    }
    let a = selected(context, &context.a, b_id).await;
    let b = selected(context, &context.b, a_id).await;
    json!({"mode":context.mode,"a":a,"b":b})
}

pub fn nal_payloads(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut starts = Vec::new();
    let mut offset = 0;
    while offset + 3 <= bytes.len() {
        let size = if bytes[offset..].starts_with(&[0, 0, 0, 1]) {
            4
        } else if bytes[offset..].starts_with(&[0, 0, 1]) {
            3
        } else {
            offset += 1;
            continue;
        };
        starts.push((offset, offset + size));
        offset += size;
    }
    assert!(!starts.is_empty(), "source-generated H264 Annex-B expected");
    starts
        .iter()
        .enumerate()
        .map(|(index, &(_, start))| {
            let end = starts
                .get(index + 1)
                .map_or(bytes.len(), |&(start, _)| start);
            assert!(start < end);
            bytes[start..end].to_vec()
        })
        .collect()
}

pub fn check_received(expected: &Unit, received: &[u8], marker: bool, video: bool) {
    if video {
        let source = expected_h264_payloads(&expected.data);
        assert_eq!(
            nal_payloads(received),
            source,
            "only canonical startcodes and pinned payloader AUD9/filler12 removal are permitted"
        );
        assert_eq!(
            marker,
            nal_payloads(received).iter().any(|nal| nal[0] & 0x1f == 5),
            "H264 assembled marker is IDR entry point"
        );
    } else {
        assert_eq!(
            received, expected.data,
            "whole Opus packet bytes must survive"
        );
    }
}

/// Check this small source fixture fits the pinned payloader's supported
/// SPS/PPS case. Never hide lost, reordered or deduplicated parameter sets or
/// SEI behind a general-purpose normalization rule.
pub fn expected_h264_payloads(bytes: &[u8]) -> Vec<Vec<u8>> {
    let nals: Vec<_> = nal_payloads(bytes)
        .into_iter()
        .filter(|nal| !matches!(nal[0] & 0x1f, 9 | 12))
        .collect();
    let mut index = 0;
    while index < nals.len() {
        match nals[index][0] & 0x1f {
            7 => {
                assert!(
                    index + 2 < nals.len(),
                    "complete SPS/PPS pair must precede a nonparameter NAL"
                );
                assert_eq!(
                    nals[index + 1][0] & 0x1f,
                    8,
                    "fixture uses one ordered SPS/PPS pair"
                );
                assert!(!matches!(nals[index + 2][0] & 0x1f, 7 | 8));
                // Exact vendor RTP_OUTBOUND_MTU=1200, packetizer subtracts its
                // twelve-byte RTP header before calling rtp0.13 H264 payload.
                assert!(
                    1 + 2 + nals[index].len() + 2 + nals[index + 1].len() <= 1188,
                    "SPS/PPS STAP-A must fit the actual pinned payload MTU"
                );
                index += 2;
            }
            8 => panic!("unpaired PPS is outside this exact fixture's supported transform"),
            _ => index += 1,
        }
    }
    nals
}

pub fn observation(
    expected: &Unit,
    data: &[u8],
    timestamp: u32,
    marker: bool,
    label: &str,
) -> Value {
    json!({"label":label, "source_ts_us_not_wire_timestamp":expected.ts_us,
           "source_duration_us":expected.duration_us, "source_sha256":expected.sha256,
           "received_sha256":digest(data), "received_bytes":data.len(),
           "rtp_timestamp":timestamp, "marker":marker})
}

pub fn check_delta(prior: Option<u32>, timestamp: u32, duration_us: u32, clock_rate: u32) {
    if let Some(previous) = prior {
        let ticks = (f64::from(duration_us) / 1_000_000.0 * f64::from(clock_rate)) as u32;
        assert_eq!(
            timestamp.wrapping_sub(previous),
            ticks,
            "actual duration-based RTP delta, modulo u32"
        );
    }
}

pub fn emit(value: Value) {
    println!("MESH_CONTROL_PAIRED {value}");
}
