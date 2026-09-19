//! Literal pre-extraction wire fixtures exercised through the public crate API.
//! No test in this file opens a socket or resolves a production address.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use allmystuff_node_client::{
    read_frame, write_frame, NodeClient, NodeEvent, NodeRequest, SUBSCRIBE_EVENTS, TAG_BYTES,
    TAG_EVENT, TAG_JSON, TAG_RESTART,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

#[derive(Deserialize)]
struct Vector {
    name: String,
    wire: String,
    outcome: String,
    tag: Option<u8>,
    payload: Option<String>,
    message: Option<String>,
}

fn vectors() -> Vec<Vector> {
    serde_json::from_str(include_str!("baseline/wire.json")).unwrap()
}

fn unhex(text: &str) -> Vec<u8> {
    assert_eq!(text.len() % 2, 0);
    text.as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

#[tokio::test]
async fn framing_matches_frozen_literal_vectors() {
    for case in vectors() {
        let bytes = unhex(&case.wire);
        let result = read_frame(&mut bytes.as_slice()).await;
        match case.outcome.as_str() {
            "frame" => assert_eq!(
                result.unwrap(),
                Some((case.tag.unwrap(), unhex(case.payload.as_ref().unwrap()))),
                "{}",
                case.name
            ),
            "eof" => assert_eq!(result.unwrap(), None, "{}", case.name),
            "invalid" => {
                let error = result.unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{}", case.name);
                assert_eq!(error.to_string(), case.message.unwrap(), "{}", case.name);
            }
            "truncated" => assert_eq!(
                result.unwrap_err().kind(),
                io::ErrorKind::UnexpectedEof,
                "{}",
                case.name
            ),
            other => panic!("unknown fixture outcome {other}"),
        }
    }
}

#[derive(Default)]
struct Writer {
    bytes: Vec<u8>,
    flushes: usize,
    fail_write: bool,
    fail_flush: bool,
}

impl AsyncWrite for Writer {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.fail_write {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "fixture write",
            )));
        }
        // Short writes also exercise write_all, independent of the real transport.
        let count = bytes.len().min(2);
        self.bytes.extend_from_slice(&bytes[..count]);
        Poll::Ready(Ok(count))
    }

    fn poll_flush(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.flushes += 1;
        Poll::Ready(if self.fail_flush {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "fixture flush"))
        } else {
            Ok(())
        })
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        panic!("write_frame must flush, not shut down the connection")
    }
}

#[tokio::test]
async fn writer_matches_literal_frames_and_flushes() {
    for case in vectors().into_iter().filter(|v| v.outcome == "frame") {
        let mut writer = Writer::default();
        write_frame(
            &mut writer,
            case.tag.unwrap(),
            &unhex(case.payload.as_ref().unwrap()),
        )
        .await
        .unwrap();
        assert_eq!(writer.bytes, unhex(&case.wire), "{}", case.name);
        assert_eq!(writer.flushes, 1, "{}", case.name);
    }
}

// After the length prefix, fail BEFORE yielding a tag. At exactly the ceiling
// the reader must reach this error, but never allocate a 256 MiB payload.
struct LengthThenError {
    length: u32,
    polls: usize,
}

impl AsyncRead for LengthThenError {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.polls += 1;
        if self.polls == 1 {
            assert_eq!(buffer.remaining(), 4);
            buffer.put_slice(&self.length.to_be_bytes());
            Poll::Ready(Ok(()))
        } else {
            Poll::Ready(Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "fixture tag read",
            )))
        }
    }
}

#[tokio::test]
async fn exact_read_ceiling_is_inclusive_without_large_allocation() {
    for length in [268_435_455, 268_435_456, 268_435_457] {
        let mut reader = LengthThenError { length, polls: 0 };
        let error = read_frame(&mut reader).await.unwrap_err();
        if length <= 268_435_456 {
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert_eq!(error.to_string(), "fixture tag read");
            assert_eq!(reader.polls, 2);
        } else {
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!(reader.polls, 1);
        }
    }
}

