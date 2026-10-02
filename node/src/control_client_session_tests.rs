//! Private fake-daemon fixtures. No default endpoint, installed daemon or peer.
//! Kept separate from the implementation so wire expectations stay independent.

use super::*;

use std::future::Future;
use std::io::Write;
use std::sync::Mutex;

use interprocess::local_socket::tokio::Listener;
use interprocess::local_socket::ListenerOptions;
use serde_json::{json, Value};

const DEADLINE: Duration = Duration::from_secs(10);
const SECRET_A: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const SECRET_B: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

struct Endpoint {
    #[cfg(unix)]
    directory: std::path::PathBuf,
    #[cfg(not(unix))]
    name: String,
}

impl Endpoint {
    fn new() -> Self {
        let mut nonce = [0_u8; 8];
        getrandom::getrandom(&mut nonce).expect("fixture endpoint nonce");
        let suffix = u64::from_le_bytes(nonce);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;

            // Short leaf and socket names also fit ordinary macOS TMPDIR paths.
            let directory = std::env::temp_dir().join(format!("acs{suffix:x}"));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&directory)
                .expect("create uniquely owned private fixture directory");
            Self { directory }
        }
        #[cfg(not(unix))]
        {
            Self {
                name: format!("ams-control-session-{}-{suffix:x}", std::process::id()),
            }
        }
    }

    fn address(&self) -> SocketAddr {
        #[cfg(unix)]
        {
            SocketAddr::Path(self.directory.join("s"))
        }
        #[cfg(not(unix))]
        {
            SocketAddr::Name(self.name.clone())
        }
    }

    fn client(&self) -> ControlClient {
        ControlClient::for_test_address(self.address())
    }

    fn bind(&self) -> Listener {
        #[cfg(unix)]
        let socket_path = self.directory.join("s");
        #[cfg(unix)]
        let name = socket_path
            .as_path()
            .to_fs_name::<GenericFilePath>()
            .expect("fixture Unix socket name");
        #[cfg(not(unix))]
        let name = self
            .name
            .as_str()
            .to_ns_name::<GenericNamespaced>()
            .expect("fixture Windows pipe name");

        let listener = ListenerOptions::new()
            .name(name)
            .create_tokio()
            .expect("bind only this fixture endpoint");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))
                .expect("owner-only fixture socket");
        }
        listener
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            // These are only the exact socket and directory created above.
            let _ = std::fs::remove_file(self.directory.join("s"));
            let _ = std::fs::remove_dir(&self.directory);
        }
    }
}

#[derive(Clone, Default)]
struct Diagnostics(Arc<Mutex<Vec<u8>>>);

impl Write for Diagnostics {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn run<T>(fixture: impl Future<Output = T>) -> (T, String) {
    let diagnostics = Diagnostics::default();
    let sink = diagnostics.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .without_time()
        .with_writer(move || sink.clone())
        .finish();
    let result = tracing::subscriber::with_default(subscriber, || {
        // Spawned reader polls inherit this thread's diagnostic capture. Runtime
        // teardown also closes fixture tasks if an assertion unwinds.
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                tokio::time::timeout(DEADLINE, fixture)
                    .await
                    .expect("private fake-daemon fixture deadline")
            })
    });
    let recorded = diagnostics.0.lock().unwrap().clone();
    (result, String::from_utf8(recorded).unwrap())
}

async fn read_request<R: tokio::io::AsyncRead + Unpin>(reader: &mut BufReader<R>) -> Value {
    let mut line = String::new();
    let count = reader.read_line(&mut line).await.expect("read fixture request");
    assert_ne!(count, 0, "expected a fixture request before EOF");
    serde_json::from_str(line.trim()).expect("request is one valid JSON line")
}

async fn write_line<W: tokio::io::AsyncWrite + Unpin>(writer: &mut W, line: &str) {
    writer.write_all(line.as_bytes()).await.unwrap();
    writer.write_all(b"\n").await.unwrap();
    writer.flush().await.unwrap();
}

