//! Independent contract fixtures; no daemon, process, device or ordinary state.
//!
//! Released Status shape: mrjeeves/MyOwnMesh v0.3.21,
//! crates/myownmesh/src/control.rs:942-959. Candidate Status shape:
//! nathanfraske/MyOwnMeshSecurityReview db7818e09fedd98899490347b86ac9bc9f97b59b,
//! crates/myownmesh/src/control/reply.rs:324-343 and control/wire.rs.
//! These package/version observations do not prove a binary's Git identity,
//! authorize an application, or grant access to a user's ledger.

use super::{
    assess_binary_version, assess_status, parse_binary_version_output, ContractReadiness,
    ContractRefusal, LegacyStatus,
};
use allmystuff_protocol::Response;
use serde_json::{json, Value};

fn released_status() -> Response {
    Response::ok(json!({
        "version": "0.3.21",
        "device_id": "fixture-device",
        "joined_networks": ["fixture-network"],
        "media_lanes": 2,
        "media_pipes": true
    }))
}

fn candidate_status(supported: bool) -> Response {
    Response::ok(json!({
        "version": "1.0.0",
        "device_id": "fixture-device",
        "joined_networks": [],
        "realtime": {
            "supported": supported,
            "encodings": if supported {
                json!([
                    {"kind": "video", "mime": "video/H264", "clock_rate": 90000, "channels": 0},
                    {"kind": "audio", "mime": "audio/opus", "clock_rate": 48000, "channels": 2}
                ])
            } else {
                json!([])
            }
        }
    }))
}

fn set_field(response: &mut Response, field: &str, value: Value) {
    response.data.as_mut().unwrap()[field] = value;
}

fn remove_field(response: &mut Response, field: &str) {
    response
        .data
        .as_mut()
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove(field);
}

fn assert_refusal(response: &Response, expected: ContractRefusal) {
    assert_eq!(
        assess_status(response),
        ContractReadiness::Refused(expected.clone())
    );
    assert_eq!(assess_status(response).require_legacy(), Err(expected));
}

#[test]
fn release_status_accepts_only_explicit_pin_spellings() {
    for version in ["0.3.21", "v0.3.21", " \t0.3.21\n"] {
        let mut response = released_status();
        set_field(&mut response, "version", json!(version));
        assert_eq!(
            assess_status(&response).require_legacy(),
            Ok(LegacyStatus { media_pipes: true })
        );
        assert_eq!(assess_binary_version(version), Ok(()));
    }
}

#[test]
fn legacy_false_media_flag_retains_json_fallback() {
    let mut response = released_status();
    set_field(&mut response, "media_pipes", json!(false));
    set_field(&mut response, "joined_networks", json!([]));
    assert_eq!(
        assess_status(&response),
        ContractReadiness::ReadyLegacy(LegacyStatus { media_pipes: false })
    );
    assert_eq!(
        assess_status(&response).require_legacy(),
        Ok(LegacyStatus { media_pipes: false })
    );
}

#[test]
fn candidate_status_refuses_supported_and_unsupported_rtp_profiles() {
    for supported in [true, false] {
        assert_refusal(
            &candidate_status(supported),
            ContractRefusal::CandidateNotMigrated,
        );
    }
}

#[test]
fn candidate_version_refuses_legacy_lookalike_fields() {
    for version in ["1.0.0", "v1.0.0", " 1.0.0 "] {
        let mut response = released_status();
        set_field(&mut response, "version", json!(version));
        assert_refusal(&response, ContractRefusal::CandidateNotMigrated);
        assert_eq!(
            assess_binary_version(version),
            Err(ContractRefusal::CandidateNotMigrated)
        );
    }
    // Version recognition is not proof of a complete product/session adapter.
    let response = Response::ok(json!({"version": "1.0.0"}));
    assert_refusal(&response, ContractRefusal::CandidateNotMigrated);
}

#[test]
fn rejected_status_cannot_become_ready_from_legacy_data() {
    for error in [None, Some("fixture Status refusal".to_owned())] {
        let mut response = released_status();
        response.ok = false;
        response.error = error.clone();
        assert_refusal(&response, ContractRefusal::StatusRejected(error));
    }
    assert_refusal(
        &Response::err("fixture unavailable"),
        ContractRefusal::StatusRejected(Some("fixture unavailable".to_owned())),
    );
}

