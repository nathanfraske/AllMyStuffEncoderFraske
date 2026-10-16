//! Public realtime adapter fixtures against immutable daemon wire literals.
//!
//! Oracle: MyOwnMeshSecurityReview db7818e09fedd98899490347b86ac9bc9f97b59b,
//! control/{wire,framing,reply,dispatch/realtime}.rs. These fake endpoints
//! characterize local ownership and framing; they do not prove peer delivery,
//! native resource retirement, codec negotiation or application authorization.

#![cfg(feature = "realtime")]

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use allmystuff_mesh_control::realtime::{
    Encoding, FlowDirection, FlowSpec, PipeEnd, RealtimeError,
    RealtimePolicy, RealtimeSession, RtpKind,
};
use allmystuff_mesh_control::{ClientId, ControlClient, Endpoint, EventContract, EventSession};
use interprocess::local_socket::tokio::{prelude::*, Listener};
#[cfg(unix)]
use interprocess::local_socket::GenericFilePath;
#[cfg(not(unix))]
use interprocess::local_socket::GenericNamespaced;
use interprocess::local_socket::ListenerOptions;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

const DEADLINE: Duration = Duration::from_secs(10);
const CLIENT_SECRET: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const FLOW_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const FLOW_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const NETWORK: &str = "fixture-network";
const PEER: &str = "fixture-peer";

async fn bounded<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(DEADLINE, future)
        .await
        .expect("private realtime fixture exceeded its finite deadline")
}

fn success<T>(result: allmystuff_mesh_control::realtime::Result<T>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("unexpected realtime result: {error:?}"),
    }
}

fn failure<T>(result: allmystuff_mesh_control::realtime::Result<T>) -> RealtimeError {
    match result {
        Err(error) => error,
        Ok(_) => panic!("realtime operation unexpectedly succeeded"),
    }
}

fn policy() -> RealtimePolicy {
    RealtimePolicy {
        max_flows: 4,
        max_inbound_pipes: 2,
        max_frame_bytes: 64,
        max_retained_bytes: 256,
        operation_timeout: Duration::from_secs(2),
        io_timeout: Duration::from_secs(2),
    }
}

fn video(label: &str) -> FlowSpec {
    FlowSpec {
        network: NETWORK.into(),
        peer: PEER.into(),
        label: label.into(),
        direction: FlowDirection::Outbound,
        encoding: Encoding {
            rtp_kind: RtpKind::Video,
            mime: "video/H264".into(),
            clock_rate: 90_000,
            channels: 0,
        },
    }
}

fn audio(label: &str) -> FlowSpec {
    FlowSpec {
        network: NETWORK.into(),
        peer: PEER.into(),
        label: label.into(),
        direction: FlowDirection::Outbound,
        encoding: Encoding {
            rtp_kind: RtpKind::Audio,
            mime: "audio/opus".into(),
            clock_rate: 48_000,
            // RTP registration uses /48000/2 even for mono Opus packets.
            channels: 2,
        },
    }
}

struct PrivateEndpoint {
    #[cfg(unix)]
    directory: std::path::PathBuf,
    #[cfg(unix)]
    path: std::path::PathBuf,
    #[cfg(not(unix))]
    name: String,
}

impl PrivateEndpoint {
    fn new() -> Self {
        let mut nonce = [0_u8; 8];
        getrandom::getrandom(&mut nonce).expect("private endpoint nonce");
        let label = format!("amr{:x}", u64::from_le_bytes(nonce));
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            use std::os::unix::fs::DirBuilderExt;

            let directory = std::env::temp_dir().join(label);
            let path = directory.join("s");
            assert!(
                path.as_os_str().as_bytes().len() < 104,
                "use a private short TMPDIR for native Unix fixtures"
            );
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&directory)
                .expect("exclusive private endpoint directory");
            Self { directory, path }
        }
        #[cfg(not(unix))]
        {
            Self { name: label }
        }
    }

    fn client(&self) -> Arc<ControlClient> {
        #[cfg(unix)]
        let endpoint = Endpoint::Path(self.path.clone());
        #[cfg(not(unix))]
        let endpoint = Endpoint::Name(self.name.clone());
        Arc::new(ControlClient::new(endpoint))
    }

    fn listen(&self) -> Listener {
        #[cfg(unix)]
        let name = self.path.as_path().to_fs_name::<GenericFilePath>().unwrap();
        #[cfg(not(unix))]
        let name = self.name.as_str().to_ns_name::<GenericNamespaced>().unwrap();
        let listener = ListenerOptions::new().name(name).create_tokio().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))
                .unwrap();
        }
        listener
    }
}

impl Drop for PrivateEndpoint {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            // Never recurse or remove a shared parent. All tasks/sockets in
            // this fixture belong to its private endpoint and Tokio runtime.
            let _ = std::fs::remove_file(&self.path);
            let _ = std::fs::remove_dir(&self.directory);
        }
    }
}

struct DaemonPeer {
    reader: BufReader<LocalSocketStream>,
}

impl DaemonPeer {
    async fn accept(listener: &Listener) -> Self {
        Self {
            reader: BufReader::new(bounded(listener.accept()).await.unwrap()),
        }
    }

    async fn verified(listener: &Listener) -> Self {
        let mut peer = Self::accept(listener).await;
        assert_eq!(peer.request().await, json!({"op":"status"}));
        // Exact candidate Status fields precede each protected operation on
        // that same connection. Events registration itself remains Events-only.
        peer.reply(json!({"ok":true,"data":{
            "version":"1.0.0","device_id":"fixture-device",
            "joined_networks":[NETWORK],"realtime":{"supported":true,"encodings":[
                {"kind":"video","mime":"video/H264","clock_rate":90000,"channels":0},
                {"kind":"audio","mime":"audio/opus","clock_rate":48000,"channels":2}
            ]}
        }})).await;
        peer
    }

