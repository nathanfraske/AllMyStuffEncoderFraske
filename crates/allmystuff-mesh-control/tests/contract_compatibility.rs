//! Portable Status and transport fixtures using only public library APIs.
//!
//! Literal release evidence: mrjeeves/MyOwnMesh v0.3.21,
//! crates/myownmesh/src/control.rs:942-959. Literal candidate evidence:
//! nathanfraske/MyOwnMeshSecurityReview db7818e09fedd98899490347b86ac9bc9f97b59b,
//! crates/myownmesh/src/control/reply.rs:324-343 and control/wire.rs:813-839.
//! These observations do not prove a daemon's Git identity, select a product
//! migration, authenticate a device, or authorize application/ledger access.
//!
//! Each IPC fixture creates its own endpoint. No default address, daemon
//! process, global environment change, device, installer or live state is used.

use allmystuff_mesh_control::contract::{
    parse_status_evidence, verify_wire_status, RealtimeAdvert, RealtimeEncoding, StatusError,
};
use allmystuff_mesh_control::{EventContract, Response};
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

fn assert_parse_error(response: &Response, expected: StatusError) {
    assert_eq!(parse_status_evidence(response), Err(expected));
}

#[test]
fn released_status_preserves_exact_version_text_and_boolean_media() {
    for version in ["0.3.21", "v0.3.21", " \t0.3.21\n"] {
        for media_pipes in [true, false] {
            let mut response = released_status();
            set_field(&mut response, "version", json!(version));
            set_field(&mut response, "media_pipes", json!(media_pipes));
            let evidence = parse_status_evidence(&response).unwrap();
            assert_eq!(evidence.version, version);
            assert_eq!(evidence.device_id, "fixture-device");
            assert_eq!(evidence.joined_networks, ["fixture-network"]);
            assert_eq!(evidence.media_pipes, Some(media_pipes));
            assert_eq!(evidence.realtime, None);
            assert_eq!(
                verify_wire_status(&response, EventContract::LegacyV0_3_21),
                Ok(evidence)
            );
        }
    }
}

#[test]
fn candidate_advert_preserves_h264_opus_and_disabled_evidence() {
    let encodings = vec![
        RealtimeEncoding {
            kind: "video".into(),
            mime: "video/H264".into(),
            clock_rate: 90000,
            channels: 0,
        },
        RealtimeEncoding {
            kind: "audio".into(),
            mime: "audio/opus".into(),
            clock_rate: 48000,
            channels: 2,
        },
    ];
    for version in ["1.0.0", "v1.0.0", " 1.0.0 "] {
        for supported in [true, false] {
            let mut response = candidate_status(supported);
            set_field(&mut response, "version", json!(version));
            let evidence = parse_status_evidence(&response).unwrap();
            assert_eq!(evidence.version, version);
            assert_eq!(evidence.device_id, "fixture-device");
            assert!(evidence.joined_networks.is_empty());
            assert_eq!(evidence.media_pipes, None);
            assert_eq!(
                evidence.realtime,
                Some(RealtimeAdvert {
                    supported,
                    encodings: if supported {
                        encodings.clone()
                    } else {
                        Vec::new()
                    },
                })
            );
            assert_eq!(
                verify_wire_status(&response, EventContract::CandidateV1Db7818e),
                Ok(evidence)
            );
        }
    }
}

#[test]
fn wire_selection_rejects_cross_family_status_despite_lookalike_fields() {
    let mut candidate = candidate_status(true);
    set_field(&mut candidate, "media_pipes", json!(true));
    let mut legacy = released_status();
    set_field(
        &mut legacy,
        "realtime",
        candidate.data.as_ref().unwrap()["realtime"].clone(),
    );
    for (response, selected, reported) in [
        (candidate, EventContract::LegacyV0_3_21, "1.0.0"),
        (legacy, EventContract::CandidateV1Db7818e, "0.3.21"),
    ] {
        let evidence = parse_status_evidence(&response).unwrap();
        assert!(evidence.media_pipes.is_some());
        assert!(evidence.realtime.is_some());
        assert_eq!(
            verify_wire_status(&response, selected),
            Err(StatusError::UnsupportedWireVersion {
                expected: selected,
                reported: reported.into(),
            })
        );
    }
}

