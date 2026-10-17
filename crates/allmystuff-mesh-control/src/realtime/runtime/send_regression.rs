//! Real local-IPC tests of the send/terminal primitive, not FlowOpen or native-F retirement.
//!
//! A real Events registration funds the existing sender policy. The test constructs
//! only its private flow primitive so it can retain the actual reader before the
//! unchanged monitor is spawned. It never fabricates Transport or calls IoGuard.

use super::{
    outbound_read_error_end, spawn_monitor, FlowState, Life, RealtimeFlow, RealtimeSession,
};
use crate::realtime::{
    Encoding, FlowDirection, FlowSpec, PipeEnd, RealtimeError, RealtimePolicy,
    Result as RealtimeResult, RtpKind,
};
use crate::{ClientId, ControlClient, Endpoint, EventContract};
use interprocess::local_socket::tokio::{prelude::*, Listener, RecvHalf};
#[cfg(unix)]
use interprocess::local_socket::GenericFilePath;
#[cfg(not(unix))]
use interprocess::local_socket::GenericNamespaced;
use interprocess::local_socket::ListenerOptions;
use serde_json::{json, Value};
use std::future::{poll_fn, Future};
use std::io::ErrorKind;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

const DEADLINE: Duration = Duration::from_secs(10);
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const COOPERATIVE_POLL_BOUND: usize = 256;

async fn bounded<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(DEADLINE, future)
        .await
        .expect("owned send-origin fixture deadline")
}

#[derive(Debug, PartialEq, Eq)]
enum SendOutcome {
    Ok,
    Transport,
    PipeEnded(PipeEnd),
    Other,
}

fn outcome(result: &RealtimeResult<()>) -> SendOutcome {
    match result {
        Ok(()) => SendOutcome::Ok,
        Err(RealtimeError::Transport) => SendOutcome::Transport,
        Err(RealtimeError::PipeEnded(end)) => SendOutcome::PipeEnded(*end),
        Err(_) => SendOutcome::Other,
    }
}

struct PrivateEndpoint {
    #[cfg(unix)]
    directory: std::path::PathBuf,
    #[cfg(unix)]
    path: std::path::PathBuf,
    #[cfg(not(unix))]
    name: String,
    cleanup_attempted: bool,
}

impl PrivateEndpoint {
    fn new() -> Self {
        let mut nonce = [0_u8; 8];
        getrandom::getrandom(&mut nonce).expect("private send-origin nonce");
        let label = format!("amrs{:x}", u64::from_le_bytes(nonce));
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            use std::os::unix::fs::DirBuilderExt;
            let directory = std::env::temp_dir().join(label);
            let path = directory.join("s");
            assert!(path.as_os_str().as_bytes().len() < 104);
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&directory)
                .expect("exclusive send-origin directory");
            Self {
                directory,
                path,
                cleanup_attempted: false,
            }
        }
        #[cfg(not(unix))]
        {
            Self {
                name: label,
                cleanup_attempted: false,
            }
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
        let name = self
            .name
            .as_str()
            .to_ns_name::<GenericNamespaced>()
            .unwrap();
        let listener = ListenerOptions::new().name(name).create_tokio().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        listener
    }

    async fn connect(&self) -> LocalSocketStream {
        #[cfg(unix)]
        let name = self.path.as_path().to_fs_name::<GenericFilePath>().unwrap();
        #[cfg(not(unix))]
        let name = self
            .name
            .as_str()
            .to_ns_name::<GenericNamespaced>()
            .unwrap();
        bounded(LocalSocketStream::connect(name)).await.unwrap()
    }

    fn cleanup(&mut self) -> bool {
        self.cleanup_attempted = true;
        #[cfg(unix)]
        {
            let unlinked = match std::fs::remove_file(&self.path) {
                Ok(()) => true,
                Err(error) => error.kind() == std::io::ErrorKind::NotFound,
            };
            let removed = std::fs::remove_dir(&self.directory).is_ok();
            unlinked && removed && !self.path.exists() && !self.directory.exists()
        }
        #[cfg(not(unix))]
        {
            true
        }
    }
}