    async fn request(&mut self) -> Value {
        let line = bounded(allmystuff_mesh_control::transport::read_json_line(
            &mut self.reader,
            Some(16 * 1024),
        ))
        .await
        .expect("private command must be bounded JSONL");
        serde_json::from_slice(&line).expect("private command JSON")
    }

    async fn bytes(&mut self, bytes: &[u8]) {
        bounded(self.reader.get_mut().write_all(bytes)).await.unwrap();
        bounded(self.reader.get_mut().flush()).await.unwrap();
    }

    async fn reply(&mut self, value: Value) {
        let mut line = serde_json::to_vec(&value).unwrap();
        line.push(b'\n');
        self.bytes(&line).await;
    }

    async fn frame(&mut self) -> Vec<u8> {
        let mut prefix = [0_u8; 4];
        bounded(self.reader.read_exact(&mut prefix)).await.unwrap();
        let length = u32::from_le_bytes(prefix) as usize;
        assert!(length <= 4096, "unexpected private frame size");
        let mut framed = prefix.to_vec();
        framed.resize(length + 4, 0);
        bounded(self.reader.read_exact(&mut framed[4..])).await.unwrap();
        framed
    }

    async fn eof(&mut self) {
        let mut byte = [0_u8; 1];
        assert_eq!(
            bounded(self.reader.read(&mut byte)).await.unwrap(),
            0,
            "owned connection must end without an extra request/frame"
        );
    }
}

async fn event_registration(
    listener: &Listener,
    client: &Arc<ControlClient>,
    id: ClientId,
    secret: &str,
) -> (EventSession, DaemonPeer, mpsc::Receiver<Value>) {
    let (sender, receiver) = mpsc::channel(8);
    let subscribing = client.subscribe_events_for_contract(EventContract::CandidateV1Db7818e, sender);
    let answering = async {
        let mut peer = DaemonPeer::accept(listener).await;
        assert_eq!(peer.request().await, json!({"op":"events_subscribe"}));
        peer.reply(json!({"ok":true,"data":{
            "subscribed":true,"client_id":id.to_string(),"client_capability":secret
        }}))
        .await;
        peer
    };
    let (session, peer) = tokio::join!(bounded(subscribing), bounded(answering));
    (session.unwrap(), peer, receiver)
}

struct Fixture {
    session: Option<RealtimeSession>,
    events: DaemonPeer,
    _event_receiver: mpsc::Receiver<Value>,
    listener: Listener,
    client: Arc<ControlClient>,
    // Last field: private paths outlive listeners, sessions and peer sockets.
    _endpoint: PrivateEndpoint,
}

impl Fixture {
    async fn new(policy: RealtimePolicy) -> Self {
        let endpoint = PrivateEndpoint::new();
        let listener = endpoint.listen();
        let client = endpoint.client();
        let (events, peer, receiver) =
            event_registration(&listener, &client, ClientId(7), CLIENT_SECRET).await;
        let session = success(RealtimeSession::new(Arc::clone(&client), events, policy));
        Self {
            session: Some(session),
            events: peer,
            _event_receiver: receiver,
            listener,
            client,
            _endpoint: endpoint,
        }
    }

    async fn finish(&mut self) {
        success(bounded(self.session.take().unwrap().close()).await);
        // This observes our local socket release, not upstream native drain.
        self.events.eof().await;
    }
}

fn open_request(label: &str, kind: &str, mime: &str, rate: u32, channels: u16) -> Value {
    json!({
        "op":"realtime_flow_open", "network":NETWORK, "peer":PEER,
        "flow_label":label, "client_id":ClientId(7).to_string(),
        "client_capability":CLIENT_SECRET, "direction":"outbound",
        "rtp_kind":kind, "mime":mime, "clock_rate":rate, "channels":channels
    })
}

fn outbound_request(capability: &str) -> Value {
    json!({
        "op":"realtime_pipe", "direction":"outbound", "network":NETWORK,
        "client_id":ClientId(7).to_string(), "client_capability":CLIENT_SECRET,
        "flow_capability":capability
    })
}

fn close_request(capability: &str) -> Value {
    json!({
        "op":"realtime_flow_close", "client_id":ClientId(7).to_string(),
        "client_capability":CLIENT_SECRET, "flow_capability":capability
    })
}

async fn opened(
    listener: &Listener,
    request: Value,
    label: &str,
    capability: &str,
) -> (DaemonPeer, DaemonPeer) {
    let mut command = DaemonPeer::verified(listener).await;
    assert_eq!(command.request().await, request);
    command
        .reply(json!({"ok":true,"data":{"flow_label":label,"flow_capability":capability}}))
        .await;
    let mut pipe = DaemonPeer::verified(listener).await;
    assert_eq!(pipe.request().await, outbound_request(capability));
    pipe.reply(json!({"ok":true,"data":{"realtime_pipe":true}})).await;
    (command, pipe)
}

async fn closed(listener: &Listener, capability: &str) -> DaemonPeer {
    let mut command = DaemonPeer::verified(listener).await;
    assert_eq!(command.request().await, close_request(capability));
    command.reply(json!({"ok":true,"data":{"closed":true}})).await;
    command
}

async fn inbound_opened(listener: &Listener, peer_name: &str) -> DaemonPeer {
    let mut pipe = DaemonPeer::verified(listener).await;
    assert_eq!(
        pipe.request().await,
        json!({
            "op":"realtime_pipe", "direction":"inbound", "network":NETWORK,
            "peer":peer_name, "client_id":ClientId(7).to_string(),
            "client_capability":CLIENT_SECRET
        })
    );
    pipe.reply(json!({"ok":true,"data":{"realtime_pipe":true}})).await;
    pipe
}