#[test]
fn unknown_and_decorated_versions_are_evidence_without_wire_compatibility() {
    for version in [
        "",
        " ",
        "0",
        "0.3",
        "0.3.20",
        "0.3.22",
        "v0.3.22",
        "0.4.0",
        "1.0.1",
        "2.0.0",
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
        assert_eq!(parse_status_evidence(&response).unwrap().version, version);
        for selected in [
            EventContract::LegacyV0_3_21,
            EventContract::CandidateV1Db7818e,
        ] {
            assert_eq!(
                verify_wire_status(&response, selected),
                Err(StatusError::UnsupportedWireVersion {
                    expected: selected,
                    reported: version.into(),
                })
            );
        }
    }
}

#[test]
fn rejected_or_contradictory_envelopes_cannot_supply_status_evidence() {
    const PRIVATE_ERROR: &str = "fixture-private-remote-error";
    for ok in [false, true] {
        let mut response = released_status();
        response.ok = ok;
        response.error = Some(PRIVATE_ERROR.into());
        let expected = if ok {
            StatusError::MalformedStatus("response envelope")
        } else {
            StatusError::StatusRejected
        };
        assert_parse_error(&response, expected.clone());
        assert!(!expected.to_string().contains(PRIVATE_ERROR));
        for selected in [
            EventContract::LegacyV0_3_21,
            EventContract::CandidateV1Db7818e,
        ] {
            assert_eq!(verify_wire_status(&response, selected), Err(expected.clone()));
        }
    }
    let mut response = released_status();
    response.ok = false;
    assert_parse_error(&response, StatusError::StatusRejected);
}

#[test]
fn status_data_must_be_present_and_an_object() {
    assert_parse_error(
        &Response {
            ok: true,
            error: None,
            data: None,
        },
        StatusError::MissingStatusData,
    );
    for data in [
        json!(null),
        json!(false),
        json!(7),
        json!("fixture"),
        json!([]),
    ] {
        assert_parse_error(
            &Response::ok(data),
            StatusError::MalformedStatus("data object"),
        );
    }
}

#[test]
fn version_must_be_present_and_a_string() {
    let mut missing = released_status();
    remove_field(&mut missing, "version");
    assert_parse_error(&missing, StatusError::MissingVersion);
    for value in [json!(null), json!(3), json!(false), json!([]), json!({})] {
        let mut response = released_status();
        set_field(&mut response, "version", value);
        assert_parse_error(&response, StatusError::MalformedStatus("version field"));
    }
}

#[test]
fn common_device_and_network_fields_have_required_shapes() {
    for (field, malformed, reason) in [
        (
            "device_id",
            vec![json!(null), json!(""), json!(7), json!([])],
            "device_id field",
        ),
        (
            "joined_networks",
            vec![
                json!(null),
                json!("network"),
                json!(["network", 7]),
                json!({}),
            ],
            "joined_networks field",
        ),
    ] {
        for selected in [
            EventContract::LegacyV0_3_21,
            EventContract::CandidateV1Db7818e,
        ] {
            let original = match selected {
                EventContract::LegacyV0_3_21 => released_status(),
                EventContract::CandidateV1Db7818e => candidate_status(true),
            };
            let mut missing = original.clone();
            remove_field(&mut missing, field);
            assert_parse_error(&missing, StatusError::MalformedStatus(reason));
            for value in &malformed {
                let mut response = original.clone();
                set_field(&mut response, field, value.clone());
                assert_parse_error(&response, StatusError::MalformedStatus(reason));
                assert_eq!(
                    verify_wire_status(&response, selected),
                    Err(StatusError::MalformedStatus(reason))
                );
            }
        }
    }
    let mut response = released_status();
    set_field(&mut response, "joined_networks", json!(["b", "a", "b"]));
    assert_eq!(
        parse_status_evidence(&response).unwrap().joined_networks,
        ["b", "a", "b"]
    );
}