impl Drop for PrivateEndpoint {
    fn drop(&mut self) {
        if !self.cleanup_attempted {
            let _ = self.cleanup();
        }
    }
}

#[derive(Debug)]
struct Cleanup {
    monitor_joined: bool,
    peer_eof: Option<bool>,
    owner_closed: bool,
    events_eof: bool,
    private_endpoint_removed: bool,
}

struct Fixture {
    flow: Option<RealtimeFlow>,
    state: Arc<FlowState>,
    reader: Option<BufReader<RecvHalf>>,
    peer: Option<LocalSocketStream>,
    owner: Option<RealtimeSession>,
    events: Option<BufReader<LocalSocketStream>>,
    _event_receiver: mpsc::Receiver<Value>,
    listener: Option<Listener>,
    endpoint: PrivateEndpoint,
}

impl Fixture {
    async fn new() -> Self {
        let endpoint = PrivateEndpoint::new();
        let listener = endpoint.listen();
        let client = endpoint.client();
        let (sender, receiver) = mpsc::channel(8);
        let subscribing =
            client.subscribe_events_for_contract(EventContract::CandidateV1Db7818e, sender);
        let registering = async {
            let mut events = BufReader::new(bounded(listener.accept()).await.unwrap());
            let line = bounded(crate::transport::read_json_line(
                &mut events,
                Some(16 * 1024),
            ))
            .await
            .expect("bounded Events request");
            assert_eq!(
                serde_json::from_slice::<Value>(&line).unwrap(),
                json!({"op":"events_subscribe"})
            );
            let mut ack = serde_json::to_vec(&json!({"ok":true,"data":{
                "subscribed":true,"client_id":ClientId(7).to_string(),
                "client_capability":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
            }})).unwrap();
            ack.push(b'\n');
            bounded(events.get_mut().write_all(&ack)).await.unwrap();
            bounded(events.get_mut().flush()).await.unwrap();
            events
        };
        let (session, events) = tokio::join!(bounded(subscribing), bounded(registering));
        let owner = RealtimeSession::new(
            client,
            session.unwrap(),
            RealtimePolicy {
                max_flows: 4,
                max_inbound_pipes: 2,
                max_frame_bytes: 64,
                max_retained_bytes: 256,
                operation_timeout: IO_TIMEOUT,
                io_timeout: IO_TIMEOUT,
            },
        )
        .unwrap();
        let (stream, peer) = tokio::join!(endpoint.connect(), bounded(listener.accept()));
        let (reader, writer) = stream.split();
        // No FlowOpen/F is negotiated: this is the private send primitive only.
        let state = FlowState::new(FlowSpec {
            network: "fixture-network".into(),
            peer: "fixture-peer".into(),
            label: "v".into(),
            direction: FlowDirection::Outbound,
            encoding: Encoding {
                rtp_kind: RtpKind::Video,
                mime: "video/H264".into(),
                clock_rate: 90_000,
                channels: 0,
            },
        });
        state.record.lock().life = Life::Open;
        *state.writer.lock() = Some(writer);
        let flow = RealtimeFlow {
            shared: Arc::clone(&owner.shared),
            state: Arc::clone(&state),
        };
        Self {
            flow: Some(flow),
            state,
            reader: Some(BufReader::new(reader)),
            peer: Some(peer.unwrap()),
            owner: Some(owner),
            events: Some(events),
            _event_receiver: receiver,
            listener: Some(listener),
            endpoint,
        }
    }

    async fn healthy_frame(&mut self) -> (SendOutcome, [u8; 16]) {
        let sending = self.flow.as_mut().unwrap().send(1, &[0x65]);
        let peer = self.peer.as_mut().unwrap();
        let reading = async {
            let mut frame = [0_u8; 16];
            bounded(peer.read_exact(&mut frame)).await.unwrap();
            frame
        };
        let (sent, frame) = tokio::join!(bounded(sending), bounded(reading));
        (outcome(&sent), frame)
    }

    fn start_monitor(&mut self) {
        let reader = self.reader.take().expect("original reader starts once");
        assert!(spawn_monitor(&self.owner.as_ref().unwrap().shared, &self.state, reader).is_ok());
    }