#[tokio::test]
async fn h264_and_opus_keep_literal_open_pipe_and_unit_fields() {
    let mut fixture = Fixture::new(policy()).await;
    let session = fixture.session.as_mut().unwrap();
    let generation = session.generation();
    let (flow, peers) = tokio::join!(
        bounded(session.open_flow(video("v"))),
        bounded(opened(&fixture.listener, open_request("v", "video", "video/H264", 90_000, 0), "v", FLOW_A))
    );
    let mut video_flow = success(flow);
    let (mut video_command, mut video_pipe) = peers;
    video_command.eof().await;
    assert_eq!(video_flow.generation(), generation);
    assert_eq!(video_flow.spec().label, "v");
    let (flow, peers) = tokio::join!(
        bounded(session.open_flow(audio("a"))),
        bounded(opened(&fixture.listener, open_request("a", "audio", "audio/opus", 48_000, 2), "a", FLOW_B))
    );
    let mut audio_flow = success(flow);
    let (mut audio_command, mut audio_pipe) = peers;
    audio_command.eof().await;
    // These complete byte vectors come from upstream framing, not the new
    // adapter's encoder. The reserved byte is zero; durations are microseconds.
    let (sent, bytes) = tokio::join!(
        bounded(video_flow.send(33_333, &[0, 0, 0, 1, 0x65, 0x88, 0x84])),
        bounded(video_pipe.frame())
    );
    success(sent);
    assert_eq!(bytes, [18, 0, 0, 0, 1, 0, 0x35, 0x82, 0, 0, 7, 0, 0, 0, b'v', 0, 0, 0, 1, 0x65, 0x88, 0x84]);
    let (sent, bytes) = tokio::join!(
        bounded(audio_flow.send(20_000, &[0xf8, 0xff, 0xfe])),
        bounded(audio_pipe.frame())
    );
    success(sent);
    assert_eq!(bytes, [14, 0, 0, 0, 1, 0, 0x20, 0x4e, 0, 0, 3, 0, 0, 0, b'a', 0xf8, 0xff, 0xfe]);
    for (flow, capability) in [(video_flow, FLOW_A), (audio_flow, FLOW_B)] {
        let (result, mut command) = tokio::join!(bounded(flow.close()), bounded(closed(&fixture.listener, capability)));
        success(result);
        command.eof().await;
    }
    video_pipe.eof().await;
    audio_pipe.eof().await;
    fixture.finish().await;
}

#[tokio::test]
async fn inbound_keeps_pipe_context_binary_label_marker_and_wrapping_rtp_timestamp() {
    let mut fixture = Fixture::new(policy()).await;
    let (pipe, mut remote) = tokio::join!(
        bounded(fixture.session.as_mut().unwrap().open_inbound(NETWORK, PEER)),
        bounded(inbound_opened(&fixture.listener, PEER))
    );
    let mut pipe = success(pipe);
    assert_eq!(pipe.network(), NETWORK);
    assert_eq!(pipe.peer(), PEER);
    assert_eq!(pipe.generation(), fixture.session.as_ref().unwrap().generation());
    // A label is raw bytes inbound. It is neither lossy UTF-8 nor authority.
    remote.bytes(&[15, 0, 0, 0, 2, 1, 0xf0, 0xff, 0xff, 0xff, 3, 0, 0, 0, 0xff, 0, 0xf8, 0xff, 0xfe]).await;
    let unit = success(bounded(pipe.recv()).await);
    assert_eq!(unit.label(), &[0xff, 0]);
    assert_eq!(unit.payload(), &[0xf8, 0xff, 0xfe]);
    assert!(unit.marker());
    assert_eq!(unit.rtp_timestamp(), 0xfffffff0);
    drop(unit);
    remote.bytes(&[12, 0, 0, 0, 1, 0, 0x10, 0, 0, 0, 1, 0, 0, 0, b'v', 0x41]).await;
    let unit = success(bounded(pipe.recv()).await);
    assert_eq!(unit.label(), b"v");
    assert_eq!(unit.payload(), &[0x41]);
    assert!(!unit.marker());
    assert_eq!(unit.rtp_timestamp(), 0x10);
    drop(unit);
    success(bounded(pipe.close()).await);
    remote.eof().await;
    fixture.finish().await;
}

#[tokio::test]
async fn local_send_size_refusal_preserves_the_healthy_frame_boundary() {
    let mut fixture = Fixture::new(policy()).await;
    let (flow, peers) = tokio::join!(
        bounded(fixture.session.as_mut().unwrap().open_flow(video("v"))),
        bounded(opened(&fixture.listener, open_request("v", "video", "video/H264", 90_000, 0), "v", FLOW_A))
    );
    let mut flow = success(flow);
    let (mut command, mut remote) = peers;
    command.eof().await;
    // 10-byte body header + 1-byte label + 54 payload bytes is one byte over.
    assert!(matches!(failure(bounded(flow.send(1, &[7; 54])).await), RealtimeError::LimitExceeded(_)));
    let (sent, bytes) = tokio::join!(bounded(flow.send(u32::MAX, &[7; 53])), bounded(remote.frame()));
    success(sent);
    let mut expected = vec![64, 0, 0, 0, 1, 0, 0xff, 0xff, 0xff, 0xff, 53, 0, 0, 0, b'v'];
    expected.extend_from_slice(&[7; 53]);
    assert_eq!(bytes, expected, "local refusal must not write even a partial prefix");
    let (result, mut command) = tokio::join!(bounded(flow.close()), bounded(closed(&fixture.listener, FLOW_A)));
    success(result);
    command.eof().await;
    remote.eof().await;
    fixture.finish().await;
}