#[test]
fn absent_media_flag_is_not_inferred_from_lane_count() {
    let mut response = released_status();
    remove_field(&mut response, "media_pipes");
    set_field(&mut response, "media_lanes", json!(8));
    assert_eq!(parse_status_evidence(&response).unwrap().media_pipes, None);
    assert_eq!(
        verify_wire_status(&response, EventContract::LegacyV0_3_21),
        Err(StatusError::MalformedStatus("media_pipes field"))
    );
}

#[test]
fn present_media_flag_must_be_boolean_on_either_wire() {
    for value in [
        json!(null),
        json!(0),
        json!(1),
        json!("true"),
        json!([]),
        json!({}),
    ] {
        for mut response in [released_status(), candidate_status(true)] {
            set_field(&mut response, "media_pipes", value.clone());
            assert_parse_error(&response, StatusError::MalformedStatus("media_pipes field"));
        }
    }
}

#[test]
fn absent_realtime_is_not_inferred_from_legacy_media() {
    let mut response = candidate_status(true);
    remove_field(&mut response, "realtime");
    set_field(&mut response, "media_pipes", json!(true));
    let evidence = parse_status_evidence(&response).unwrap();
    assert_eq!(evidence.media_pipes, Some(true));
    assert_eq!(evidence.realtime, None);
    assert_eq!(
        verify_wire_status(&response, EventContract::CandidateV1Db7818e),
        Err(StatusError::MalformedStatus("realtime field"))
    );
}

#[test]
fn realtime_advert_requires_boolean_supported_and_encoding_array() {
    for value in [
        json!(null),
        json!(false),
        json!([]),
        json!({}),
        json!({"encodings": []}),
        json!({"supported": true}),
        json!({"supported": "true", "encodings": []}),
        json!({"supported": 1, "encodings": []}),
        json!({"supported": true, "encodings": null}),
        json!({"supported": true, "encodings": {}}),
        json!({"supported": true, "encodings": [null]}),
    ] {
        for mut response in [released_status(), candidate_status(true)] {
            set_field(&mut response, "realtime", value.clone());
            assert_parse_error(&response, StatusError::MalformedStatus("realtime field"));
        }
    }
}

#[test]
fn encoding_records_preserve_actual_widths_without_codec_policy() {
    for (field, malformed) in [
        ("kind", vec![json!(null), json!(7), json!([])]),
        ("mime", vec![json!(null), json!(false), json!({})]),
        (
            "clock_rate",
            vec![json!(null), json!(-1), json!(1.5), json!(4294967296_u64)],
        ),
        (
            "channels",
            vec![json!(null), json!(-1), json!(1.5), json!(65536)],
        ),
    ] {
        let mut missing = candidate_status(true);
        missing.data.as_mut().unwrap()["realtime"]["encodings"][0]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert_parse_error(&missing, StatusError::MalformedStatus("realtime field"));
        for value in malformed {
            let mut response = candidate_status(true);
            response.data.as_mut().unwrap()["realtime"]["encodings"][0][field] = value;
            assert_parse_error(&response, StatusError::MalformedStatus("realtime field"));
        }
    }
    // The wire's integer widths and strings are evidence. This parser does
    // not promise support for an unfamiliar codec, rate or channel layout.
    let mut response = candidate_status(true);
    response.data.as_mut().unwrap()["realtime"]["encodings"] = json!([
        {"kind": "fixture-kind", "mime": "application/fixture", "clock_rate": 4294967295_u64, "channels": 65535}
    ]);
    assert_eq!(
        verify_wire_status(&response, EventContract::CandidateV1Db7818e)
            .unwrap()
            .realtime,
        Some(RealtimeAdvert {
            supported: true,
            encodings: vec![RealtimeEncoding {
                kind: "fixture-kind".into(),
                mime: "application/fixture".into(),
                clock_rate: u32::MAX,
                channels: u16::MAX,
            }],
        })
    );
}

