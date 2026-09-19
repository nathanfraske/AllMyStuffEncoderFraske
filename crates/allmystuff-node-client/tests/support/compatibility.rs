//! Included by lib.rs only under cfg(test), to inject private fixture addresses.
//! This module never calls new(), probe(), or the production address resolver.

#[rustfmt::skip]
#[path = "../baseline/node_client.rs"]
mod old_node;
#[rustfmt::skip]
#[path = "../baseline/terminal_client.rs"]
mod old_terminal;

use std::future::Future;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use interprocess::local_socket::tokio::prelude::*;
use interprocess::local_socket::tokio::Listener;
#[cfg(unix)]
use interprocess::local_socket::GenericFilePath;
#[cfg(not(unix))]
use interprocess::local_socket::GenericNamespaced;
use interprocess::local_socket::ListenerOptions;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

const REQUEST: &[u8] = br#"{"cmd":"fixture","args":{"n":7}}"#;
const SUBSCRIBE: &[u8] = br#"{"cmd":"__subscribe_events","args":null}"#;
const ACK: &[u8] = br#"{"ok":true}"#;
const DEADLINE: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug)]
enum Flavor {
    OldNode,
    Node,
    OldTerminal,
    Terminal,
}
impl Flavor {
    fn node(self) -> bool {
        matches!(self, Self::OldNode | Self::Node)
    }
}
const PAIRS: [(Flavor, Flavor); 2] = [
    (Flavor::OldNode, Flavor::Node),
    (Flavor::OldTerminal, Flavor::Terminal),
];
const ALL: [Flavor; 4] = [
    Flavor::OldNode,
    Flavor::Node,
    Flavor::OldTerminal,
    Flavor::Terminal,
];

struct Endpoint {
    _directory: tempfile::TempDir,
    #[cfg(unix)]
    path: std::path::PathBuf,
    #[cfg(not(unix))]
    name: String,
}

impl Endpoint {
    fn new() -> Self {
        let directory = tempfile::Builder::new()
            .prefix("ams-ipc-fixture-")
            .tempdir()
            .unwrap();
        #[cfg(unix)]
        let path = directory.path().join("fixture.sock");
        #[cfg(not(unix))]
        let name = format!(
            "ams-ipc-fixture-{}-{}",
            std::process::id(),
            directory.path().file_name().unwrap().to_string_lossy()
        );
        Self {
            _directory: directory,
            #[cfg(unix)]
            path,
            #[cfg(not(unix))]
            name,
        }
    }

    fn address(&self) -> crate::address::SocketAddr {
        #[cfg(unix)]
        {
            crate::address::SocketAddr::Path(self.path.clone())
        }
        #[cfg(not(unix))]
        {
            crate::address::SocketAddr::Name(self.name.clone())
        }
    }

    fn bind(&self) -> Listener {
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
            std::fs::set_permissions(
                self._directory.path(),
                std::fs::Permissions::from_mode(0o700),
            )
            .unwrap();
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        listener
    }

    fn client(&self, flavor: Flavor) -> Client {
        match flavor {
            Flavor::Node => Client::Node(crate::NodeClient::for_test_address(self.address())),
            Flavor::Terminal => Client::Terminal(crate::terminal::NodeClient::for_test_address(
                self.address(),
            )),
            Flavor::OldNode => {
                #[cfg(unix)]
                let address = self.path.clone();
                #[cfg(not(unix))]
                let address = self.name.clone();
                Client::OldNode(old_node::NodeClient::for_fixture(address))
            }
            Flavor::OldTerminal => {
                #[cfg(unix)]
                let address = self.path.clone();
                #[cfg(not(unix))]
                let address = self.name.clone();
                Client::OldTerminal(old_terminal::NodeClient::for_fixture(address))
            }
        }
    }