async fn write_json<W: tokio::io::AsyncWrite + Unpin>(writer: &mut W, value: &Value) {
    write_line(writer, &serde_json::to_string(value).unwrap()).await;
}

async fn assert_eof<R: tokio::io::AsyncRead + Unpin>(reader: &mut BufReader<R>) {
    let mut line = String::new();
    match reader.read_line(&mut line).await {
        Ok(0) => {}
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::UnexpectedEof
            ) => {}
        result => panic!("expected closed fixture socket, got {result:?}"),
    }
}

fn assert_secret_safe(text: &str) {
    for secret in [SECRET_A, SECRET_B] {
        assert!(!text.contains(secret), "synthetic capability leaked into diagnostics");
    }
}

fn candidate_ack(id: &str, secret: &str) -> Value {
    json!({"ok":true,"data":{"client_id":id,"client_capability":secret}})
}

async fn accept_control(listener: &Listener, contract: EventContract) -> BufReader<LocalSocketStream> {
    let mut socket = BufReader::new(listener.accept().await.unwrap());
    if matches!(contract, EventContract::LegacyV0_3_21) {
        assert_eq!(read_request(&mut socket).await, json!({"op":"status"}));
        write_json(socket.get_mut(), &json!({"ok":true,"data":{"version":"0.3.21"}})).await;
    }
    socket
}

async fn accept_events(listener: &Listener, ack: &Value, contract: EventContract) -> BufReader<LocalSocketStream> {
    let mut socket = accept_control(listener, contract).await;
    assert_eq!(read_request(&mut socket).await, json!({"op":"events_subscribe"}));
    write_json(socket.get_mut(), ack).await;
    socket
}

fn check_refused_acks(contract: EventContract, acks: Vec<String>) {
    let (_, diagnostics) = run(async {
        for ack in acks {
            let endpoint = Endpoint::new();
            let listener = endpoint.bind();
            let server = tokio::spawn(async move {
                let mut socket = accept_control(&listener, contract).await;
                assert_eq!(read_request(&mut socket).await, json!({"op":"events_subscribe"}));
                write_line(socket.get_mut(), &ack).await;
                assert_eof(&mut socket).await;
            });
            let (tx, mut rx) = mpsc::channel(1);
            let client = endpoint.client();
            let result = client.subscribe_events_for_contract(contract, tx).await;
            let error = result.err().expect("invalid ACK must refuse the subscription");
            assert_secret_safe(&format!("{error}\n{error:#}\n{error:?}"));
            assert!(rx.recv().await.is_none(), "failed subscription retained its sender");
            server.await.unwrap();
        }
    });
    assert_secret_safe(&diagnostics);
}

#[test]
fn legacy_ack_retains_id_without_candidate_authority() {
    let (debug, diagnostics) = run(async {
        let endpoint = Endpoint::new();
        let listener = endpoint.bind();
        let server = tokio::spawn(async move {
            let mut socket = accept_events(&listener, &json!({"ok":true,"data":{"client_id":"c7"}}), EventContract::LegacyV0_3_21).await;
            assert_eof(&mut socket).await;
        });
        let (tx, _rx) = mpsc::channel(1);
        let client = endpoint.client();
        let session = client.subscribe_events_for_contract(EventContract::LegacyV0_3_21, tx).await.unwrap();
        let registration = session.registration();
        assert_eq!(registration.client_id().to_string(), "c7");
        assert!(registration.is_active());
        let debug = format!("{session:?} {registration:?}");
        drop(session);
        server.await.unwrap();
        debug
    });
    assert_secret_safe(&debug);
    assert_secret_safe(&diagnostics);
}

