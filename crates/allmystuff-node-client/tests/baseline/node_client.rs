// Frozen compatibility oracle from 1bb9bff6a3d7598565513bd4496bab8faf3b4a8f.
// See manifest.json for exact source identity and the endpoint-only adaptation.
// Do not update this oracle to agree with an extracted implementation.
#![allow(dead_code)]

#[cfg(unix)]
use std::path::PathBuf;
use anyhow::{anyhow, bail, Context, Result};
use interprocess::local_socket::tokio::prelude::*;
#[cfg(unix)]
use interprocess::local_socket::GenericFilePath;
#[cfg(not(unix))]
use interprocess::local_socket::GenericNamespaced;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

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
const MAX_FRAME_LEN: usize = 256 * 1024 * 1024;

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
/// clean EOF *before any byte of a frame* — a peer that hung up between frames,
/// not a truncated one (a partial frame is an error). Rejects a length past
/// [`MAX_FRAME_LEN`] before allocating.
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

// ---------------------------------------------------------------------------
// Request / response shapes
// ---------------------------------------------------------------------------

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
/// `emit(event, payload)` from the [`UiSink`], or the relaunch signal.
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

enum SocketAddr {
    #[cfg(unix)]
    Path(PathBuf),
    #[cfg(not(unix))]
    Name(String),
}

impl SocketAddr {
    /// This address as an interprocess [`Name`], for connect or bind.
    fn to_name(&self) -> Result<interprocess::local_socket::Name<'_>> {
        match self {
            #[cfg(unix)]
            SocketAddr::Path(p) => p
                .as_path()
                .to_fs_name::<GenericFilePath>()
                .context("node socket path → fs_name"),
            #[cfg(not(unix))]
            SocketAddr::Name(n) => n
                .as_str()
                .to_ns_name::<GenericNamespaced>()
                .context("node socket name → ns_name"),
        }
    }

    /// The on-disk socket file, on unix — for clearing a stale one before a
    /// fresh bind (daemons do the same).
    #[cfg(unix)]
    fn path(&self) -> &std::path::Path {
        match self {
            SocketAddr::Path(p) => p.as_path(),
        }
    }
}

pub struct NodeClient {
    addr: SocketAddr,
}

impl NodeClient {
    #[cfg(unix)]
    pub fn for_fixture(path: PathBuf) -> Self { Self { addr: SocketAddr::Path(path) } }
    #[cfg(not(unix))]
    pub fn for_fixture(name: String) -> Self { Self { addr: SocketAddr::Name(name) } }

    async fn connect(&self) -> Result<LocalSocketStream> {
        let name = self.addr.to_name()?;
        LocalSocketStream::connect(name)
            .await
            .context("connect node socket — is `allmystuff-serve` running?")
    }

    /// One-shot command → JSON result. Opens a connection, writes one
    /// [`NodeRequest`] as a [`TAG_JSON`] frame, reads one `TAG_JSON` response,
    /// and returns its `result` (or errors with `error`).
    pub async fn request(&self, cmd: &str, args: Value) -> Result<Value> {
        let (tag, payload) = self.round_trip(cmd, args).await?;
        if tag != TAG_JSON {
            bail!("node sent a {tag} frame where a JSON response was expected");
        }
        let resp: WireResponse = serde_json::from_slice(&payload).context("parse node response")?;
        if resp.ok {
            Ok(resp.result)
        } else {
            Err(anyhow!(resp.error.unwrap_or_else(|| "(no error)".into())))
        }
    }

    /// One-shot command → raw bytes (the poll commands). Same as
    /// [`NodeClient::request`] but expects a [`TAG_BYTES`] response.
    pub async fn request_bytes(&self, cmd: &str, args: Value) -> Result<Vec<u8>> {
        let (tag, payload) = self.round_trip(cmd, args).await?;
        match tag {
            TAG_BYTES => Ok(payload),
            // A failed poll still comes back as a JSON error frame.
            TAG_JSON => {
                let resp: WireResponse =
                    serde_json::from_slice(&payload).context("parse node response")?;
                Err(anyhow!(resp.error.unwrap_or_else(|| {
                    "node returned JSON where bytes were expected".into()
                })))
            }
            other => bail!("node sent a {other} frame where raw bytes were expected"),
        }
    }

    /// Connect, send the request, read exactly one response frame, close.
    async fn round_trip(&self, cmd: &str, args: Value) -> Result<(u8, Vec<u8>)> {
        let stream = self.connect().await?;
        let (mut reader, mut writer) = stream.split();
        let body = serde_json::to_vec(&NodeRequest {
            cmd: cmd.to_string(),
            args,
        })?;
        write_frame(&mut writer, TAG_JSON, &body)
            .await
            .context("write node request")?;
        read_frame(&mut reader)
            .await
            .context("read node response")?
            .ok_or_else(|| anyhow!("node closed the connection without a response"))
    }

    /// Subscribe to the node's event stream: connect, send the subscribe
    /// sentinel, await the ack, then spawn a read loop forwarding each
    /// [`NodeEvent`] to `tx` until EOF. Returns once the ack lands.
    pub async fn subscribe_events(&self, tx: mpsc::Sender<NodeEvent>) -> Result<()> {
        let stream = self.connect().await?;
        let (mut reader, mut writer) = stream.split();
        let body = serde_json::to_vec(&NodeRequest {
            cmd: SUBSCRIBE_EVENTS.to_string(),
            args: Value::Null,
        })?;
        write_frame(&mut writer, TAG_JSON, &body)
            .await
            .context("write node subscribe")?;

        // The ack — a TAG_JSON `{ok:true}` — confirms we're registered.
        let (tag, payload) = read_frame(&mut reader)
            .await
            .context("read subscribe ack")?
            .ok_or_else(|| anyhow!("node closed the connection before the subscribe ack"))?;
        if tag != TAG_JSON {
            bail!("subscribe ack wasn't a JSON frame");
        }
        let ack: WireResponse = serde_json::from_slice(&payload).context("parse subscribe ack")?;
        if !ack.ok {
            return Err(anyhow!(
                "subscribe rejected: {}",
                ack.error.unwrap_or_else(|| "(no error)".into())
            ));
        }

        tokio::spawn(async move {
            // Keep the writer half alive for the read loop's lifetime.
            let _writer_keepalive = writer;
            loop {
                match read_frame(&mut reader).await {
                    Ok(Some((TAG_EVENT, body))) => {
                        match serde_json::from_slice::<NodeEvent>(&body) {
                            Ok(ev) => {
                                if tx.send(ev).await.is_err() {
                                    break;
                                }
                            }
                            Err(e) => tracing::warn!("malformed node event: {e}"),
                        }
                    }
                    Ok(Some((TAG_RESTART, _))) => {
                        let _ = tx.send(NodeEvent::Restart).await;
                        break;
                    }
                    Ok(Some((tag, _))) => {
                        tracing::warn!("unexpected node event frame tag {tag}");
                    }
                    Ok(None) => break,
                    Err(e) => {
                        tracing::warn!("node event stream read failed: {e}");
                        break;
                    }
                }
            }
        });
        Ok(())
    }
}

/// The body of a normal (`TAG_JSON`) response frame.
#[derive(Debug, Serialize, Deserialize)]
struct WireResponse {
    ok: bool,
    #[serde(default)]
    result: Value,
    #[serde(default)]
    error: Option<String>,
}