    fn make_invalid(&mut self) {
        #[cfg(unix)]
        {
            self.path = self._directory.path().join("invalid\0fixture");
        }
        #[cfg(not(unix))]
        {
            self.name = "ams-ipc-fixture-invalid\0name".into();
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct ErrorView {
    display: String,
    chain: Vec<String>,
    io_kind: Option<io::ErrorKind>,
    json_cause: Option<(String, usize, usize)>,
}
impl ErrorView {
    fn node(error: anyhow::Error) -> Self {
        Self {
            display: error.to_string(),
            chain: error.chain().map(ToString::to_string).collect(),
            io_kind: error.downcast_ref::<io::Error>().map(io::Error::kind),
            json_cause: error
                .downcast_ref::<serde_json::Error>()
                .map(|e| (format!("{:?}", e.classify()), e.line(), e.column())),
        }
    }
    fn terminal(error: String) -> Self {
        Self {
            display: error.clone(),
            chain: vec![error],
            io_kind: None,
            json_cause: None,
        }
    }
}

#[derive(Debug, PartialEq)]
enum Outcome {
    Json(Value),
    Bytes(Vec<u8>),
    Error(ErrorView),
}

enum Client {
    OldNode(old_node::NodeClient),
    Node(crate::NodeClient),
    OldTerminal(old_terminal::NodeClient),
    Terminal(crate::terminal::NodeClient),
}

macro_rules! request_outcome {
    ($client:expr, $raw:expr, $error:path) => {
        if $raw {
            $client.request_bytes("fixture", json!({"n": 7})).await
                .map(Outcome::Bytes).unwrap_or_else(|e| Outcome::Error($error(e)))
        } else {
            $client.request("fixture", json!({"n": 7})).await
                .map(Outcome::Json).unwrap_or_else(|e| Outcome::Error($error(e)))
        }
    };
}

impl Client {
    async fn request(&self, raw: bool) -> Outcome {
        match self {
            Self::OldNode(c) => request_outcome!(c, raw, ErrorView::node),
            Self::Node(c) => request_outcome!(c, raw, ErrorView::node),
            Self::OldTerminal(c) => request_outcome!(c, raw, ErrorView::terminal),
            Self::Terminal(c) => request_outcome!(c, raw, ErrorView::terminal),
        }
    }

    async fn subscribe(&self, capacity: usize) -> (Result<(), ErrorView>, Events) {
        match self {
            Self::OldNode(c) => {
                let (tx, rx) = mpsc::channel(capacity);
                (
                    c.subscribe_events(tx).await.map_err(ErrorView::node),
                    Events::OldNode(rx),
                )
            }
            Self::Node(c) => {
                let (tx, rx) = mpsc::channel(capacity);
                (
                    c.subscribe_events(tx).await.map_err(ErrorView::node),
                    Events::New(rx),
                )
            }
            Self::OldTerminal(c) => {
                let (tx, rx) = mpsc::channel(capacity);
                (
                    c.subscribe_events(tx).await.map_err(ErrorView::terminal),
                    Events::OldTerminal(rx),
                )
            }
            Self::Terminal(c) => {
                let (tx, rx) = mpsc::channel(capacity);
                (
                    c.subscribe_events(tx).await.map_err(ErrorView::terminal),
                    Events::New(rx),
                )
            }
        }
    }
}

enum Events {
    OldNode(mpsc::Receiver<old_node::NodeEvent>),
    OldTerminal(mpsc::Receiver<old_terminal::NodeEvent>),
    New(mpsc::Receiver<crate::NodeEvent>),
}
impl Events {
    async fn recv(&mut self) -> Option<Value> {
        match self {
            Self::OldNode(rx) => rx.recv().await.map(|e| match e {
                old_node::NodeEvent::Emit { event, payload } => {
                    json!({"kind":"emit","event":event,"payload":payload})
                }
                old_node::NodeEvent::Upgrade => json!({"kind":"upgrade"}),
                old_node::NodeEvent::Restart => json!({"kind":"restart"}),
            }),
            Self::OldTerminal(rx) => rx.recv().await.map(|e| match e {
                old_terminal::NodeEvent::Emit { event, payload } => {
                    json!({"kind":"emit","event":event,"payload":payload})
                }
                old_terminal::NodeEvent::Upgrade => json!({"kind":"upgrade"}),
                old_terminal::NodeEvent::Restart => json!({"kind":"restart"}),
            }),
            Self::New(rx) => rx.recv().await.map(|e| serde_json::to_value(e).unwrap()),
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::OldNode(rx) => rx.len(),
            Self::OldTerminal(rx) => rx.len(),
            Self::New(rx) => rx.len(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Trace {
    target: String,
    level: String,
    fields: Vec<(String, String)>,
}
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<Trace>>>);
struct Fields(Vec<(String, String)>);
impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push((field.name().into(), format!("{value:?}")));
    }
}
impl tracing::Subscriber for Capture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        if *event.metadata().level() != tracing::Level::WARN {
            return;
        }
        let mut fields = Fields(Vec::new());
        event.record(&mut fields);
        self.0.lock().unwrap().push(Trace {
            target: event.metadata().target().into(),
            level: event.metadata().level().to_string(),
            fields: fields.0,
        });
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

fn run<T>(future: impl Future<Output = T>) -> (T, Vec<Trace>) {
    // A current-thread runtime keeps spawned reader polls under this dispatcher.
    // Dropping it also aborts any owned reader if a failed assertion unwinds.
    let capture = Capture::default();
    let result = tracing::subscriber::with_default(capture.clone(), || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            tokio::time::timeout(DEADLINE, future)
                .await
                .expect("private IPC fixture timed out")
        })
    });
    let traces = capture.0.lock().unwrap().clone();
    (result, traces)
}

// A tiny independent peer codec: no extracted frame/type/constant is used.
fn frame(tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut bytes = ((payload.len() + 1) as u32).to_be_bytes().to_vec();
    bytes.push(tag);
    bytes.extend_from_slice(payload);
    bytes
}
async fn read_request(stream: &mut LocalSocketStream, expected: &[u8]) {
    let mut length = [0; 4];
    stream.read_exact(&mut length).await.unwrap();
    assert_eq!(u32::from_be_bytes(length) as usize, expected.len() + 1);
    let mut body = vec![0; expected.len() + 1];
    stream.read_exact(&mut body).await.unwrap();
    assert_eq!(body[0], 0);
    assert_eq!(&body[1..], expected);
}
async fn send(stream: &mut LocalSocketStream, bytes: &[u8]) {
    stream.write_all(bytes).await.unwrap();
    stream.flush().await.unwrap();
}
async fn closed(stream: &mut LocalSocketStream) {
    let mut byte = [0; 1];
    match stream.read(&mut byte).await {
        Ok(0) => (),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::BrokenPipe
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::NotConnected
            ) =>
        {
            ()
        }
        other => panic!("expected the private connection to close, got {other:?}"),
    }
}
async fn stays_open(stream: &mut LocalSocketStream) {
    let mut byte = [0; 1];
    assert!(
        tokio::time::timeout(Duration::from_millis(40), stream.read(&mut byte))
            .await
            .is_err(),
        "writer half closed while subscription was still active"
    );
}