#[test]
fn candidate_ack_retains_secret_safe_owned_registration() {
    let (debug, diagnostics) = run(async {
        let endpoint = Endpoint::new();
        let listener = endpoint.bind();
        let server = tokio::spawn(async move {
            let mut socket = accept_events(&listener, &candidate_ack("c7", SECRET_A), EventContract::CandidateV1Db7818e).await;
            assert_eof(&mut socket).await;
        });
        let (tx, _rx) = mpsc::channel(1);
        let client = endpoint.client();
        let session = client.subscribe_events_for_contract(EventContract::CandidateV1Db7818e, tx).await.unwrap();
        let registration = session.registration();
        assert_eq!(registration.client_id().to_string(), "c7");
        assert!(registration.is_active());
        let debug = format!("{session:?} {registration:?}");
        drop(session);
        server.await.unwrap();
        debug
    });
    assert_secret_safe(&debug);
    assert_secret_safe(&diagnostics);
}

#[test]
fn candidate_ack_missing_empty_or_wrong_type_capability_refuses() {
    check_refused_acks(
        EventContract::CandidateV1Db7818e,
        [
            json!({"ok":true,"data":{"client_id":"c7"}}),
            json!({"ok":true,"data":{"client_id":"c7","client_capability":""}}),
            json!({"ok":true,"data":{"client_id":"c7","client_capability":null}}),
            json!({"ok":true,"data":{"client_id":"c7","client_capability":7}}),
        ].into_iter().map(|ack| ack.to_string()).collect(),
    );
}

#[test]
fn missing_or_invalid_client_id_refuses_both_contracts() {
    for contract in [EventContract::LegacyV0_3_21, EventContract::CandidateV1Db7818e] {
        check_refused_acks(
            contract,
            [
                json!({"ok":true}),
                json!({"ok":true,"data":null}),
                json!({"ok":true,"data":{"client_capability":SECRET_A}}),
                json!({"ok":true,"data":{"client_id":"invalid","client_capability":SECRET_A}}),
                json!({"ok":true,"data":{"client_id":7,"client_capability":SECRET_A}}),
            ].into_iter().map(|ack| ack.to_string()).collect(),
        );
    }
}

#[test]
fn malformed_ack_errors_and_diagnostics_never_echo_capability() {
    for contract in [EventContract::LegacyV0_3_21, EventContract::CandidateV1Db7818e] {
        check_refused_acks(
            contract,
            vec![
                format!("not-json-{SECRET_A}"),
                format!("{{\"ok\":true,\"data\":{{\"client_capability\":\"{SECRET_A}\"}}"),
                json!({"ok":SECRET_A,"data":{"client_id":"c7","client_capability":SECRET_B}}).to_string(),
            ],
        );
    }
}

#[test]
fn refused_ack_errors_and_diagnostics_never_echo_remote_secret() {
    for contract in [EventContract::LegacyV0_3_21, EventContract::CandidateV1Db7818e] {
        check_refused_acks(
            contract,
            vec![json!({"ok":false,"error":format!("refused-{SECRET_A}"),"data":{"client_id":"c7","client_capability":SECRET_B}}).to_string()],
        );
    }
}

#[test]
fn eof_before_ack_refuses_and_releases_event_sender() {
    let (_, diagnostics) = run(async {
        for contract in [EventContract::LegacyV0_3_21, EventContract::CandidateV1Db7818e] {
            let endpoint = Endpoint::new();
            let listener = endpoint.bind();
            let server = tokio::spawn(async move {
                let mut socket = accept_control(&listener, contract).await;
                assert_eq!(read_request(&mut socket).await, json!({"op":"events_subscribe"}));
            });
            let (tx, mut rx) = mpsc::channel(1);
            let client = endpoint.client();
            assert!(client.subscribe_events_for_contract(contract, tx).await.is_err());
            assert!(rx.recv().await.is_none());
            server.await.unwrap();
        }
    });
    assert_secret_safe(&diagnostics);
}