    async fn join_completed_monitor(&self) -> bool {
        let monitor = self
            .state
            .monitor
            .lock()
            .take()
            .expect("original tracked monitor");
        bounded(monitor).await.is_ok()
    }

    async fn finish(mut self) -> Cleanup {
        drop(self.flow.take());
        let monitor = self.state.monitor.lock().take();
        let monitor_joined = if let Some(monitor) = monitor {
            match bounded(monitor).await {
                Ok(()) => true,
                Err(error) => error.is_cancelled(),
            }
        } else {
            true
        };
        drop(self.reader.take());
        let peer_eof = if let Some(mut peer) = self.peer.take() {
            let mut byte = [0_u8; 1];
            let eof = matches!(bounded(peer.read(&mut byte)).await, Ok(0));
            drop(peer);
            Some(eof)
        } else {
            None
        };
        let owner_closed = bounded(self.owner.take().unwrap().close()).await.is_ok();
        let mut events = self.events.take().unwrap();
        let mut byte = [0_u8; 1];
        let events_eof = matches!(bounded(events.read(&mut byte)).await, Ok(0));
        drop(events);
        drop(self.listener.take());
        let private_endpoint_removed = self.endpoint.cleanup();
        Cleanup {
            monitor_joined,
            peer_eof,
            owner_closed,
            events_eof,
            private_endpoint_removed,
        }
    }
}

fn assert_cleanup(cleanup: &Cleanup) {
    assert!(cleanup.monitor_joined, "original monitor must be joined");
    assert!(
        cleanup.owner_closed && cleanup.events_eof,
        "real Events owner must close"
    );
    assert!(
        cleanup.private_endpoint_removed,
        "private endpoint must be removed"
    );
}

#[test]
fn outbound_monitor_connection_reset_is_remote_closed() {
    assert_eq!(
        outbound_read_error_end(ErrorKind::ConnectionReset),
        PipeEnd::RemoteClosed,
    );
}