enum Expected {
    Json(Value),
    Bytes(Vec<u8>),
    Message(&'static str, &'static str),
    Context(&'static str),
}
impl Expected {
    fn check(&self, outcome: &Outcome, flavor: Flavor) {
        match (self, outcome) {
            (Self::Json(value), Outcome::Json(got)) => assert_eq!(got, value),
            (Self::Bytes(value), Outcome::Bytes(got)) => assert_eq!(got, value),
            (Self::Message(node, terminal), Outcome::Error(got)) => {
                assert_eq!(got.display, if flavor.node() { *node } else { *terminal });
                assert_eq!(got.chain, vec![got.display.clone()]);
            }
            (Self::Context(context), Outcome::Error(got)) => {
                if flavor.node() {
                    assert_eq!(got.display, *context);
                    assert!(got.chain.len() >= 2);
                } else {
                    assert!(got.display.starts_with(&format!("{context}: ")), "{got:?}");
                    assert_eq!(got.chain, vec![got.display.clone()]);
                }
            }
            _ => panic!("unexpected outcome for {flavor:?}: {outcome:?}"),
        }
    }
}

#[test]
fn request_response_matrix_matches_old_clients_and_fixed_expectations() {
    let mut cases: Vec<(&str, bool, Vec<u8>, Expected)> = vec![
        (
            "default null",
            false,
            frame(0, br#"{"ok":true}"#),
            Expected::Json(Value::Null),
        ),
        (
            "explicit nulls",
            false,
            frame(0, br#"{"ok":true,"result":null,"error":null}"#),
            Expected::Json(Value::Null),
        ),
        (
            "success ignores error",
            false,
            frame(
                0,
                br#"{"ok":true,"result":[1,false],"error":"ignored","extra":7}"#,
            ),
            Expected::Json(json!([1, false])),
        ),
        (
            "failure reason",
            false,
            frame(0, br#"{"ok":false,"result":17,"error":"denied"}"#),
            Expected::Message("denied", "denied"),
        ),
        (
            "failure default",
            false,
            frame(0, br#"{"ok":false}"#),
            Expected::Message("(no error)", "(no error)"),
        ),
        (
            "failure null",
            false,
            frame(0, br#"{"ok":false,"error":null}"#),
            Expected::Message("(no error)", "(no error)"),
        ),
        (
            "binary",
            true,
            frame(1, &[0, 10, 255, 1]),
            Expected::Bytes(vec![0, 10, 255, 1]),
        ),
        ("empty binary", true, frame(1, &[]), Expected::Bytes(vec![])),
        (
            "raw JSON error",
            true,
            frame(0, br#"{"ok":false,"error":"poll denied"}"#),
            Expected::Message("poll denied", "poll denied"),
        ),
        (
            "raw success still uses error",
            true,
            frame(0, br#"{"ok":true,"error":"still returned"}"#),
            Expected::Message("still returned", "still returned"),
        ),
        (
            "raw JSON fallback",
            true,
            frame(0, br#"{"ok":true,"result":[1,2]}"#),
            Expected::Message(
                "node returned JSON where bytes were expected",
                "node returned JSON where bytes were expected",
            ),
        ),
        (
            "wrong JSON tag",
            false,
            frame(1, b"raw"),
            Expected::Message(
                "node sent a 1 frame where a JSON response was expected",
                "node sent a 1 frame where JSON was expected",
            ),
        ),
        (
            "wrong byte tag",
            true,
            frame(9, b"raw"),
            Expected::Message(
                "node sent a 9 frame where raw bytes were expected",
                "node sent a 9 frame where bytes were expected",
            ),
        ),
        (
            "zero frame",
            false,
            vec![0, 0, 0, 0],
            Expected::Context("read node response"),
        ),
        (
            "oversize",
            false,
            vec![16, 0, 0, 1],
            Expected::Context("read node response"),
        ),
        (
            "truncated payload",
            false,
            vec![0, 0, 0, 4, 0, b'{'],
            Expected::Context("read node response"),
        ),
    ];
    for payload in [
        b"".as_slice(),
        b"{}",
        br#"{"ok":null}"#,
        br#"{"ok":true,"error":7}"#,
    ] {
        cases.push((
            "JSON parse/type failure",
            false,
            frame(0, payload),
            Expected::Context("parse node response"),
        ));
        cases.push((
            "raw JSON parse/type failure",
            true,
            frame(0, payload),
            Expected::Context("parse node response"),
        ));
    }
    for length in 0..4 {
        cases.push((
            "partial length EOF",
            false,
            vec![0; length],
            Expected::Message(
                "node closed the connection without a response",
                "node closed the connection without a response",
            ),
        ));
    }
    for (name, raw, reply, expected) in cases {
        for (old, new) in PAIRS {
            let mut outcomes = Vec::new();
            for flavor in [old, new] {
                let (outcome, traces) = run(async {
                    let endpoint = Endpoint::new();
                    let listener = endpoint.bind();
                    let client = endpoint.client(flavor);
                    let reply = reply.clone();
                    let peer = tokio::spawn(async move {
                        let mut stream = listener.accept().await.unwrap();
                        read_request(&mut stream, REQUEST).await;
                        send(&mut stream, &reply).await;
                    });
                    let outcome = client.request(raw).await;
                    peer.await.unwrap();
                    outcome
                });
                expected.check(&outcome, flavor);
                assert!(traces.is_empty(), "{name}: {traces:?}");
                outcomes.push(outcome);
            }
            assert_eq!(outcomes[0], outcomes[1], "{name}: {old:?} vs {new:?}");
        }
    }
}

#[test]
fn acknowledgement_matrix_matches_old_clients() {
    let mut cases = vec![
        (frame(0, ACK), Expected::Json(Value::Null)),
        (
            frame(0, br#"{"ok":true,"error":"ignored","result":null}"#),
            Expected::Json(Value::Null),
        ),
        (
            frame(0, br#"{"ok":false,"error":"no events"}"#),
            Expected::Message(
                "subscribe rejected: no events",
                "subscribe rejected: no events",
            ),
        ),
        (
            frame(0, br#"{"ok":false}"#),
            Expected::Message(
                "subscribe rejected: (no error)",
                "subscribe rejected: (no error)",
            ),
        ),
        (
            frame(1, b"{}"),
            Expected::Message(
                "subscribe ack wasn't a JSON frame",
                "subscribe ack wasn't a JSON frame",
            ),
        ),
        (frame(0, b"{}"), Expected::Context("parse subscribe ack")),
        (frame(0, b"{"), Expected::Context("parse subscribe ack")),
        (vec![0, 0, 0, 0], Expected::Context("read subscribe ack")),
        (vec![0, 0, 0, 2, 0], Expected::Context("read subscribe ack")),
    ];
    for length in 0..4 {
        cases.push((
            vec![0; length],
            Expected::Message(
                "node closed the connection before the subscribe ack",
                "node closed the connection before the subscribe ack",
            ),
        ));
    }
    for (reply, expected) in cases {
        for (old, new) in PAIRS {
            let mut outcomes = Vec::new();
            for flavor in [old, new] {
                let (outcome, traces) = run(async {
                    let endpoint = Endpoint::new();
                    let listener = endpoint.bind();
                    let client = endpoint.client(flavor);
                    let reply = reply.clone();
                    let peer = tokio::spawn(async move {
                        let mut stream = listener.accept().await.unwrap();
                        read_request(&mut stream, SUBSCRIBE).await;
                        send(&mut stream, &reply).await;
                    });
                    let (result, mut events) = client.subscribe(1).await;
                    assert_eq!(events.recv().await, None);
                    peer.await.unwrap();
                    match result {
                        Ok(()) => Outcome::Json(Value::Null),
                        Err(e) => Outcome::Error(e),
                    }
                });
                expected.check(&outcome, flavor);
                assert!(traces.is_empty(), "{traces:?}");
                outcomes.push(outcome);
            }
            assert_eq!(outcomes[0], outcomes[1]);
        }
    }
}

#[test]
fn connection_and_name_error_chains_remain_distinct() {
    for invalid in [false, true] {
        for (old, new) in PAIRS {
            let (outcomes, traces) = run(async {
                let mut endpoint = Endpoint::new();
                if invalid {
                    endpoint.make_invalid();
                }
                let old = endpoint.client(old).request(false).await;
                let new = endpoint.client(new).request(false).await;
                (old, new)
            });
            assert_eq!(outcomes.0, outcomes.1);
            let Outcome::Error(error) = outcomes.1 else {
                panic!("unbound private endpoint connected")
            };
            if new.node() {
                assert!(error.chain.len() >= 2);
            } else {
                assert_eq!(error.chain.len(), 1);
            }
            assert!(traces.is_empty());
        }
    }
}

#[test]
fn every_request_uses_a_fresh_connection_and_closes_the_old_one() {
    for flavor in ALL {
        run(async {
            let endpoint = Endpoint::new();
            let listener = endpoint.bind();
            let client = endpoint.client(flavor);
            let peer = tokio::spawn(async move {
                for _ in 0..2 {
                    let mut stream = listener.accept().await.unwrap();
                    read_request(&mut stream, REQUEST).await;
                    send(&mut stream, &frame(0, ACK)).await;
                    closed(&mut stream).await;
                }
            });
            assert_eq!(client.request(false).await, Outcome::Json(Value::Null));
            assert_eq!(client.request(false).await, Outcome::Json(Value::Null));
            peer.await.unwrap();
        });
    }
}

#[test]
fn event_policy_and_restart_semantics_match_the_original_readers() {
    let stream_frames = [
        frame(2, br#"{"kind":"emit","event":"fixture","payload":null}"#),
        frame(2, b"{bad"),
        frame(0, ACK),
        frame(255, b"unknown"),
        frame(2, br#"{"kind":"future_kind"}"#),
        frame(2, br#"{"kind":"restart"}"#),
        frame(2, br#"{"kind":"upgrade"}"#),
        frame(3, b"not JSON and intentionally ignored"),
    ]
    .concat();
    let expected = vec![
        json!({"kind":"emit","event":"fixture","payload":null}),
        json!({"kind":"restart"}),
        json!({"kind":"upgrade"}),
        json!({"kind":"restart"}),
    ];
    for (old, new) in PAIRS {
        let mut observations = Vec::new();
        for flavor in [old, new] {
            let (events, traces) = run(async {
                let endpoint = Endpoint::new();
                let listener = endpoint.bind();
                let client = endpoint.client(flavor);
                let bytes = [frame(0, ACK), stream_frames.clone()].concat();
                let peer = tokio::spawn(async move {
                    let mut stream = listener.accept().await.unwrap();
                    read_request(&mut stream, SUBSCRIBE).await;
                    send(&mut stream, &bytes).await;
                    // Keep the peer open: closure must come from tag-3 handling,
                    // not merely from the peer reaching EOF after this script.
                    closed(&mut stream).await;
                });
                let (ack, mut rx) = client.subscribe(1).await;
                ack.unwrap();
                let mut events = Vec::new();
                while let Some(event) = rx.recv().await {
                    events.push(event);
                }
                peer.await.unwrap();
                events
            });
            assert_eq!(events, expected);
            if flavor.node() {
                assert_eq!(traces.len(), 4);
                assert!(traces[0].fields[0].1.starts_with("malformed node event: "));
                assert_eq!(traces[1].fields[0].1, "unexpected node event frame tag 0");
                assert_eq!(traces[2].fields[0].1, "unexpected node event frame tag 255");
                assert!(traces[3].fields[0].1.starts_with("malformed node event: "));
                for trace in &traces {
                    assert_eq!(trace.level, "WARN");
                    assert_eq!(trace.fields.len(), 1);
                    assert_eq!(trace.fields[0].0, "message");
                    if matches!(flavor, Flavor::Node) {
                        assert_eq!(trace.target, "allmystuff_node::node_control");
                    }
                }
            } else {
                assert!(traces.is_empty());
            }
            observations.push(
                traces
                    .into_iter()
                    .map(|t| (t.level, t.fields))
                    .collect::<Vec<_>>(),
            );
        }
        assert_eq!(observations[0], observations[1]);
    }
}

#[test]
fn event_read_failures_warn_only_for_node_and_partial_length_eof_is_quiet() {
    for (suffix, warning) in [
        (vec![0, 0, 0, 0], true),
        (vec![0, 0, 0, 3, 2, b'{'], true),
        (vec![0, 0], false),
    ] {
        for (old, new) in PAIRS {
            let mut observations = Vec::new();
            for flavor in [old, new] {
                let (_, traces) = run(async {
                    let endpoint = Endpoint::new();
                    let listener = endpoint.bind();
                    let client = endpoint.client(flavor);
                    let bytes = [frame(0, ACK), suffix.clone()].concat();
                    let peer = tokio::spawn(async move {
                        let mut stream = listener.accept().await.unwrap();
                        read_request(&mut stream, SUBSCRIBE).await;
                        send(&mut stream, &bytes).await;
                    });
                    let (ack, mut rx) = client.subscribe(1).await;
                    ack.unwrap();
                    assert_eq!(rx.recv().await, None);
                    peer.await.unwrap();
                });
                assert_eq!(traces.len(), usize::from(warning && flavor.node()));
                for trace in &traces {
                    assert_eq!(trace.level, "WARN");
                    assert_eq!(trace.fields.len(), 1);
                    assert_eq!(trace.fields[0].0, "message");
                    assert!(trace.fields[0]
                        .1
                        .starts_with("node event stream read failed: "));
                    if matches!(flavor, Flavor::Node) {
                        assert_eq!(trace.target, "allmystuff_node::node_control");
                    }
                }
                observations.push(
                    traces
                        .into_iter()
                        .map(|t| (t.level, t.fields))
                        .collect::<Vec<_>>(),
                );
            }
            assert_eq!(observations[0], observations[1]);
        }
    }
}

#[test]
fn subscription_waits_for_ack_and_retains_writer_until_tag_restart() {
    for flavor in ALL {
        run(async {
            let endpoint = Endpoint::new();
            let listener = endpoint.bind();
            let client = endpoint.client(flavor);
            let (request_seen_tx, request_seen_rx) = tokio::sync::oneshot::channel();
            let (allow_ack_tx, allow_ack_rx) = tokio::sync::oneshot::channel();
            let (event_seen_tx, event_seen_rx) = tokio::sync::oneshot::channel();
            let peer = tokio::spawn(async move {
                let mut stream = listener.accept().await.unwrap();
                read_request(&mut stream, SUBSCRIBE).await;
                request_seen_tx.send(()).unwrap();
                allow_ack_rx.await.unwrap();
                send(&mut stream, &frame(0, ACK)).await;
                send(&mut stream, &frame(2, br#"{"kind":"upgrade"}"#)).await;
                event_seen_rx.await.unwrap();
                stays_open(&mut stream).await;
                send(&mut stream, &frame(3, b"ignored")).await;
                closed(&mut stream).await;
            });
            let subscribed = client.subscribe(1);
            tokio::pin!(subscribed);
            tokio::select! {
                _ = &mut subscribed => panic!("subscription completed before its acknowledgement"),
                _ = request_seen_rx => (),
            }
            assert!(
                tokio::time::timeout(Duration::from_millis(40), &mut subscribed)
                    .await
                    .is_err()
            );
            allow_ack_tx.send(()).unwrap();
            let (ack, mut rx) = subscribed.await;
            ack.unwrap();
            assert_eq!(rx.recv().await, Some(json!({"kind":"upgrade"})));
            event_seen_tx.send(()).unwrap();
            assert_eq!(rx.recv().await, Some(json!({"kind":"restart"})));
            assert_eq!(rx.recv().await, None);
            peer.await.unwrap();
        });
    }
}

#[test]
fn bounded_channel_backpressure_keeps_order_and_receiver_drop_closes_the_reader() {
    for flavor in ALL {
        for drop_receiver in [false, true] {
            run(async {
                let endpoint = Endpoint::new();
                let listener = endpoint.bind();
                let client = endpoint.client(flavor);
                let (checked_tx, checked_rx) = tokio::sync::oneshot::channel();
                let (queue_full_tx, queue_full_rx) = tokio::sync::oneshot::channel();
                let peer = tokio::spawn(async move {
                    let mut stream = listener.accept().await.unwrap();
                    read_request(&mut stream, SUBSCRIBE).await;
                    let bytes = [
                        frame(0, ACK),
                        frame(2, br#"{"kind":"emit","event":"fixture","payload":1}"#),
                        frame(2, br#"{"kind":"emit","event":"fixture","payload":2}"#),
                        frame(2, br#"{"kind":"emit","event":"fixture","payload":3}"#),
                        frame(3, b""),
                    ]
                    .concat();
                    send(&mut stream, &bytes).await;
                    queue_full_rx.await.unwrap();
                    stays_open(&mut stream).await;
                    checked_tx.send(()).unwrap();
                    closed(&mut stream).await;
                });
                let (ack, mut rx) = client.subscribe(1).await;
                ack.unwrap();
                while rx.len() == 0 {
                    tokio::task::yield_now().await;
                }
                assert_eq!(rx.len(), 1);
                queue_full_tx.send(()).unwrap();
                checked_rx.await.unwrap();
                if drop_receiver {
                    drop(rx);
                } else {
                    for value in 1..=3 {
                        assert_eq!(
                            rx.recv().await,
                            Some(json!({"kind":"emit","event":"fixture","payload":value}))
                        );
                    }
                    assert_eq!(rx.recv().await, Some(json!({"kind":"restart"})));
                    assert_eq!(rx.recv().await, None);
                }
                peer.await.unwrap();
            });
        }
    }
}