fn check_channel_round_trip(contract: EventContract, ack: Value, expected: Value) {
    let (_, diagnostics) = run(async {
        let endpoint = Endpoint::new();
        let listener = endpoint.bind();
        let inbound = json!({"kind":"channel_inbound","network":"fixture-network","from":"fixture-peer","channel":"fixture-channel","payload":{"message":"owned inbound"}});
        let expected_inbound = inbound.clone();
        let server = tokio::spawn(async move {
            let mut events = accept_events(&listener, &ack, contract).await;
            let mut command = accept_control(&listener, contract).await;
            assert_eq!(read_request(&mut command).await, expected);
            write_json(command.get_mut(), &json!({"ok":true})).await;
            write_json(events.get_mut(), &inbound).await;
            assert_eof(&mut command).await;
            assert_eof(&mut events).await;
        });
        let client = endpoint.client();
        let (tx, mut rx) = mpsc::channel(1);
        let session = client.subscribe_events_for_contract(contract, tx).await.unwrap();
        let response = client.subscribe_channel(&session.registration(), "fixture-network", "fixture-channel").await.unwrap();
        assert!(response.ok);
        assert_eq!(rx.recv().await.unwrap(), expected_inbound);
        drop(session);
        server.await.unwrap();
        assert!(rx.recv().await.is_none());
    });
    assert_secret_safe(&diagnostics);
}

#[test]
fn candidate_channel_subscribe_carries_capability_and_routes_inbound() {
    check_channel_round_trip(
        EventContract::CandidateV1Db7818e,
        candidate_ack("c7", SECRET_A),
        json!({"op":"channel_subscribe","client_id":"c7","client_capability":SECRET_A,"network":"fixture-network","channel":"fixture-channel"}),
    );
}

#[test]
fn legacy_channel_subscribe_omits_capability_and_routes_inbound() {
    check_channel_round_trip(
        EventContract::LegacyV0_3_21,
        json!({"ok":true,"data":{"client_id":"c7"}}),
        json!({"op":"channel_subscribe","client_id":"c7","network":"fixture-network","channel":"fixture-channel"}),
    );
}

#[test]
fn malformed_event_diagnostics_redact_secret_and_keep_valid_inbound() {
    let (_, diagnostics) = run(async {
        let endpoint = Endpoint::new();
        let listener = endpoint.bind();
        let inbound = json!({"kind":"channel_inbound","network":"fixture-network","from":"fixture-peer","channel":"fixture-channel","payload":17});
        let expected = inbound.clone();
        let server = tokio::spawn(async move {
            let mut socket = accept_events(&listener, &candidate_ack("c7", SECRET_A), EventContract::CandidateV1Db7818e).await;
            write_line(socket.get_mut(), &format!("{{\"client_capability\":\"{SECRET_A}\",broken-{SECRET_B}")).await;
            write_json(socket.get_mut(), &inbound).await;
            assert_eof(&mut socket).await;
        });
        let (tx, mut rx) = mpsc::channel(1);
        let client = endpoint.client();
        let session = client.subscribe_events_for_contract(EventContract::CandidateV1Db7818e, tx).await.unwrap();
        assert_eq!(rx.recv().await.unwrap(), expected);
        drop(session);
        server.await.unwrap();
    });
    assert!(!diagnostics.is_empty(), "malformed input should retain a safe diagnostic");
    assert_secret_safe(&diagnostics);
}

#[test]
fn dropping_idle_receiver_closes_owned_event_socket() {
    let (_, diagnostics) = run(async {
        let endpoint = Endpoint::new();
        let listener = endpoint.bind();
        let server = tokio::spawn(async move {
            let mut socket = accept_events(&listener, &candidate_ack("c7", SECRET_A), EventContract::CandidateV1Db7818e).await;
            // No event is sent to wake the reader after its receiver disappears.
            assert_eof(&mut socket).await;
        });
        let client = endpoint.client();
        let (tx, rx) = mpsc::channel(1);
        let session = client.subscribe_events_for_contract(EventContract::CandidateV1Db7818e, tx).await.unwrap();
        let registration = session.registration();
        drop(rx);
        server.await.unwrap();
        assert!(client.subscribe_channel(&registration, "fixture-network", "fixture-channel").await.is_err());
        drop(session);
    });
    assert_secret_safe(&diagnostics);
}