#[tokio::test]
async fn retained_inbound_unit_holds_the_shared_frame_budget_until_drop() {
    let mut limits = policy();
    limits.max_retained_bytes = 68; // one maximum body plus its four-byte prefix
    let mut fixture = Fixture::new(limits).await;
    let (pipe, mut inbound_remote) = tokio::join!(
        bounded(fixture.session.as_mut().unwrap().open_inbound(NETWORK, PEER)),
        bounded(inbound_opened(&fixture.listener, PEER))
    );
    let mut inbound = success(pipe);
    let (flow, peers) = tokio::join!(
        bounded(fixture.session.as_mut().unwrap().open_flow(video("v"))),
        bounded(opened(&fixture.listener, open_request("v", "video", "video/H264", 90_000, 0), "v", FLOW_A))
    );
    let mut flow = success(flow);
    let (mut command, mut outbound_remote) = peers;
    command.eof().await;
    let mut frame = vec![64, 0, 0, 0, 1, 0, 1, 0, 0, 0, 53, 0, 0, 0, b'v'];
    frame.extend_from_slice(&[9; 53]);
    inbound_remote.bytes(&frame).await;
    let unit = success(bounded(inbound.recv()).await);
    assert_eq!(unit.payload(), &[9; 53]);
    assert!(matches!(failure(bounded(flow.send(1, &[3])).await), RealtimeError::LimitExceeded(_)));
    drop(unit);
    let (sent, frame) = tokio::join!(bounded(flow.send(1, &[3])), bounded(outbound_remote.frame()));
    success(sent);
    assert_eq!(frame, [12, 0, 0, 0, 1, 0, 1, 0, 0, 0, 1, 0, 0, 0, b'v', 3]);
    let (result, mut command) = tokio::join!(bounded(flow.close()), bounded(closed(&fixture.listener, FLOW_A)));
    success(result);
    command.eof().await;
    outbound_remote.eof().await;
    success(bounded(inbound.close()).await);
    inbound_remote.eof().await;
    fixture.finish().await;
}

#[tokio::test]
async fn invalid_inbound_lengths_and_marker_end_the_pipe_without_resynchronizing() {
    // All short vectors are complete outer frames. Their inner bodies each
    // violate one independent wire property, rather than failing at read EOF.
    let cases: &[&[u8]] = &[
        &[9, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0], // body shorter than ten bytes
        &[10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], // empty label
        &[11, 0, 0, 0, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, b'v'], // marker is not bool
        &[11, 0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, b'v'], // payload absent
        &[12, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, b'v', 9], // uncounted tail
    ];
    for frame in cases {
        let mut fixture = Fixture::new(policy()).await;
        let (pipe, mut remote) = tokio::join!(
            bounded(fixture.session.as_mut().unwrap().open_inbound(NETWORK, PEER)),
            bounded(inbound_opened(&fixture.listener, PEER))
        );
        let mut pipe = success(pipe);
        remote.bytes(frame).await;
        assert!(matches!(failure(bounded(pipe.recv()).await), RealtimeError::InvalidResponse(_) | RealtimeError::PipeEnded(PipeEnd::InvalidData)));
        remote.eof().await;
        assert!(matches!(failure(bounded(pipe.recv()).await), RealtimeError::PipeEnded(_)));
        success(bounded(pipe.close()).await);
        fixture.finish().await;
    }
}

#[tokio::test]
async fn oversized_inbound_prefix_refuses_before_waiting_for_its_body() {
    let mut fixture = Fixture::new(policy()).await;
    let (pipe, mut remote) = tokio::join!(
        bounded(fixture.session.as_mut().unwrap().open_inbound(NETWORK, PEER)),
        bounded(inbound_opened(&fixture.listener, PEER))
    );
    let mut pipe = success(pipe);
    // No body follows. This must be a local size refusal, not an IO timeout
    // while trying to allocate/read an attacker-selected four-gigabyte body.
    remote.bytes(&[0xff, 0xff, 0xff, 0xff]).await;
    assert!(matches!(failure(bounded(pipe.recv()).await), RealtimeError::LimitExceeded(_)));
    remote.eof().await;
    success(bounded(pipe.close()).await);
    fixture.finish().await;
}

#[tokio::test]
async fn cancelled_partial_receive_closes_the_socket_and_cannot_reuse_a_prefix() {
    let mut fixture = Fixture::new(policy()).await;
    let (pipe, mut remote) = tokio::join!(
        bounded(fixture.session.as_mut().unwrap().open_inbound(NETWORK, PEER)),
        bounded(inbound_opened(&fixture.listener, PEER))
    );
    let mut pipe = success(pipe);
    remote.bytes(&[12, 0]).await;
    let mut receiving = Box::pin(pipe.recv());
    // Incomplete length prefix makes completion impossible. Polling once
    // establishes an in-progress receive without scheduler sleeps or retries.
    std::future::poll_fn(|context| {
        assert!(receiving.as_mut().poll(context).is_pending());
        std::task::Poll::Ready(())
    }).await;
    drop(receiving);
    remote.eof().await;
    assert!(matches!(failure(bounded(pipe.recv()).await), RealtimeError::PipeEnded(_)));
    success(bounded(pipe.close()).await);
    fixture.finish().await;
}

#[tokio::test]
async fn partial_receive_timeout_is_terminal_and_releases_its_reservation() {
    let mut limits = policy();
    limits.io_timeout = Duration::from_millis(40);
    let mut fixture = Fixture::new(limits).await;
    let (pipe, mut remote) = tokio::join!(
        bounded(fixture.session.as_mut().unwrap().open_inbound(NETWORK, PEER)),
        bounded(inbound_opened(&fixture.listener, PEER))
    );
    let mut pipe = success(pipe);
    remote.bytes(&[12, 0, 0, 0, 1]).await;
    assert!(matches!(failure(bounded(pipe.recv()).await), RealtimeError::Timeout));
    remote.eof().await;
    success(bounded(pipe.close()).await);
    // A new explicit pipe claim can use the slot/budget the failed read held.
    let (pipe, mut remote) = tokio::join!(
        bounded(fixture.session.as_mut().unwrap().open_inbound(NETWORK, "replacement")),
        bounded(inbound_opened(&fixture.listener, "replacement"))
    );
    let pipe = success(pipe);
    success(bounded(pipe.close()).await);
    remote.eof().await;
    fixture.finish().await;
}