#[test]
fn contradictory_success_and_error_envelope_is_refused() {
    let mut response = released_status();
    response.error = Some("fixture contradictory error".to_owned());
    assert_refusal(
        &response,
        ContractRefusal::MalformedStatus("response envelope"),
    );
}

#[test]
fn status_data_must_be_present_and_an_object() {
    assert_refusal(
        &Response {
            ok: true,
            error: None,
            data: None,
        },
        ContractRefusal::MissingStatusData,
    );
    for data in [json!(null), json!(false), json!(7), json!("fixture"), json!([])] {
        assert_refusal(
            &Response::ok(data),
            ContractRefusal::MalformedStatus("data object"),
        );
    }
}

#[test]
fn status_version_must_be_present_and_a_string() {
    let mut missing = released_status();
    remove_field(&mut missing, "version");
    assert_refusal(&missing, ContractRefusal::MissingVersion);
    for version in [json!(null), json!(3), json!(false), json!([]), json!({})] {
        let mut response = released_status();
        set_field(&mut response, "version", version);
        assert_refusal(&response, ContractRefusal::MalformedStatus("version field"));
    }
}

#[test]
fn numeric_newer_and_older_versions_are_not_compatibility_matches() {
    for version in ["0.3.20", "0.3.22", "v0.3.22", "0.4.0", "1.0.1", "2.0.0"] {
        let mut response = released_status();
        set_field(&mut response, "version", json!(version));
        let refusal = ContractRefusal::UnsupportedVersion(version.to_owned());
        assert_refusal(&response, refusal.clone());
        assert_eq!(assess_binary_version(version), Err(refusal));
    }
}

#[test]
fn version_suffixes_aliases_and_overflow_are_not_collapsed() {
    for version in [
        "",
        " ",
        "0",
        "0.3",
        "0.3.21.0",
        "00.3.21",
        "0.03.21",
        "0.3.021",
        "V0.3.21",
        "vv0.3.21",
        "0.3.21-rc.1",
        "0.3.21+build",
        "0.3.21garbage",
        "1.0.0-rc.1",
        "1.0.0+db7818e",
        "18446744073709551616.3.21",
        "db7818e09fedd98899490347b86ac9bc9f97b59b",
        "myownmesh 0.3.21",
    ] {
        let mut response = released_status();
        set_field(&mut response, "version", json!(version));
        let refusal = ContractRefusal::MalformedVersion(version.trim().to_owned());
        assert_refusal(&response, refusal.clone());
        assert_eq!(assess_binary_version(version), Err(refusal));
    }
}

#[test]
fn released_status_requires_explicit_media_flag_without_guessing_from_lanes() {
    let mut missing = released_status();
    remove_field(&mut missing, "media_pipes");
    set_field(&mut missing, "media_lanes", json!(8));
    assert_refusal(
        &missing,
        ContractRefusal::MalformedStatus("media_pipes field"),
    );
    for value in [
        json!(null),
        json!(0),
        json!(1),
        json!("true"),
        json!([]),
        json!({}),
    ] {
        let mut response = released_status();
        set_field(&mut response, "media_pipes", value);
        assert_refusal(
            &response,
            ContractRefusal::MalformedStatus("media_pipes field"),
        );
    }
}

#[test]
fn released_status_requires_device_and_network_field_shapes() {
    for (field, malformed, reason) in [
        (
            "device_id",
            vec![json!(null), json!(""), json!(7), json!([])],
            "device_id field",
        ),
        (
            "joined_networks",
            vec![json!(null), json!("network"), json!(["network", 7]), json!({})],
            "joined_networks field",
        ),
    ] {
        let mut missing = released_status();
        remove_field(&mut missing, field);
        assert_refusal(&missing, ContractRefusal::MalformedStatus(reason));
        for value in malformed {
            let mut response = released_status();
            set_field(&mut response, field, value);
            assert_refusal(&response, ContractRefusal::MalformedStatus(reason));
        }
    }
}