#[test]
fn dropping_idle_session_closes_socket_and_invalidates_registration() {
    let (_, diagnostics) = run(async {
        let endpoint = Endpoint::new();
        let listener = endpoint.bind();
        let server = tokio::spawn(async move {
            let mut socket = accept_events(&listener, &candidate_ack("c7", SECRET_A), EventContract::CandidateV1Db7818e).await;
            assert_eof(&mut socket).await;
        });
        let client = endpoint.client();
        let (tx, mut rx) = mpsc::channel(1);
        let session = client.subscribe_events_for_contract(EventContract::CandidateV1Db7818e, tx).await.unwrap();
        let registration = session.registration();
        drop(session);
        assert!(client.subscribe_channel(&registration, "fixture-network", "fixture-channel").await.is_err());
        assert!(rx.recv().await.is_none());
        server.await.unwrap();
    });
    assert_secret_safe(&diagnostics);
}

#[test]
fn dropping_full_receiver_closes_owned_event_socket() {
    let (_, diagnostics) = run(async {
        let endpoint = Endpoint::new();
        let listener = endpoint.bind();
        let server = tokio::spawn(async move {
            let mut socket = accept_events(&listener, &candidate_ack("c7", SECRET_A), EventContract::CandidateV1Db7818e).await;
            for value in [1, 2, 3] {
                write_json(socket.get_mut(), &json!({"kind":"channel_inbound","network":"fixture-network","from":"fixture-peer","channel":"fixture-channel","payload":value})).await;
            }
            assert_eof(&mut socket).await;
        });
        let (tx, rx) = mpsc::channel(1);
        let client = endpoint.client();
        let session = client.subscribe_events_for_contract(EventContract::CandidateV1Db7818e, tx).await.unwrap();
        while rx.len() != 1 {
            tokio::task::yield_now().await;
        }
        drop(rx);
        server.await.unwrap();
        drop(session);
    });
    assert_secret_safe(&diagnostics);
}

#[test]
fn daemon_eof_closes_receiver_and_invalidates_registration() {
    let (_, diagnostics) = run(async {
        let endpoint = Endpoint::new();
        let listener = endpoint.bind();
        let server = tokio::spawn(async move {
            let _socket = accept_events(&listener, &candidate_ack("c7", SECRET_A), EventContract::CandidateV1Db7818e).await;
        });
        let client = endpoint.client();
        let (tx, mut rx) = mpsc::channel(1);
        let session = client.subscribe_events_for_contract(EventContract::CandidateV1Db7818e, tx).await.unwrap();
        let registration = session.registration();
        assert!(rx.recv().await.is_none());
        server.await.unwrap();
        assert!(client.subscribe_channel(&registration, "fixture-network", "fixture-channel").await.is_err());
        drop(session);
    });
    assert_secret_safe(&diagnostics);
}