#[tokio::test]
async fn outbound_terminal_observer_sees_remote_eof_without_another_send() {
    let mut fixture = Fixture::new(policy()).await;
    let (flow, peers) = tokio::join!(
        bounded(fixture.session.as_mut().unwrap().open_flow(video("v"))),
        bounded(opened(&fixture.listener, open_request("v", "video", "video/H264", 90_000, 0), "v", FLOW_A))
    );
    let mut flow = success(flow);
    let (mut command, remote) = peers;
    command.eof().await;
    let mut termination = flow.termination();
    drop(remote);
    assert_eq!(success(bounded(termination.wait(Duration::from_secs(2))).await), PipeEnd::RemoteClosed);
    assert!(matches!(failure(bounded(flow.send(1, &[1])).await), RealtimeError::PipeEnded(PipeEnd::RemoteClosed)));
    // EOF does not consume F or turn a local socket into a native close ACK.
    let (result, mut command) = tokio::join!(bounded(flow.close()), bounded(closed(&fixture.listener, FLOW_A)));
    success(result);
    command.eof().await;
    fixture.finish().await;
}

#[tokio::test]
async fn dropped_flow_keeps_its_exact_cleanup_owner_and_capacity_charge() {
    let mut limits = policy();
    limits.max_flows = 1;
    let mut fixture = Fixture::new(limits).await;
    let (flow, peers) = tokio::join!(
        bounded(fixture.session.as_mut().unwrap().open_flow(video("v"))),
        bounded(opened(&fixture.listener, open_request("v", "video", "video/H264", 90_000, 0), "v", FLOW_A))
    );
    let flow = success(flow);
    let (mut command, mut remote) = peers;
    command.eof().await;
    assert!(matches!(failure(bounded(fixture.session.as_mut().unwrap().open_flow(video("other"))).await), RealtimeError::LimitExceeded(_)));
    drop(flow);
    remote.eof().await;
    assert!(matches!(failure(bounded(fixture.session.as_mut().unwrap().open_flow(video("other"))).await), RealtimeError::LimitExceeded(_)));
    let (result, mut command) = tokio::join!(
        bounded(fixture.session.take().unwrap().close()),
        bounded(closed(&fixture.listener, FLOW_A))
    );
    success(result);
    command.eof().await;
    fixture.events.eof().await;
}

#[tokio::test]
async fn inbound_capacity_returns_after_explicit_pipe_close() {
    let mut limits = policy();
    limits.max_inbound_pipes = 1;
    let mut fixture = Fixture::new(limits).await;
    let (pipe, mut remote) = tokio::join!(
        bounded(fixture.session.as_mut().unwrap().open_inbound(NETWORK, PEER)),
        bounded(inbound_opened(&fixture.listener, PEER))
    );
    let pipe = success(pipe);
    assert!(matches!(failure(bounded(fixture.session.as_mut().unwrap().open_inbound(NETWORK, "other")).await), RealtimeError::LimitExceeded(_)));
    success(bounded(pipe.close()).await);
    remote.eof().await;
    let (pipe, mut remote) = tokio::join!(
        bounded(fixture.session.as_mut().unwrap().open_inbound(NETWORK, "other")),
        bounded(inbound_opened(&fixture.listener, "other"))
    );
    success(bounded(success(pipe).close()).await);
    remote.eof().await;
    fixture.finish().await;
}

#[tokio::test]
async fn utf8_label_limit_counts_wire_bytes_without_truncation() {
    let mut limits = policy();
    limits.max_frame_bytes = 512;
    limits.max_retained_bytes = 1024;
    let mut fixture = Fixture::new(limits).await;
    let overlong = "\u{e9}".repeat(128); // 128 characters, 256 bytes
    for label in ["", overlong.as_str()] {
        assert!(matches!(failure(bounded(fixture.session.as_mut().unwrap().open_flow(video(label))).await), RealtimeError::InvalidInput(_)));
    }
    let label = "\u{e9}".repeat(127) + "x";
    assert_eq!(label.len(), 255);
    let (flow, peers) = tokio::join!(
        bounded(fixture.session.as_mut().unwrap().open_flow(video(&label))),
        bounded(opened(&fixture.listener, open_request(&label, "video", "video/H264", 90_000, 0), &label, FLOW_A))
    );
    let mut flow = success(flow);
    let (mut command, mut remote) = peers;
    command.eof().await;
    let (sent, frame) = tokio::join!(bounded(flow.send(1, &[])), bounded(remote.frame()));
    success(sent);
    let mut expected = vec![9, 1, 0, 0, 255, 0, 1, 0, 0, 0, 0, 0, 0, 0];
    expected.extend_from_slice(label.as_bytes());
    assert_eq!(frame, expected);
    let (result, mut command) = tokio::join!(bounded(flow.close()), bounded(closed(&fixture.listener, FLOW_A)));
    success(result);
    command.eof().await;
    remote.eof().await;
    fixture.finish().await;
}