#[tokio::test]
async fn transport_write_and_flush_errors_are_not_reclassified() {
    for (fail_write, fail_flush, kind, message, flushes) in [
        (
            true,
            false,
            io::ErrorKind::PermissionDenied,
            "fixture write",
            0,
        ),
        (false, true, io::ErrorKind::BrokenPipe, "fixture flush", 1),
    ] {
        let mut writer = Writer {
            fail_write,
            fail_flush,
            ..Writer::default()
        };
        let error = write_frame(&mut writer, 1, b"abc").await.unwrap_err();
        assert_eq!(error.kind(), kind);
        assert_eq!(error.to_string(), message);
        assert_eq!(writer.flushes, flushes);
    }
}

#[tokio::test]
async fn adjacent_frames_are_consumed_one_at_a_time() {
    let wire = unhex("000000010100000003020102");
    let mut input = wire.as_slice();
    assert_eq!(read_frame(&mut input).await.unwrap(), Some((1, vec![])));
    assert_eq!(input, &wire[5..]);
    assert_eq!(read_frame(&mut input).await.unwrap(), Some((2, vec![1, 2])));
    assert_eq!(read_frame(&mut input).await.unwrap(), None);
}

#[test]
fn request_defaults_and_serialization_match_the_baseline_contract() {
    assert_eq!([TAG_JSON, TAG_BYTES, TAG_EVENT, TAG_RESTART], [0, 1, 2, 3]);
    assert_eq!(SUBSCRIBE_EVENTS, "__subscribe_events");
    for text in [
        r#"{"cmd":"scan_self"}"#,
        r#"{"cmd":"scan_self","args":null}"#,
    ] {
        let request: NodeRequest = serde_json::from_str(text).unwrap();
        assert_eq!(request.cmd, "scan_self");
        assert_eq!(request.args, Value::Null);
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"cmd":"scan_self","args":null}"#
        );
    }
    let request: NodeRequest =
        serde_json::from_str(r#"{"cmd":"fixture","args":[1,null],"extra":true}"#).unwrap();
    assert_eq!(request.args, json!([1, null]));
    assert_eq!(
        serde_json::to_string(&request).unwrap(),
        r#"{"cmd":"fixture","args":[1,null]}"#
    );
    for text in [r#"{}"#, r#"{"cmd":null}"#, r#"{"cmd":7}"#] {
        assert!(serde_json::from_str::<NodeRequest>(text).is_err(), "{text}");
    }
}

#[test]
fn event_discriminators_and_required_fields_match_literal_json() {
    for (event, text) in [
        (
            NodeEvent::Emit {
                event: "fixture".into(),
                payload: Value::Null,
            },
            r#"{"kind":"emit","event":"fixture","payload":null}"#,
        ),
        (NodeEvent::Upgrade, r#"{"kind":"upgrade"}"#),
        (NodeEvent::Restart, r#"{"kind":"restart"}"#),
    ] {
        assert_eq!(serde_json::to_string(&event).unwrap(), text);
        let decoded: NodeEvent = serde_json::from_str(text).unwrap();
        assert_eq!(
            serde_json::to_value(decoded).unwrap(),
            serde_json::to_value(event).unwrap()
        );
    }
    let extra: NodeEvent = serde_json::from_str(r#"{"kind":"upgrade","ignored":17}"#).unwrap();
    assert!(matches!(extra, NodeEvent::Upgrade));
    for text in [
        r#"{}"#,
        r#"{"kind":"other"}"#,
        r#"{"kind":"emit","event":"fixture"}"#,
        r#"{"kind":"emit","payload":null}"#,
        r#"{"kind":"Emit"}"#,
    ] {
        assert!(serde_json::from_str::<NodeEvent>(text).is_err(), "{text}");
    }
}

#[test]
fn legacy_constructor_result_types_remain_public_without_calling_them() {
    let _: fn() -> anyhow::Result<NodeClient> = NodeClient::new;
    let _: fn() -> Result<allmystuff_node_client::terminal::NodeClient, String> =
        allmystuff_node_client::terminal::NodeClient::new;
}
