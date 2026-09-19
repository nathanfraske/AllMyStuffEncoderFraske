//! The existing node wire representation and length-prefixed framing.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// A JSON-bodied frame: a [`NodeRequest`] in, or a normal command's
/// `{ok,result,error}` response (and the event-stream ack) out.
pub const TAG_JSON: u8 = 0;
/// A raw-bytes frame: the response to a poll command (`video_poll`,
/// `term_poll`, `file_poll`), whose body is the engine's already length-framed
/// media batch — kept binary rather than re-encoded as JSON.
pub const TAG_BYTES: u8 = 1;
/// One streamed engine event on the long-lived event connection.
pub const TAG_EVENT: u8 = 2;
/// The "relaunch onto the staged update" signal, streamed on the event
/// connection just before the node re-execs.
pub const TAG_RESTART: u8 = 3;

/// The largest frame we'll read — a media batch poll can be sizeable, but a
/// length this far past anything legitimate is a desync or a hostile peer, and
/// allocating it would be the attack. 256 MiB is comfortably above any real
/// frame while still bounding the damage.
pub const MAX_FRAME_LEN: usize = 256 * 1024 * 1024;

/// Write one length-prefixed frame: `[u32 BE len][tag][payload]`, then flush.
/// `len` counts the tag byte plus the payload, so an empty payload is `len 1`.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    w: &mut W,
    tag: u8,
    payload: &[u8],
) -> std::io::Result<()> {
    let len = (payload.len() as u64 + 1) as u32;
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(&[tag]).await?;
    w.write_all(payload).await?;
    w.flush().await
}

/// Read one length-prefixed frame, returning `(tag, payload)`. `Ok(None)` is a
/// clean EOF or a truncated four-byte length prefix. EOF after a complete
/// length is an error. Rejects a length past [`MAX_FRAME_LEN`] before allocating.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<Option<(u8, Vec<u8>)>> {
    let mut len_buf = [0u8; 4];
    // A clean hangup right at a frame boundary is a normal end of stream.
    match r.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "zero-length frame (missing tag byte)",
        ));
    }
    if len > MAX_FRAME_LEN {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("frame length {len} exceeds the {MAX_FRAME_LEN}-byte ceiling"),
        ));
    }
    let mut tag = [0u8; 1];
    r.read_exact(&mut tag).await?;
    let mut payload = vec![0u8; len - 1];
    r.read_exact(&mut payload).await?;
    Ok(Some((tag[0], payload)))
}

/// One command over the control socket: a command name plus its JSON args
/// object. Mirrors the GUI's Tauri command boundary — `cmd` is the command
/// name and `args` the (named) parameters as a JSON object.
#[derive(Debug, Serialize, Deserialize)]
pub struct NodeRequest {
    /// The command name (`scan_self`, `connect_route`, …) or the
    /// [`SUBSCRIBE_EVENTS`] sentinel.
    pub cmd: String,
    /// The command's named arguments, as a JSON object (empty for no-arg
    /// commands).
    #[serde(default)]
    pub args: Value,
}

/// The reserved `cmd` that turns a connection into the long-lived event
/// stream instead of a one-shot command.
pub const SUBSCRIBE_EVENTS: &str = "__subscribe_events";

/// One engine event as it travels the event connection — either an
/// `emit(event, payload)` from the `UiSink`, or the relaunch signal.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodeEvent {
    /// A `UiSink::emit` — one named event + its JSON payload.
    Emit { event: String, payload: Value },
    /// An attached desktop must update in the GUI's own install context.
    Upgrade,
    /// The node is re-execing onto a staged update (`UiSink::restart`).
    Restart,
}

/// The body of a normal (`TAG_JSON`) response frame.
#[derive(Debug, Serialize, Deserialize)]
pub struct WireResponse {
    pub(crate) ok: bool,
    #[serde(default)]
    pub(crate) result: Value,
    #[serde(default)]
    pub(crate) error: Option<String>,
}

impl WireResponse {
    pub fn ok(result: Value) -> Self {
        Self {
            ok: true,
            result,
            error: None,
        }
    }

    pub fn err(error: String) -> Self {
        Self {
            ok: false,
            result: Value::Null,
            error: Some(error),
        }
    }
}