#[tokio::test]
async fn known_flow_is_closed_once_when_outbound_pipe_setup_refuses() {
    let mut fixture = Fixture::new(policy()).await;
    let answering = async {
        let mut command = DaemonPeer::verified(&fixture.listener).await;
        assert_eq!(command.request().await, open_request("v", "video", "video/H264", 90_000, 0));
        command.reply(json!({"ok":true,"data":{"flow_label":"v","flow_capability":FLOW_A}})).await;
        let mut pipe = DaemonPeer::verified(&fixture.listener).await;
        assert_eq!(pipe.request().await, outbound_request(FLOW_A));
        pipe.reply(json!({"ok":false,"error":"fixture pipe admission refusal","data":{"code":"flow_refused"}})).await;
        let mut cleanup = closed(&fixture.listener, FLOW_A).await;
        cleanup.eof().await;
        command.eof().await;
        pipe.eof().await;
    };
    let (result, ()) = tokio::join!(
        bounded(fixture.session.as_mut().unwrap().open_flow(video("v"))),
        bounded(answering)
    );
    assert!(matches!(failure(result), RealtimeError::Refused { code: Some(code) } if code == "flow_refused"));
    // No flow was delivered. The exact rollback ACK discharged the known F;
    // finishing the owner must not issue a second close for that capability.
    fixture.finish().await;
}

#[tokio::test]
async fn cancelled_open_tracks_late_capability_until_one_acknowledged_close() {
    let mut fixture = Fixture::new(policy()).await;
    let mut opening = Box::pin(fixture.session.as_mut().unwrap().open_flow(video("v")));
    let mut command = tokio::select! {
        peer = async {
            let mut peer = DaemonPeer::verified(&fixture.listener).await;
            assert_eq!(peer.request().await, open_request("v", "video", "video/H264", 90_000, 0));
            peer
        } => peer,
        _ = &mut opening => panic!("open completed before the withheld capability ACK"),
    };
    drop(opening);
    assert!(fixture.session.as_ref().unwrap().is_fenced());
    let cleanup = async {
        command.reply(json!({"ok":true,"data":{"flow_label":"v","flow_capability":FLOW_A}})).await;
        let mut next = DaemonPeer::verified(&fixture.listener).await;
        let request = next.request().await;
        let mut pipe = None;
        let mut close = if request.get("op").and_then(Value::as_str) == Some("realtime_pipe") {
            // A cancelled offer may be noticed before or after pipe setup.
            // Both paths must still retire exactly the same returned F once.
            assert_eq!(request, outbound_request(FLOW_A));
            next.reply(json!({"ok":true,"data":{"realtime_pipe":true}})).await;
            pipe = Some(next);
            let mut close = DaemonPeer::verified(&fixture.listener).await;
            assert_eq!(close.request().await, close_request(FLOW_A));
            close
        } else {
            assert_eq!(request, close_request(FLOW_A));
            next
        };
        close.reply(json!({"ok":true,"data":{"closed":true}})).await;
        close.eof().await;
        if let Some(mut pipe) = pipe {
            pipe.eof().await;
        }
        command.eof().await;
    };
    let (result, ()) = tokio::join!(bounded(fixture.session.take().unwrap().close()), bounded(cleanup));
    success(result);
    fixture.events.eof().await;
}

#[tokio::test]
async fn unknown_open_capability_is_uncertain_and_fences_label_retry() {
    for capability in [None, Some(json!("")), Some(json!(7))] {
        let mut fixture = Fixture::new(policy()).await;
        let answering = async {
            let mut command = DaemonPeer::verified(&fixture.listener).await;
            assert_eq!(command.request().await, open_request("v", "video", "video/H264", 90_000, 0));
            let mut data = json!({"flow_label":"v"});
            if let Some(capability) = capability {
                data["flow_capability"] = capability;
            }
            command.reply(json!({"ok":true,"data":data})).await;
            command.eof().await;
        };
        let (result, ()) = tokio::join!(
            bounded(fixture.session.as_mut().unwrap().open_flow(video("v"))),
            bounded(answering)
        );
        assert!(matches!(failure(result), RealtimeError::CleanupUncertain));
        assert!(fixture.session.as_ref().unwrap().is_fenced());
        let (finished_tx, mut finished_rx) = tokio::sync::oneshot::channel();
        let owner_close = async {
            assert!(matches!(failure(fixture.session.as_mut().unwrap().open_flow(video("v")).await), RealtimeError::SessionFenced));
            let result = fixture.session.take().unwrap().close().await;
            let _ = finished_tx.send(());
            result
        };
        let observe = async {
            tokio::select! {
                biased;
                _ = fixture.listener.accept() => {
                    panic!("unknown capability must not trigger a retry or a close connection");
                }
                _ = &mut finished_rx => {}
            }
        };
        let (result, ()) = tokio::join!(bounded(owner_close), bounded(observe));
        assert!(matches!(failure(result), RealtimeError::CleanupUncertain));
        fixture.events.eof().await;
    }
}

#[tokio::test]
async fn refusal_codes_survive_without_exposing_client_or_known_flow_secrets() {
    let mut fixture = Fixture::new(policy()).await;
    let (flow, peers) = tokio::join!(
        bounded(fixture.session.as_mut().unwrap().open_flow(video("v"))),
        bounded(opened(&fixture.listener, open_request("v", "video", "video/H264", 90_000, 0), "v", FLOW_A))
    );
    let flow = success(flow);
    let (mut command, mut pipe) = peers;
    command.eof().await;
    for code in ["session_not_current", "label_in_use", "flow_refused", "provider_configuration_invalid", CLIENT_SECRET, FLOW_A] {
        let answering = async {
            let mut command = DaemonPeer::verified(&fixture.listener).await;
            assert_eq!(command.request().await, open_request("other", "video", "video/H264", 90_000, 0));
            command.reply(json!({"ok":false,
                "error":format!("remote diagnostic {CLIENT_SECRET} {FLOW_A}"),
                "data":{"code":code}
            })).await;
            command.eof().await;
        };
        let (result, ()) = tokio::join!(
            bounded(fixture.session.as_mut().unwrap().open_flow(video("other"))),
            bounded(answering)
        );
        let error = failure(result);
        let rendered = format!("{error} {error:?}");
        assert!(!rendered.contains(CLIENT_SECRET));
        assert!(!rendered.contains(FLOW_A));
        assert!(!rendered.contains("remote diagnostic"));
        if code != CLIENT_SECRET && code != FLOW_A {
            assert!(matches!(error, RealtimeError::Refused { code: Some(value) } if value == code));
        } else {
            assert!(matches!(error, RealtimeError::Refused { .. }));
        }
    }
    let (result, mut command) = tokio::join!(bounded(flow.close()), bounded(closed(&fixture.listener, FLOW_A)));
    success(result);
    command.eof().await;
    pipe.eof().await;
    fixture.finish().await;
}