#[test]
fn outbound_monitor_other_read_errors_are_not_remote_close() {
    for error_kind in [
        ErrorKind::PermissionDenied,
        ErrorKind::Other,
        ErrorKind::BrokenPipe,
        ErrorKind::ConnectionAborted,
        ErrorKind::UnexpectedEof,
    ] {
        assert_eq!(
            outbound_read_error_end(error_kind),
            PipeEnd::IoFailure,
            "unrelated read error {error_kind:?} must remain an IO failure",
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn completed_transport_error_preserves_native_monitor_origin() {
    let mut fixture = Fixture::new().await;
    let (healthy, frame) = fixture.healthy_frame().await;
    drop(fixture.peer.take());
    // The real reader has not been polled. The actual native write must fail
    // before the existing monitor may independently observe its EOF/IO result.
    let completed = outcome(&bounded(fixture.flow.as_mut().unwrap().send(1, &[0x66])).await);
    let before_monitor = *fixture.state.terminal.borrow();
    let writer_not_reusable = fixture.state.writer.lock().is_none();
    let reuse = outcome(&bounded(fixture.flow.as_mut().unwrap().send(1, &[0x67])).await);
    let after_reuse = *fixture.state.terminal.borrow();
    fixture.start_monitor();
    let monitor_completed = fixture.join_completed_monitor().await;
    let mut termination = fixture.flow.as_ref().unwrap().termination();
    let after_monitor = bounded(termination.wait(IO_TIMEOUT)).await.ok();
    let owner_active = fixture.owner.as_ref().unwrap().registration().is_active();
    let cleanup = fixture.finish().await;
    // Setup, controls, and complete owned cleanup precede the normative failure.
    assert_eq!(healthy, SendOutcome::Ok);
    assert_eq!(
        frame,
        [12, 0, 0, 0, 1, 0, 1, 0, 0, 0, 1, 0, 0, 0, b'v', 0x65]
    );
    assert_eq!(
        completed,
        SendOutcome::Transport,
        "actual closed-peer send must finish Transport"
    );
    assert!(writer_not_reusable);
    assert_eq!(
        reuse,
        SendOutcome::PipeEnded(PipeEnd::LocalClosed),
        "taken writer cannot send again"
    );
    assert_eq!(
        after_reuse, before_monitor,
        "non-reuse must not invent a terminal signal"
    );
    assert!(monitor_completed && owner_active);
    assert_eq!(
        cleanup.peer_eof, None,
        "peer was explicitly closed before failing send"
    );
    assert_cleanup(&cleanup);
    eprintln!("MESH_REALTIME_SEND_REGRESSION completed_error controls_and_cleanup_passed");
    assert_eq!(
        before_monitor, None,
        "completed Transport must not manufacture caller LocalClosed"
    );
    assert_eq!(
        after_monitor,
        Some(PipeEnd::RemoteClosed),
        "unchanged monitor must report this clean native peer EOF"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_pending_send_keeps_local_origin_and_cannot_reuse_socket() {
    let mut fixture = Fixture::new().await;
    let (healthy, frame) = fixture.healthy_frame().await;
    fixture.start_monitor();
    let state = Arc::clone(&fixture.state);
    let writer_present_before = state.writer.lock().is_some();
    let terminal_before = *state.terminal.borrow();
    let mut sending = Box::pin(fixture.flow.as_mut().unwrap().send(1, &[0x66]));
    // Locked Tokio 1.52.3 starts a task poll with 128 cooperative units, and its
    // native IO registration checks poll_proceed before writable readiness.
    // Consume only that scheduler quota, then poll the real send once without
    // yielding. No OS buffer, payload, resource policy or deadline is enlarged.
    let (
        quota_exhausted,
        quota_steps,
        pending,
        ready_outcome,
        writer_taken,
        terminal_while_pending,
    ) = poll_fn(|context| {
        let mut quota_exhausted = false;
        let mut quota_steps = 0;
        for _ in 0..COOPERATIVE_POLL_BOUND {
            match tokio::task::coop::poll_proceed(context) {
                Poll::Ready(permit) => {
                    permit.made_progress();
                    quota_steps += 1;
                }
                Poll::Pending => {
                    quota_exhausted = true;
                    break;
                }
            }
        }
        let polled = sending.as_mut().poll(context);
        let (pending, ready_outcome) = match polled {
            Poll::Pending => (true, None),
            Poll::Ready(result) => (false, Some(outcome(&result))),
        };
        Poll::Ready((
            quota_exhausted,
            quota_steps,
            pending,
            ready_outcome,
            state.writer.lock().is_none(),
            *state.terminal.borrow(),
        ))
    })
    .await;
    // In send there is no await between taking the writer and arming IoGuard.
    // Pending plus taken custody therefore establishes that cancellation edge.
    drop(sending);
    let after_drop = *state.terminal.borrow();
    let writer_missing_after_drop = state.writer.lock().is_none();
    let mut termination = fixture.flow.as_ref().unwrap().termination();
    let observed = bounded(termination.wait(IO_TIMEOUT)).await.ok();
    let reuse = outcome(&bounded(fixture.flow.as_mut().unwrap().send(1, &[0x67])).await);
    let cleanup = fixture.finish().await;
    assert_eq!(healthy, SendOutcome::Ok);
    assert_eq!(
        frame,
        [12, 0, 0, 0, 1, 0, 1, 0, 0, 0, 1, 0, 0, 0, b'v', 0x65]
    );
    assert!(writer_present_before && terminal_before.is_none());
    assert!(
        quota_exhausted && quota_steps <= 128,
        "locked cooperative quota must be exhausted within bound"
    );
    assert!(
        pending && ready_outcome.is_none(),
        "actual send must be genuinely Pending"
    );
    assert!(
        writer_taken && terminal_while_pending.is_none(),
        "Pending send must own writer with no terminal evidence"
    );
    assert!(writer_missing_after_drop);
    assert_eq!(reuse, SendOutcome::PipeEnded(PipeEnd::LocalClosed));
    assert_eq!(
        cleanup.peer_eof,
        Some(true),
        "cancelled socket must close without an extra frame"
    );
    assert_cleanup(&cleanup);
    eprintln!("MESH_REALTIME_SEND_REGRESSION pending_cancel controls_and_cleanup_passed");
    assert_eq!(after_drop, Some(PipeEnd::LocalClosed));
    assert_eq!(observed, Some(PipeEnd::LocalClosed));
}