#[test]
fn binary_output_parser_preserves_the_first_exact_version_token() {
    for (output, expected) in [
        ("myownmesh 0.3.21\n", "0.3.21"),
        ("myownmesh v0.3.21\r\n", "v0.3.21"),
        ("  myownmesh\t0.3.21  \n", "0.3.21"),
        ("myownmesh 1.0.0\nmyownmesh 0.3.21\n", "1.0.0"),
        ("myownmesh 0.3.21-rc.1\n", "0.3.21-rc.1"),
        ("myownmesh 0.3.21+build\n", "0.3.21+build"),
    ] {
        assert_eq!(parse_binary_version_output(output), Some(expected));
    }
}

#[test]
fn unrelated_or_malformed_binary_output_cannot_supply_readiness() {
    for output in [
        "",
        "0.3.21",
        "myownmesh",
        "other-daemon 0.3.21",
        "myownmesh 0.3.21 extra",
        "myownmesh --version 0.3.21",
        "error\nmyownmesh 0.3.21\n",
        "\nmyownmesh 0.3.21\n",
    ] {
        assert_eq!(parse_binary_version_output(output), None);
    }
}

// Each IPC case owns its endpoint. No default address, daemon binary, global
// environment, Mesh, device, installer or external service is used. The only
// lifecycle calls below supply occupied, refused endpoints; their source-
// reviewed early returns precede binary lookup, orphan handling and state I/O.
mod endpoints {
    use super::*;
    use crate::control_client::{ControlClient, MediaPipe, Request, SocketAddr};
    use crate::mesh_contract::EndpointProbe;
    use interprocess::local_socket::tokio::{prelude::*, Listener};
    #[cfg(unix)]
    use interprocess::local_socket::GenericFilePath;
    #[cfg(not(unix))]
    use interprocess::local_socket::GenericNamespaced;
    use interprocess::local_socket::ListenerOptions;
    use std::future::Future;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
    use tokio::task::JoinHandle;

    const DEADLINE: Duration = Duration::from_secs(10);
    const SENTINEL: &str = "fixture-private-invalid-json";
    static NEXT_ENDPOINT: AtomicU64 = AtomicU64::new(0);

    struct Endpoint {
        #[cfg(unix)]
        directory: std::path::PathBuf,
        #[cfg(unix)]
        path: std::path::PathBuf,
        #[cfg(not(unix))]
        name: String,
    }