#[tokio::test]
async fn owner_close_attempts_every_known_flow_and_reports_unacknowledged_retirement() {
    let mut fixture = Fixture::new(policy()).await;
    let (first, peers) = tokio::join!(
        bounded(fixture.session.as_mut().unwrap().open_flow(video("v"))),
        bounded(opened(&fixture.listener, open_request("v", "video", "video/H264", 90_000, 0), "v", FLOW_A))
    );
    let (mut first_command, mut first_pipe) = peers;
    first_command.eof().await;
    let (second, peers) = tokio::join!(
        bounded(fixture.session.as_mut().unwrap().open_flow(audio("a"))),
        bounded(opened(&fixture.listener, open_request("a", "audio", "audio/opus", 48_000, 2), "a", FLOW_B))
    );
    let (mut second_command, mut second_pipe) = peers;
    second_command.eof().await;
    let first = success(first);
    let second = success(second);
    let (inbound, mut inbound_remote) = tokio::join!(
        bounded(fixture.session.as_mut().unwrap().open_inbound(NETWORK, PEER)),
        bounded(inbound_opened(&fixture.listener, PEER))
    );
    let inbound = success(inbound);
    let answering = async {
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..2 {
            let mut command = DaemonPeer::verified(&fixture.listener).await;
            let request = command.request().await;
            let capability = request["flow_capability"].as_str().unwrap().to_owned();
            assert!(capability == FLOW_A || capability == FLOW_B);
            assert!(seen.insert(capability.clone()), "consumed close must never be retried");
            assert_eq!(request, close_request(&capability));
            if capability == FLOW_A {
                command.reply(json!({"ok":false,"error":"fixture consumed close refused","data":{"code":"flow_refused"}})).await;
            } else {
                command.reply(json!({"ok":true,"data":{"closed":true}})).await;
            }
            command.eof().await;
        }
        assert_eq!(seen.len(), 2);
    };
    let (result, ()) = tokio::join!(bounded(fixture.session.take().unwrap().close()), bounded(answering));
    assert!(matches!(failure(result), RealtimeError::CleanupUncertain));
    // Local owner end must close idle sockets while the external handles
    // remain alive. No later send/recv or handle Drop may be needed to do so.
    first_pipe.eof().await;
    second_pipe.eof().await;
    inbound_remote.eof().await;
    fixture.events.eof().await;
    drop((first, second, inbound));
}

#[tokio::test]
async fn inbound_direction_cannot_send_or_acquire_an_outbound_pipe() {
    let mut fixture = Fixture::new(policy()).await;
    let mut spec = video("v");
    spec.direction = FlowDirection::Inbound;
    let answering = async {
        let mut command = DaemonPeer::verified(&fixture.listener).await;
        let mut expected = open_request("v", "video", "video/H264", 90_000, 0);
        expected["direction"] = json!("inbound");
        assert_eq!(command.request().await, expected);
        command.reply(json!({"ok":true,"data":{"flow_label":"v","flow_capability":FLOW_A}})).await;
        command
    };
    let (flow, mut command) = tokio::join!(bounded(fixture.session.as_mut().unwrap().open_flow(spec)), bounded(answering));
    let mut flow = success(flow);
    command.eof().await;
    assert!(matches!(failure(bounded(flow.send(1, &[1])).await), RealtimeError::InvalidInput(_)));
    // The next accepted command must be exact close, never an outbound pipe.
    let (result, mut command) = tokio::join!(bounded(flow.close()), bounded(closed(&fixture.listener, FLOW_A)));
    success(result);
    command.eof().await;
    fixture.finish().await;
}

#[tokio::test]
async fn another_client_cannot_adopt_an_owned_event_session() {
    let endpoint = PrivateEndpoint::new();
    let listener = endpoint.listen();
    let client = endpoint.client();
    let (events, mut peer, _receiver) = event_registration(&listener, &client, ClientId(7), CLIENT_SECRET).await;
    let foreign_client = endpoint.client();
    assert!(matches!(failure(RealtimeSession::new(foreign_client, events, policy())), RealtimeError::StaleSession));
    peer.eof().await;
}