#[tokio::test]
async fn bounded_status_line_keeps_the_next_ack_and_rejects_overflow() {
    use allmystuff_mesh_control::transport::read_json_line;
    use allmystuff_mesh_control::STATUS_ACK_LIMIT;
    use tokio::io::BufReader;

    let prefix = b"{\"ok\":true,\"data\":\"";
    let suffix = b"\"}\n";
    let mut exact = prefix.to_vec();
    exact.resize(16 * 1024 - suffix.len(), b'x');
    exact.extend_from_slice(suffix);
    let next = b"{\"ok\":false,\"error\":\"fixture\"}\n";
    let mut both = exact.clone();
    both.extend_from_slice(next);
    let mut reader = BufReader::new(both.as_slice());
    assert_eq!(
        read_json_line(&mut reader, Some(STATUS_ACK_LIMIT))
            .await
            .unwrap(),
        exact
    );
    assert_eq!(
        read_json_line(&mut reader, Some(STATUS_ACK_LIMIT))
            .await
            .unwrap(),
        next.to_vec()
    );
    assert!(read_json_line(&mut reader, Some(STATUS_ACK_LIMIT))
        .await
        .is_err());
    let mut overflow = exact;
    overflow.insert(prefix.len(), b'x');
    let mut reader = BufReader::new(overflow.as_slice());
    let error = read_json_line(&mut reader, Some(STATUS_ACK_LIMIT))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("adapter limit"));
}

mod ipc {
    use super::*;
    use allmystuff_mesh_control::{ControlClient, Endpoint, Request, StatusProbe};
    use interprocess::local_socket::tokio::{prelude::*, Listener};
    #[cfg(unix)]
    use interprocess::local_socket::GenericFilePath;
    #[cfg(not(unix))]
    use interprocess::local_socket::GenericNamespaced;
    use interprocess::local_socket::ListenerOptions;
    use std::future::Future;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
    use tokio::task::JoinHandle;

    const DEADLINE: Duration = Duration::from_secs(10);
    const SENTINEL: &str = "fixture-private-invalid-json";
    static NEXT_ENDPOINT: AtomicU64 = AtomicU64::new(0);

    struct OwnedEndpoint {
        #[cfg(unix)]
        directory: std::path::PathBuf,
        #[cfg(unix)]
        path: std::path::PathBuf,
        #[cfg(not(unix))]
        name: String,
    }