    impl Endpoint {
        fn new() -> Self {
            let sequence = NEXT_ENDPOINT.fetch_add(1, Ordering::Relaxed);
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let label = format!("ams-contract-{}-{nonce:x}-{sequence:x}", std::process::id());
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                let directory = std::env::temp_dir().join(label);
                let path = directory.join("s");
                // macOS has the shortest supported native sun_path. Runners
                // must provide a short private TMPDIR rather than truncate or
                // share a name when their default temporary root is too long.
                use std::os::unix::ffi::OsStrExt;
                assert!(
                    path.as_os_str().as_bytes().len() < 104,
                    "fixture TMPDIR must keep Unix socket paths below 104 bytes"
                );
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&directory)
                    .unwrap();
                Self { directory, path }
            }
            #[cfg(not(unix))]
            {
                Self { name: label }
            }
        }

        fn client(&self) -> ControlClient {
            #[cfg(unix)]
            let address = SocketAddr::Path(self.path.clone());
            #[cfg(not(unix))]
            let address = SocketAddr::Name(self.name.clone());
            ControlClient::for_test_address(address)
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
                std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))
                    .unwrap();
            }
            listener
        }
    }

    impl Drop for Endpoint {
        fn drop(&mut self) {
            #[cfg(unix)]
            {
                // No recursion: remove only the owned socket (or deliberate
                // non-socket file), then its exclusively created directory.
                let _ = std::fs::remove_file(&self.path);
                let _ = std::fs::remove_dir(&self.directory);
            }
        }
    }

    async fn bounded<F: Future>(future: F) -> F::Output {
        tokio::time::timeout(DEADLINE, future)
            .await
            .expect("owned IPC fixture exceeded its deadline")
    }

    enum Reply {
        Status(Response),
        Raw(&'static [u8]),
        Close,
        Silent,
    }

    struct FakeDaemon {
        task: Option<JoinHandle<Vec<Vec<Value>>>>,
    }

    async fn read_request<R: AsyncRead + Unpin>(reader: &mut BufReader<R>) -> Option<Value> {
        let mut line = String::new();
        if bounded(reader.read_line(&mut line)).await.unwrap() == 0 {
            return None;
        }
        assert!(line.len() <= 16 * 1024, "fixture request unexpectedly large");
        Some(serde_json::from_str(line.trim()).unwrap())
    }

    impl FakeDaemon {
        fn start(endpoint: &Endpoint, replies: Vec<Reply>) -> Self {
            let listener = endpoint.bind();
            let task = tokio::spawn(async move {
                let mut connections = Vec::new();
                for reply in replies {
                    let stream = bounded(listener.accept()).await.unwrap();
                    let (reader, mut writer) = stream.split();
                    let mut reader = BufReader::new(reader);
                    let first = read_request(&mut reader)
                        .await
                        .expect("fixture expected Status before EOF");
                    let mut requests = vec![first];
                    match reply {
                        Reply::Status(response) => {
                            let line = serde_json::to_string(&response).unwrap() + "\n";
                            bounded(writer.write_all(line.as_bytes())).await.unwrap();
                            bounded(writer.flush()).await.unwrap();
                        }
                        Reply::Raw(bytes) => {
                            bounded(writer.write_all(bytes)).await.unwrap();
                            bounded(writer.flush()).await.unwrap();
                        }
                        Reply::Close => {
                            connections.push(requests);
                            continue;
                        }
                        Reply::Silent => {}
                    }
                    // EOF is causal proof that the client has finished this
                    // connection. No sleeps or traffic from another endpoint
                    // can satisfy the protected-operation assertions.
                    while let Some(request) = read_request(&mut reader).await {
                        assert!(requests.len() < 4, "unexpected extra fixture requests");
                        requests.push(request);
                        bounded(writer.write_all(b"{\"ok\":true,\"data\":{\"fixture_ack\":true}}\n"))
                            .await
                            .unwrap();
                        bounded(writer.flush()).await.unwrap();
                    }
                    connections.push(requests);
                }
                connections
            });
            Self { task: Some(task) }
        }

        async fn finish(&mut self) -> Vec<Vec<Value>> {
            // Keep the handle inside self through timeout/unwind so Drop can
            // cancel the listener and accepted connections on every failure.
            let observed = bounded(self.task.as_mut().unwrap()).await.unwrap();
            let _ = self.task.take();
            observed
        }
    }

    impl Drop for FakeDaemon {
        fn drop(&mut self) {
            if let Some(task) = self.task.take() {
                task.abort();
            }
        }
    }

    fn operation() -> Request {
        Request::ChannelSendTo {
            network: "fixture-network".into(),
            channel: "fixture-channel".into(),
            peer: "fixture-peer".into(),
            payload: json!({"fixture": 7}),
        }
    }

    fn status_only() -> Vec<Value> {
        vec![json!({"op": "status"})]
    }

    fn status_and_operation() -> Vec<Value> {
        vec![
            json!({"op": "status"}),
            json!({
                "op": "channel_send_to", "network": "fixture-network",
                "channel": "fixture-channel", "peer": "fixture-peer",
                "payload": {"fixture": 7}
            }),
        ]
    }

    #[tokio::test]
    async fn missing_owned_endpoint_is_absent_without_lifecycle_launch() {
        let endpoint = Endpoint::new();
        let client = endpoint.client();
        assert_eq!(bounded(client.probe_contract()).await, EndpointProbe::Absent);
        assert!(!bounded(crate::daemon_spawn::probe(&client)).await);
        // Deliberately do not invoke ensure_daemon_running on an absent endpoint.
    }

    #[tokio::test]
    async fn occupied_legacy_status_and_false_flag_remain_ready() {
        for media_pipes in [true, false] {
            let endpoint = Endpoint::new();
            let mut status = released_status();
            set_field(&mut status, "media_pipes", json!(media_pipes));
            let mut server = FakeDaemon::start(
                &endpoint,
                vec![Reply::Status(status.clone()), Reply::Status(status)],
            );
            let client = endpoint.client();
            assert_eq!(
                bounded(client.probe_contract()).await,
                EndpointProbe::Answered(ContractReadiness::ReadyLegacy(LegacyStatus {
                    media_pipes,
                }))
            );
            assert!(bounded(crate::daemon_spawn::probe(&client)).await);
            assert_eq!(server.finish().await, vec![status_only(), status_only()]);
        }
    }

    #[tokio::test]
    async fn occupied_status_refusals_are_answered_and_preserve_occupancy() {
        let mut unknown = released_status();
        set_field(&mut unknown, "version", json!("0.3.22"));
        let mut missing_version = released_status();
        remove_field(&mut missing_version, "version");
        let mut invalid_media = released_status();
        set_field(&mut invalid_media, "media_pipes", json!("true"));
        for (status, refusal) in [
            (candidate_status(true), ContractRefusal::CandidateNotMigrated),
            (
                Response::err("fixture refusal"),
                ContractRefusal::StatusRejected(Some("fixture refusal".into())),
            ),
            (
                Response {
                    ok: true,
                    error: None,
                    data: None,
                },
                ContractRefusal::MissingStatusData,
            ),
            (missing_version, ContractRefusal::MissingVersion),
            (unknown, ContractRefusal::UnsupportedVersion("0.3.22".into())),
            (
                invalid_media,
                ContractRefusal::MalformedStatus("media_pipes field"),
            ),
        ] {
            let endpoint = Endpoint::new();
            let mut server = FakeDaemon::start(
                &endpoint,
                vec![Reply::Status(status.clone()), Reply::Status(status)],
            );
            let client = endpoint.client();
            assert_eq!(
                bounded(client.probe_contract()).await,
                EndpointProbe::Answered(ContractReadiness::Refused(refusal))
            );
            assert!(bounded(crate::daemon_spawn::probe(&client)).await);
            assert_eq!(server.finish().await, vec![status_only(), status_only()]);
        }
    }

    #[tokio::test]
    async fn diagnostic_status_remains_readable_on_a_refused_candidate_endpoint() {
        let endpoint = Endpoint::new();
        let candidate = candidate_status(true);
        let mut server = FakeDaemon::start(
            &endpoint,
            vec![Reply::Status(candidate.clone()), Reply::Status(candidate.clone())],
        );
        let client = endpoint.client();
        let response = bounded(client.request(&Request::Status)).await.unwrap();
        assert_eq!(response.data, candidate.data);
        assert_refusal(&response, ContractRefusal::CandidateNotMigrated);
        assert_eq!(
            bounded(client.probe_contract()).await,
            EndpointProbe::Answered(ContractReadiness::Refused(
                ContractRefusal::CandidateNotMigrated,
            ))
        );
        assert_eq!(server.finish().await, vec![status_only(), status_only()]);
    }

    #[tokio::test]
    async fn malformed_and_closed_status_connections_are_unavailable() {
        for reply in [Reply::Raw(b"fixture-private-invalid-json\n"), Reply::Close] {
            let endpoint = Endpoint::new();
            let mut server = FakeDaemon::start(&endpoint, vec![reply]);
            let result = bounded(endpoint.client().probe_contract()).await;
            let EndpointProbe::Unavailable(error) = result else {
                panic!("occupied endpoint must be unavailable, got {result:?}");
            };
            assert!(
                !error.contains(SENTINEL),
                "raw remote payload must not appear in probe diagnostics"
            );
            assert_eq!(server.finish().await, vec![status_only()]);
        }
    }

    #[tokio::test]
    async fn silent_occupied_status_endpoint_is_bounded_and_unavailable() {
        let endpoint = Endpoint::new();
        let mut server = FakeDaemon::start(&endpoint, vec![Reply::Silent]);
        assert!(matches!(
            bounded(endpoint.client().probe_contract()).await,
            EndpointProbe::Unavailable(_)
        ));
        assert_eq!(server.finish().await, vec![status_only()]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn occupied_non_socket_path_is_unavailable() {
        let endpoint = Endpoint::new();
        std::fs::write(&endpoint.path, b"owned non-socket fixture").unwrap();
        assert!(matches!(
            bounded(endpoint.client().probe_contract()).await,
            EndpointProbe::Unavailable(_)
        ));
        assert!(bounded(crate::daemon_spawn::probe(&endpoint.client())).await);
    }

    #[tokio::test]
    async fn protected_legacy_requests_check_status_on_their_actual_connection() {
        let endpoint = Endpoint::new();
        let mut fallback = released_status();
        set_field(&mut fallback, "media_pipes", json!(false));
        let mut server = FakeDaemon::start(
            &endpoint,
            vec![Reply::Status(released_status()), Reply::Status(fallback)],
        );
        let client = endpoint.client();
        for response in [
            bounded(client.request(&operation())).await.unwrap(),
            bounded(client.request_with_timeout(&operation(), Duration::from_secs(2)))
                .await
                .unwrap(),
        ] {
            assert!(response.ok);
            assert_eq!(response.data, Some(json!({"fixture_ack": true})));
        }
        assert_eq!(
            server.finish().await,
            vec![status_and_operation(), status_and_operation()]
        );
    }

    #[tokio::test]
    async fn protected_operation_is_not_written_after_contract_refusal() {
        let mut unknown = released_status();
        set_field(&mut unknown, "version", json!("0.3.22"));
        let mut missing_version = released_status();
        remove_field(&mut missing_version, "version");
        let mut malformed_version = released_status();
        set_field(&mut malformed_version, "version", json!("0.3.21+local"));
        for reply in [
            Reply::Status(candidate_status(true)),
            Reply::Status(unknown),
            Reply::Status(missing_version),
            Reply::Status(malformed_version),
            Reply::Status(Response::err("fixture refusal")),
            Reply::Status(Response { ok: true, error: None, data: None }),
            Reply::Raw(b"fixture-private-invalid-json\n"),
            Reply::Close,
        ] {
            let endpoint = Endpoint::new();
            let mut server = FakeDaemon::start(&endpoint, vec![reply]);
            assert!(bounded(endpoint.client().request(&operation())).await.is_err());
            assert_eq!(server.finish().await, vec![status_only()]);
        }
    }

    #[tokio::test]
    async fn previous_ready_probe_does_not_authorize_a_new_candidate_connection() {
        let endpoint = Endpoint::new();
        let mut server = FakeDaemon::start(
            &endpoint,
            vec![Reply::Status(released_status()), Reply::Status(candidate_status(true))],
        );
        let client = endpoint.client();
        assert_eq!(
            bounded(client.probe_contract()).await,
            EndpointProbe::Answered(ContractReadiness::ReadyLegacy(LegacyStatus {
                media_pipes: true,
            }))
        );
        assert!(bounded(client.request_with_timeout(&operation(), Duration::from_secs(2)))
            .await
            .is_err());
        assert_eq!(server.finish().await, vec![status_only(), status_only()]);
    }

    #[tokio::test]
    async fn candidate_refuses_event_and_media_handshakes_before_their_operation() {
        let endpoint = Endpoint::new();
        let mut server = FakeDaemon::start(
            &endpoint,
            vec![
                Reply::Status(candidate_status(true)),
                Reply::Status(candidate_status(true)),
            ],
        );
        let client = endpoint.client();
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(1);
        assert!(bounded(client.subscribe_events(event_tx)).await.is_err());
        let pipe = MediaPipe::new(Arc::new(endpoint.client()));
        assert!(bounded(pipe.send(&operation())).await.is_err());
        drop(pipe);
        assert_eq!(server.finish().await, vec![status_only(), status_only()]);

        // The session-bound source API requires a real owned registration.
        // A bare id through the generic request API is refused locally; do
        // not forge a registration or negotiate another wire on its behalf.
        let raw_endpoint = Endpoint::new();
        let listener = raw_endpoint.bind();
        let request = Request::MediaSourcePipe { client_id: "c1".parse().unwrap() };
        let error = bounded(raw_endpoint.client().request(&request)).await.unwrap_err();
        assert!(error.to_string().contains("owned event registration"));
        let mut accept = Box::pin(listener.accept());
        let pending = std::future::poll_fn(|cx| {
            std::task::Poll::Ready(accept.as_mut().poll(cx))
        }).await;
        assert!(pending.is_pending(), "a locally refused session operation must not connect");
    }

    #[tokio::test]
    async fn occupied_refusals_stop_lifecycle_before_binary_lookup_or_state_work() {
        for reply in [
            Reply::Status(candidate_status(true)),
            Reply::Raw(b"fixture-private-invalid-json\n"),
            Reply::Close,
        ] {
            let endpoint = Endpoint::new();
            let mut server = FakeDaemon::start(&endpoint, vec![reply]);
            // No absent/ready branch is selected; these return before any
            // installed binary lookup, orphan/pidfile inspection or spawning.
            assert!(bounded(crate::daemon_spawn::ensure_daemon_running(&endpoint.client()))
                .await
                .is_err());
            assert_eq!(server.finish().await, vec![status_only()]);
        }
    }
}