#[tokio::test]
async fn event_replacement_fences_old_io_and_cleanup_keeps_the_new_owner_alive() {
    const NEXT_SECRET: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
    let mut fixture = Fixture::new(policy()).await;
    let old_generation = fixture.session.as_ref().unwrap().generation();
    let (flow, peers) = tokio::join!(
        bounded(fixture.session.as_mut().unwrap().open_flow(video("v"))),
        bounded(opened(&fixture.listener, open_request("v", "video", "video/H264", 90_000, 0), "v", FLOW_A))
    );
    let mut flow = success(flow);
    let (mut command, mut old_pipe) = peers;
    command.eof().await;
    let (old_inbound, mut old_inbound_remote) = tokio::join!(
        bounded(fixture.session.as_mut().unwrap().open_inbound(NETWORK, PEER)),
        bounded(inbound_opened(&fixture.listener, PEER))
    );
    let old_inbound = success(old_inbound);
    let (next_events, mut next_peer, _next_receiver) =
        event_registration(&fixture.listener, &fixture.client, ClientId(8), NEXT_SECRET).await;
    let mut next = success(RealtimeSession::new(Arc::clone(&fixture.client), next_events, policy()));
    assert_ne!(next.generation(), old_generation);
    assert!(!fixture.session.as_ref().unwrap().is_current());
    fixture.events.eof().await;
    // Registration replacement must release both idle media directions before
    // an old handle is operated on or dropped.
    old_pipe.eof().await;
    old_inbound_remote.eof().await;
    assert!(matches!(failure(bounded(flow.send(1, &[1])).await), RealtimeError::StaleSession | RealtimeError::SessionFenced | RealtimeError::PipeEnded(PipeEnd::SessionEnded)));
    drop((flow, old_inbound));
    let (result, mut command) = tokio::join!(bounded(fixture.session.take().unwrap().close()), bounded(closed(&fixture.listener, FLOW_A)));
    success(result);
    command.eof().await;
    assert!(next.is_current(), "old cleanup must not invalidate the successor generation");
    let answering = async {
        let mut peer = DaemonPeer::verified(&fixture.listener).await;
        assert_eq!(peer.request().await, json!({
            "op":"realtime_pipe","direction":"inbound","network":NETWORK,
            "peer":PEER,"client_id":ClientId(8).to_string(),"client_capability":NEXT_SECRET
        }));
        peer.reply(json!({"ok":true,"data":{"realtime_pipe":true}})).await;
        peer
    };
    let (pipe, mut remote) = tokio::join!(bounded(next.open_inbound(NETWORK, PEER)), bounded(answering));
    let mut pipe = success(pipe);
    remote.bytes(&[12, 0, 0, 0, 1, 0, 3, 0, 0, 0, 1, 0, 0, 0, b'v', 9]).await;
    let unit = success(bounded(pipe.recv()).await);
    assert_eq!(unit.payload(), &[9]);
    drop(unit);
    // Drop is only a local socket-lifetime boundary, not an acknowledged
    // upstream flow retirement. The idle pipe handle remains alive here.
    drop(next);
    remote.eof().await;
    next_peer.eof().await;
    drop(pipe);
}

#[tokio::test]
async fn cancelled_close_is_uncertain_without_retrying_the_consumed_capability() {
    let mut fixture = Fixture::new(policy()).await;
    let (first, peers) = tokio::join!(
        bounded(fixture.session.as_mut().unwrap().open_flow(video("v"))),
        bounded(opened(&fixture.listener, open_request("v", "video", "video/H264", 90_000, 0), "v", FLOW_A))
    );
    let first = success(first);
    let (mut first_command, mut first_pipe) = peers;
    first_command.eof().await;
    let (second, peers) = tokio::join!(
        bounded(fixture.session.as_mut().unwrap().open_flow(audio("a"))),
        bounded(opened(&fixture.listener, open_request("a", "audio", "audio/opus", 48_000, 2), "a", FLOW_B))
    );
    let (mut second_command, mut second_pipe) = peers;
    second_command.eof().await;
    let mut closing = Box::pin(first.close());
    let mut unacknowledged = tokio::select! {
        peer = async {
            let mut command = DaemonPeer::verified(&fixture.listener).await;
            assert_eq!(command.request().await, close_request(FLOW_A));
            command
        } => peer,
        _ = &mut closing => panic!("close completed before the withheld retirement ACK"),
    };
    drop(closing);
    unacknowledged.eof().await;
    first_pipe.eof().await;
    drop(success(second));
    second_pipe.eof().await;
    let (finished_tx, mut finished_rx) = tokio::sync::oneshot::channel();
    let owner_close = async {
        let result = fixture.session.take().unwrap().close().await;
        let _ = finished_tx.send(());
        result
    };
    let observe = async {
        let mut closes = 0;
        loop {
            tokio::select! {
                biased;
                mut command = DaemonPeer::verified(&fixture.listener) => {
                    // F_A may already have been consumed upstream. Only B is
                    // still known and available for one exact close attempt.
                    assert_eq!(command.request().await, close_request(FLOW_B));
                    closes += 1;
                    assert_eq!(closes, 1, "no known capability may be closed twice");
                    command.reply(json!({"ok":true,"data":{"closed":true}})).await;
                    command.eof().await;
                }
                _ = &mut finished_rx => break,
            }
        }
        assert_eq!(closes, 1, "uncertainty must not skip the other known flow");
    };
    let (result, ()) = tokio::join!(bounded(owner_close), bounded(observe));
    assert!(matches!(failure(result), RealtimeError::CleanupUncertain));
    fixture.events.eof().await;
}

#[tokio::test]
async fn impossible_body_budget_and_deadline_policies_refuse_before_transport() {
    let mut invalid = Vec::new();
    let mut too_short_body = policy();
    too_short_body.max_frame_bytes = 10; // even empty payload needs a label
    invalid.push(too_short_body);
    let mut too_short_retention = policy();
    too_short_retention.max_retained_bytes = 14; // minimum body11 + prefix4
    invalid.push(too_short_retention);
    let mut no_deadline = policy();
    no_deadline.operation_timeout = Duration::ZERO;
    invalid.push(no_deadline);
    let mut overflow_deadline = policy();
    overflow_deadline.io_timeout = Duration::MAX;
    invalid.push(overflow_deadline);
    for limits in invalid {
        let endpoint = PrivateEndpoint::new();
        let listener = endpoint.listen();
        let client = endpoint.client();
        let (events, mut peer, _receiver) =
            event_registration(&listener, &client, ClientId(7), CLIENT_SECRET).await;
        assert!(matches!(failure(RealtimeSession::new(client, events, limits)), RealtimeError::InvalidPolicy(_)));
        peer.eof().await;
    }
}