#[test]
fn reconnect_renews_generation_and_refuses_stale_before_write() {
    let (_, diagnostics) = run(async {
        let endpoint = Endpoint::new();
        let listener = endpoint.bind();
        let server = tokio::spawn(async move {
            let mut first = accept_events(&listener, &candidate_ack("c7", SECRET_A), EventContract::CandidateV1Db7818e).await;
            let mut second = accept_events(&listener, &candidate_ack("c7", SECRET_B), EventContract::CandidateV1Db7818e).await;
            assert_eof(&mut first).await;
            // The next accepted command must be the live registration. A stale
            // write (or connect) before it causes this independent wire check to fail.
            let mut command = BufReader::new(listener.accept().await.unwrap());
            assert_eq!(read_request(&mut command).await, json!({"op":"channel_subscribe","client_id":"c7","client_capability":SECRET_B,"network":"fixture-network","channel":"fixture-channel"}));
            write_json(command.get_mut(), &json!({"ok":true})).await;
            assert_eof(&mut command).await;
            assert_eof(&mut second).await;
        });
        let client = endpoint.client();
        let (tx_a, mut rx_a) = mpsc::channel(1);
        let first = client.subscribe_events_for_contract(EventContract::CandidateV1Db7818e, tx_a).await.unwrap();
        let old = first.registration();
        let (tx_b, _rx_b) = mpsc::channel(1);
        let second = client.subscribe_events_for_contract(EventContract::CandidateV1Db7818e, tx_b).await.unwrap();
        let current = second.registration();
        assert_ne!(current.generation(), old.generation());
        assert_eq!(current.client_id(), old.client_id(), "daemon restart can reuse its numeric id");
        assert!(rx_a.recv().await.is_none());
        // A retained old session may be dropped after its successor is live.
        // Its cleanup must not invalidate the successor's registration.
        drop(first);
        assert!(client.subscribe_channel(&old, "fixture-network", "fixture-channel").await.is_err());
        assert!(client.subscribe_channel(&current, "fixture-network", "fixture-channel").await.unwrap().ok);
        drop(second);
        server.await.unwrap();
    });
    assert_secret_safe(&diagnostics);
}

#[test]
fn another_client_registration_refuses_before_write() {
    let (_, diagnostics) = run(async {
        let endpoint = Endpoint::new();
        let listener = endpoint.bind();
        let server = tokio::spawn(async move {
            let mut first = accept_events(&listener, &candidate_ack("c7", SECRET_A), EventContract::CandidateV1Db7818e).await;
            let mut second = accept_events(&listener, &candidate_ack("c8", SECRET_B), EventContract::CandidateV1Db7818e).await;
            let mut command = BufReader::new(listener.accept().await.unwrap());
            assert_eq!(read_request(&mut command).await, json!({"op":"channel_subscribe","client_id":"c7","client_capability":SECRET_A,"network":"fixture-network","channel":"fixture-channel"}));
            write_json(command.get_mut(), &json!({"ok":true})).await;
            assert_eof(&mut command).await;
            assert_eof(&mut first).await;
            assert_eof(&mut second).await;
        });
        let client_a = endpoint.client();
        let client_b = endpoint.client();
        let (tx_a, _rx_a) = mpsc::channel(1);
        let (tx_b, _rx_b) = mpsc::channel(1);
        let session_a = client_a.subscribe_events_for_contract(EventContract::CandidateV1Db7818e, tx_a).await.unwrap();
        let session_b = client_b.subscribe_events_for_contract(EventContract::CandidateV1Db7818e, tx_b).await.unwrap();
        let registration = session_a.registration();
        assert!(client_b.subscribe_channel(&registration, "fixture-network", "fixture-channel").await.is_err());
        assert!(client_a.subscribe_channel(&registration, "fixture-network", "fixture-channel").await.unwrap().ok);
        drop(session_a);
        drop(session_b);
        server.await.unwrap();
    });
    assert_secret_safe(&diagnostics);
}

#[test]
fn cancelled_subscription_before_ack_closes_its_socket() {
    let (_, diagnostics) = run(async {
        let endpoint = Endpoint::new();
        let listener = endpoint.bind();
        let (request_seen_tx, request_seen_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let mut socket = BufReader::new(listener.accept().await.unwrap());
            assert_eq!(read_request(&mut socket).await, json!({"op":"events_subscribe"}));
            request_seen_tx.send(()).unwrap();
            // Do not ACK: the pending caller must close the connection itself.
            assert_eof(&mut socket).await;
        });
        let client = endpoint.client();
        let (tx, mut rx) = mpsc::channel(1);
        let pending = tokio::spawn(async move {
            client.subscribe_events_for_contract(EventContract::CandidateV1Db7818e, tx).await
        });
        request_seen_rx.await.unwrap();
        pending.abort();
        assert!(pending.await.unwrap_err().is_cancelled());
        assert!(rx.recv().await.is_none());
        server.await.unwrap();
    });
    assert_secret_safe(&diagnostics);
}