    impl OwnedEndpoint {
        fn new() -> Self {
            let sequence = NEXT_ENDPOINT.fetch_add(1, Ordering::Relaxed);
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let label = format!("ams-mc-{}-{nonce:x}-{sequence:x}", std::process::id());
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt;
                use std::os::unix::fs::DirBuilderExt;
                let directory = std::env::temp_dir().join(label);
                let path = directory.join("s");
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
            let address = Endpoint::Path(self.path.clone());
            #[cfg(not(unix))]
            let address = Endpoint::Name(self.name.clone());
            ControlClient::new(address)
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

    impl Drop for OwnedEndpoint {
        fn drop(&mut self) {
            #[cfg(unix)]
            {
                // Never recurse or remove a shared parent. Only the owned
                // socket/non-socket file and exclusive directory are eligible.
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

    async fn read_request<R: AsyncRead + Unpin>(
        reader: &mut BufReader<R>,
    ) -> Option<(String, Value)> {
        let mut line = String::new();
        if bounded(reader.read_line(&mut line)).await.unwrap() == 0 {
            return None;
        }
        assert!(line.len() <= 16 * 1024, "fixture request unexpectedly large");
        assert!(line.ends_with('\n'), "fixture request must be line framed");
        let value = serde_json::from_str(line.trim()).unwrap();
        Some((line, value))
    }

    impl FakeDaemon {
        fn start(endpoint: &OwnedEndpoint, replies: Vec<Reply>) -> Self {
            let listener = endpoint.bind();
            let task = tokio::spawn(async move {
                let mut connections = Vec::new();
                for reply in replies {
                    let stream = bounded(listener.accept()).await.unwrap();
                    let (reader, mut writer) = stream.split();
                    let mut reader = BufReader::new(reader);
                    let (line, first) = read_request(&mut reader)
                        .await
                        .expect("fixture expected Status before EOF");
                    assert_eq!(line, "{\"op\":\"status\"}\n");
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
                    // Client EOF fences this connection's complete request
                    // sequence. A sleep or another endpoint cannot satisfy it.
                    while let Some((_, request)) = read_request(&mut reader).await {
                        assert!(requests.len() < 4, "unexpected extra fixture requests");
                        requests.push(request);
                        bounded(
                            writer.write_all(b"{\"ok\":true,\"data\":{\"fixture_ack\":true}}\n"),
                        )
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
            // Retain the handle during timeout/unwind so Drop still owns it.
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

    fn status_only() -> Vec<Value> {
        vec![json!({"op": "status"})]
    }

    fn assert_response_eq(actual: &Response, expected: &Response) {
        assert_eq!(actual.ok, expected.ok);
        assert_eq!(actual.error, expected.error);
        assert_eq!(actual.data, expected.data);
    }

    async fn answered(client: &ControlClient) -> Response {
        match bounded(client.probe_status()).await {
            StatusProbe::Answered(response) => response,
            other => panic!("expected raw Status evidence, got {other:?}"),
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

    fn require_fixture_guard(response: &Response) -> anyhow::Result<()> {
        if response
            .data
            .as_ref()
            .and_then(|data| data.get("fixture_guard"))
            != Some(&json!("allow"))
        {
            anyhow::bail!("fixture policy refused the actual Status");
        }
        Ok(())
    }

    #[tokio::test]
    async fn missing_owned_endpoint_is_absent() {
        let endpoint = OwnedEndpoint::new();
        assert!(matches!(
            bounded(endpoint.client().probe_status()).await,
            StatusProbe::Absent
        ));
    }

    #[tokio::test]
    async fn raw_legacy_status_retains_true_and_false_media_evidence() {
        for media_pipes in [true, false] {
            let endpoint = OwnedEndpoint::new();
            let mut expected = released_status();
            set_field(&mut expected, "media_pipes", json!(media_pipes));
            let mut server = FakeDaemon::start(&endpoint, vec![Reply::Status(expected.clone())]);
            let response = answered(&endpoint.client()).await;
            assert_response_eq(&response, &expected);
            assert_eq!(
                verify_wire_status(&response, EventContract::LegacyV0_3_21)
                    .unwrap()
                    .media_pipes,
                Some(media_pipes)
            );
            assert_eq!(server.finish().await, vec![status_only()]);
        }
    }

    #[tokio::test]
    async fn raw_candidate_and_unknown_status_are_answered_without_wire_selection() {
        let mut unknown = released_status();
        set_field(&mut unknown, "version", json!("0.3.22"));
        for expected in [candidate_status(true), candidate_status(false), unknown] {
            let endpoint = OwnedEndpoint::new();
            let mut server = FakeDaemon::start(&endpoint, vec![Reply::Status(expected.clone())]);
            let response = answered(&endpoint.client()).await;
            assert_response_eq(&response, &expected);
            assert!(parse_status_evidence(&response).is_ok());
            assert!(verify_wire_status(&response, EventContract::LegacyV0_3_21).is_err());
            assert_eq!(server.finish().await, vec![status_only()]);
        }
    }

    #[tokio::test]
    async fn invalid_semantic_status_stays_answered_transport_evidence() {
        let mut missing_version = released_status();
        remove_field(&mut missing_version, "version");
        let mut invalid_media = released_status();
        set_field(&mut invalid_media, "media_pipes", json!("true"));
        let mut contradictory = released_status();
        contradictory.error = Some("fixture contradictory error".into());
        for expected in [
            Response::err("fixture refusal"),
            Response {
                ok: true,
                error: None,
                data: None,
            },
            missing_version,
            invalid_media,
            contradictory,
        ] {
            let endpoint = OwnedEndpoint::new();
            let mut server = FakeDaemon::start(&endpoint, vec![Reply::Status(expected.clone())]);
            let response = answered(&endpoint.client()).await;
            assert_response_eq(&response, &expected);
            assert!(parse_status_evidence(&response).is_err());
            assert_eq!(server.finish().await, vec![status_only()]);
        }
    }

    #[tokio::test]
    async fn diagnostic_status_sends_one_literal_request_on_either_selected_wire() {
        for selected in [
            EventContract::LegacyV0_3_21,
            EventContract::CandidateV1Db7818e,
        ] {
            for expected in [candidate_status(true), Response::err("fixture refusal")] {
                let endpoint = OwnedEndpoint::new();
                let mut server =
                    FakeDaemon::start(&endpoint, vec![Reply::Status(expected.clone())]);
                let response = bounded(endpoint.client().request(selected, &Request::Status))
                    .await
                    .unwrap();
                assert_response_eq(&response, &expected);
                assert_eq!(server.finish().await, vec![status_only()]);
            }
        }
    }

    #[tokio::test]
    async fn explicit_candidate_generic_operation_refuses_before_transport() {
        // There is no listener. A transport attempt would return a connection
        // error, whereas this explicit unsupported operation refuses locally.
        // Session/capability fixtures use only real owned registrations in the
        // separate session_lifecycle suite; none is fabricated here.
        let endpoint = OwnedEndpoint::new();
        let error = bounded(endpoint.client().request(
            EventContract::CandidateV1Db7818e,
            &operation(),
        ))
        .await
        .unwrap_err();
        assert!(error.to_string().contains("candidate request"));
        assert!(error.to_string().contains("characterized"));
    }

    #[tokio::test]
    async fn malformed_and_closed_status_connections_are_unavailable() {
        for reply in [Reply::Raw(b"fixture-private-invalid-json\n"), Reply::Close] {
            let endpoint = OwnedEndpoint::new();
            let mut server = FakeDaemon::start(&endpoint, vec![reply]);
            let result = bounded(endpoint.client().probe_status()).await;
            let StatusProbe::Unavailable(error) = result else {
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
        let endpoint = OwnedEndpoint::new();
        let mut server = FakeDaemon::start(&endpoint, vec![Reply::Silent]);
        assert!(matches!(
            bounded(endpoint.client().probe_status()).await,
            StatusProbe::Unavailable(_)
        ));
        assert_eq!(server.finish().await, vec![status_only()]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn occupied_non_socket_path_is_unavailable() {
        let endpoint = OwnedEndpoint::new();
        std::fs::write(&endpoint.path, b"owned non-socket fixture").unwrap();
        assert!(matches!(
            bounded(endpoint.client().probe_status()).await,
            StatusProbe::Unavailable(_)
        ));
    }

    #[tokio::test]
    async fn checked_legacy_request_uses_status_and_operation_on_one_connection() {
        for media_pipes in [true, false] {
            let endpoint = OwnedEndpoint::new();
            let mut status = released_status();
            set_field(&mut status, "media_pipes", json!(media_pipes));
            set_field(&mut status, "fixture_guard", json!("allow"));
            let mut server = FakeDaemon::start(&endpoint, vec![Reply::Status(status)]);
            let response = bounded(endpoint.client().request_checked(
                EventContract::LegacyV0_3_21,
                &operation(),
                Duration::from_secs(2),
                require_fixture_guard,
            ))
            .await
            .unwrap();
            assert!(response.ok);
            assert_eq!(response.data, Some(json!({"fixture_ack": true})));
            assert_eq!(server.finish().await, vec![status_and_operation()]);
        }
    }

    #[tokio::test]
    async fn current_connection_guard_refuses_operation_after_a_previous_probe() {
        let mut wrong_wire = candidate_status(true);
        set_field(&mut wrong_wire, "fixture_guard", json!("deny"));
        let mut missing_version = released_status();
        remove_field(&mut missing_version, "version");
        set_field(&mut missing_version, "fixture_guard", json!("deny"));
        let mut denied = released_status();
        set_field(&mut denied, "fixture_guard", json!("deny"));
        for current in [denied, wrong_wire, missing_version] {
            let endpoint = OwnedEndpoint::new();
            let mut previous = released_status();
            set_field(&mut previous, "fixture_guard", json!("allow"));
            let mut server = FakeDaemon::start(
                &endpoint,
                vec![Reply::Status(previous.clone()), Reply::Status(current)],
            );
            let client = endpoint.client();
            assert_response_eq(&answered(&client).await, &previous);
            let error = bounded(client.request_checked(
                EventContract::LegacyV0_3_21,
                &operation(),
                Duration::from_secs(2),
                require_fixture_guard,
            ))
            .await
            .unwrap_err();
            // The supplied policy runs before wire validation on this actual
            // Status, including a wrong wire or otherwise incomplete response.
            assert_eq!(
                error.to_string(),
                "fixture policy refused the actual Status"
            );
            assert_eq!(server.finish().await, vec![status_only(), status_only()]);
        }
    }
}
