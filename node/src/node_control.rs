//! The node's own local control + event socket — the per-machine seam a thin
//! GUI drives instead of running its own in-process [`Mesh`].
//!
//! AllMyStuff is converging on **one node per machine** ([`crate::instance`]):
//! the headless `allmystuff-serve` binary owns the live [`Mesh`], and a future
//! GUI becomes a thin client that issues commands over *this* socket rather
//! than linking the engine and supervising its own daemon. This module is the
//! node side of that link — purely additive plumbing layered on top of the
//! engine, mirroring the shapes [`crate::control_client`] already uses to talk
//! to the `myownmesh` daemon:
//!
//!  * [`NodeClient::request`] / [`NodeClient::request_bytes`] — one short-lived
//!    round trip per command (like [`ControlClient::request`]).
//!  * [`NodeClient::subscribe_events`] — a long-lived stream of engine events
//!    (like [`ControlClient::subscribe_events`]).
//!  * [`serve`] — the accept loop the node runs, dispatching commands to its
//!    [`Mesh`] / [`ControlClient`] / [`DisabledNetworks`] and fanning engine
//!    events out to every subscribed client through a [`SocketSink`].
//!
//! The wire is **length-prefixed frames** (`[u32 BE len][1 tag byte][payload]`)
//! rather than the daemon's newline-JSON, because the poll commands and event
//! payloads carry raw binary (media batches) that newline framing can't.
//!
//! [`ControlClient`]: crate::control_client::ControlClient
//! [`ControlClient::request`]: crate::control_client::ControlClient::request
//! [`ControlClient::subscribe_events`]: crate::control_client::ControlClient::subscribe_events
//! [`Mesh`]: crate::mesh::Mesh

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use interprocess::local_socket::tokio::prelude::*;
use interprocess::local_socket::tokio::Listener;
#[cfg(unix)]
use interprocess::local_socket::GenericFilePath;
#[cfg(not(unix))]
use interprocess::local_socket::GenericNamespaced;
use interprocess::local_socket::ListenerOptions;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, Mutex};

use allmystuff_graph::{Grant, NodeId, Person, PersonId};
use allmystuff_session::{FileEvent, InputAction, TermEvent};

use crate::control_client::{ControlClient, Request};
use crate::mesh::Mesh;
use crate::networks_store::DisabledNetworks;
use crate::UiSink;

// ---------------------------------------------------------------------------
// Wire protocol
// ---------------------------------------------------------------------------

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
    /// The node is re-execing onto a staged update (`UiSink::restart`).
    Restart,
}

/// What [`dispatch`] produces for one command: a JSON result, a raw-bytes
/// result (the poll commands), or an error string.
pub enum DispatchOut {
    /// A normal command's JSON result.
    Json(Value),
    /// A poll command's raw media batch.
    Bytes(Vec<u8>),
    /// The command failed; the string is surfaced to the client as `error`.
    Err(String),
}

// ---------------------------------------------------------------------------
// Socket addressing
// ---------------------------------------------------------------------------

/// Where the node's control socket lives — distinct from the `myownmesh`
/// daemon socket (this is AllMyStuff's own per-machine node, not the mesh
/// daemon). Mirrors [`crate::control_client`]'s addressing.
enum SocketAddr {
    #[cfg(unix)]
    Path(PathBuf),
    #[cfg(not(unix))]
    Name(String),
}

/// On Windows the namespaced pipe name. (On unix the socket is a file path
/// under the `~/.myownmesh` home; see [`node_socket_addr`].)
#[cfg(not(unix))]
const NODE_SOCKET_NAME: &str = "allmystuff-node";

/// Resolve the node control socket address. On unix it's
/// `<myownmesh_home>/allmystuff-node.sock` — the *same* `~/.myownmesh` home
/// (honoring `MYOWNMESH_HOME`) the ownership store and networks store use; on
/// Windows it's a namespaced pipe. Distinct from the daemon socket either way.
fn node_socket_addr() -> Result<SocketAddr> {
    #[cfg(unix)]
    {
        let home = std::env::var_os("MYOWNMESH_HOME")
            .map(PathBuf::from)
            .or_else(dirs::home_dir)
            .map(|h| h.join(".myownmesh"))
            .context("resolve the ~/.myownmesh home for the node socket")?;
        Ok(SocketAddr::Path(home.join("allmystuff-node.sock")))
    }
    #[cfg(not(unix))]
    {
        Ok(SocketAddr::Name(NODE_SOCKET_NAME.to_string()))
    }
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

// ---------------------------------------------------------------------------
// NodeClient — the GUI's (and the tests') side of the wire
// ---------------------------------------------------------------------------

/// Client of a running node's control socket. The GUI uses this in Phase B to
/// drive the node it no longer runs in-process; the tests use it to exercise
/// [`serve`]. Cheap to clone the address; every call opens its own connection
/// (a local round trip is cheap and pooling muddies node-restart semantics —
/// same reasoning as [`ControlClient`](crate::control_client::ControlClient)).
pub struct NodeClient {
    addr: SocketAddr,
}

impl NodeClient {
    /// Resolve the node socket address (does not connect).
    pub fn new() -> Result<Self> {
        Ok(Self {
            addr: node_socket_addr()?,
        })
    }

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

    /// True when a node is already listening on the control socket.
    pub async fn probe() -> bool {
        let Ok(client) = NodeClient::new() else {
            return false;
        };
        client.connect().await.is_ok()
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

impl WireResponse {
    fn ok(result: Value) -> Self {
        Self {
            ok: true,
            result,
            error: None,
        }
    }

    fn err(error: String) -> Self {
        Self {
            ok: false,
            result: Value::Null,
            error: Some(error),
        }
    }
}

// ---------------------------------------------------------------------------
// SocketSink — the node's UiSink, fanning events to every event connection
// ---------------------------------------------------------------------------

/// The subscribed event connections' senders — the fan-out task's registry,
/// shared with [`serve`]'s accept loop (each event connection pushes its
/// sender here, [`fan_out`] writes to them).
pub type Broadcaster = Arc<Mutex<Vec<mpsc::Sender<NodeEvent>>>>;

/// Build a fresh, empty broadcaster.
pub fn new_broadcaster() -> Broadcaster {
    Arc::new(Mutex::new(Vec::new()))
}

/// Create the ordered event hand-off: the sender a [`SocketSink`] pushes every
/// engine event into, and the receiver [`serve`]'s [`fan_out`] task drains.
/// Unbounded so `emit` (the [`UiSink`] contract is non-blocking) never stalls
/// the engine; FIFO so events reach subscribers in the order they happened.
pub fn event_channel() -> (
    mpsc::UnboundedSender<NodeEvent>,
    mpsc::UnboundedReceiver<NodeEvent>,
) {
    mpsc::unbounded_channel()
}

/// The node's [`UiSink`]: every engine event is both logged (via the wrapped
/// `inner` sink — the binary's `LogSink`) **and** handed to the fan-out task,
/// which streams it to every connected event subscriber, so a thin GUI sees
/// exactly what the headless node logs.
///
/// `restart` is delegated: the node binary owns re-exec, so [`SocketSink`]
/// signals a [`NodeEvent::Restart`] to subscribers (so a GUI can relaunch its
/// window) and then hands off to `inner.restart()`, which never returns.
pub struct SocketSink {
    /// The wrapped sink — the binary's `LogSink`, which owns re-exec.
    inner: Arc<dyn UiSink>,
    /// Ordered hand-off to the fan-out task. `emit` is called from many engine
    /// tasks at once; funnelling every event through one FIFO queue (rather
    /// than spawning a task per event, which the runtime may then reorder) is
    /// what keeps them in order on the wire — a stale session snapshot
    /// arriving *after* a newer one would mis-paint the GUI.
    tx: mpsc::UnboundedSender<NodeEvent>,
}

impl SocketSink {
    /// Wrap `inner` (the node binary's `LogSink`); events flow through `tx` to
    /// the fan-out task [`serve`] runs. Build the pair with [`event_channel`].
    pub fn new(inner: Arc<dyn UiSink>, tx: mpsc::UnboundedSender<NodeEvent>) -> Self {
        Self { inner, tx }
    }
}

impl UiSink for SocketSink {
    fn emit(&self, event: &str, payload: Value) {
        self.inner.emit(event, payload.clone());
        // Non-blocking + ordered: a dropped receiver (no fan-out running) just
        // discards, exactly like a UI with no listener.
        let _ = self.tx.send(NodeEvent::Emit {
            event: event.to_string(),
            payload,
        });
    }

    fn restart(&self) -> ! {
        // Tell subscribers to relaunch before we re-exec, give the fan-out a
        // beat to flush it, then delegate to the inner sink (re-execs, never
        // returns).
        let _ = self.tx.send(NodeEvent::Restart);
        std::thread::sleep(Duration::from_millis(100));
        self.inner.restart()
    }
}

/// Drain the ordered event queue and fan each event out to every subscribed
/// connection, in order. One task, one queue — so all subscribers observe
/// events in the same order the engine produced them. A subscriber whose buffer
/// is full loses the event (`try_send`, never block the fan-out) rather than
/// stalling every other subscriber; a disconnected one is reaped.
async fn fan_out(mut rx: mpsc::UnboundedReceiver<NodeEvent>, broadcaster: Broadcaster) {
    while let Some(ev) = rx.recv().await {
        let mut subs = broadcaster.lock().await;
        subs.retain(|tx| match tx.try_send(ev.clone()) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => true, // alive, just behind
            Err(mpsc::error::TrySendError::Closed(_)) => false, // gone — reap it
        });
    }
}

// ---------------------------------------------------------------------------
// The server
// ---------------------------------------------------------------------------

/// Bind the node control socket, enforcing **one node per machine**: the bind
/// itself is the guard (there is no separate lock). If a *live* node already
/// answers the socket, this machine is already served — the bind fails with an
/// error the caller treats as "step aside, don't start a second mesh". A
/// *stale* socket file (a crashed node) is cleared and the bind retried.
///
/// This is race-safe: two nodes starting at once both try to create the name;
/// the first wins and the second's create fails, probes the now-live winner,
/// and steps aside.
pub async fn bind_control_socket() -> Result<Listener> {
    let addr = node_socket_addr()?;
    match ListenerOptions::new().name(addr.to_name()?).create_tokio() {
        Ok(listener) => Ok(listener),
        Err(_) => {
            // The name is taken. A node that answers owns the machine; a name
            // taken by nothing live is a corpse from a crash — clear it and
            // bind once more.
            if NodeClient::probe().await {
                bail!("another allmystuff node already owns this machine's control socket");
            }
            #[cfg(unix)]
            {
                let _ = std::fs::remove_file(addr.path());
            }
            ListenerOptions::new()
                .name(addr.to_name()?)
                .create_tokio()
                .context("bind the node control socket")
        }
    }
}

/// Accept connections on an already-bound `listener` forever, each on its own
/// task, and run the [`fan_out`] task that streams engine events to
/// subscribers. The first frame of every connection is a [`NodeRequest`]: the
/// [`SUBSCRIBE_EVENTS`] sentinel turns it into a long-lived event stream;
/// anything else is dispatched as a one-shot command and the connection closes
/// after its response.
pub async fn serve(
    listener: Listener,
    mesh: Arc<Mesh>,
    client: Arc<ControlClient>,
    disabled: Arc<DisabledNetworks>,
    broadcaster: Broadcaster,
    event_rx: mpsc::UnboundedReceiver<NodeEvent>,
) -> Result<()> {
    // Drain the engine's ordered event queue out to every subscribed client.
    tokio::spawn(fan_out(event_rx, broadcaster.clone()));
    tracing::info!("node control socket listening");

    loop {
        let stream = match listener.accept().await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("node control accept failed: {e}");
                continue;
            }
        };
        let mesh = mesh.clone();
        let client = client.clone();
        let disabled = disabled.clone();
        let broadcaster = broadcaster.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, mesh, client, disabled, broadcaster).await {
                tracing::debug!("node control connection ended: {e:#}");
            }
        });
    }
}

/// Serve one connection: read its first [`NodeRequest`], then either run the
/// event-writer loop (subscribe) or dispatch one command and reply.
async fn handle_connection(
    stream: LocalSocketStream,
    mesh: Arc<Mesh>,
    client: Arc<ControlClient>,
    disabled: Arc<DisabledNetworks>,
    broadcaster: Broadcaster,
) -> Result<()> {
    let (mut reader, mut writer) = stream.split();
    let Some((tag, body)) = read_frame(&mut reader).await? else {
        // Clean hangup before sending anything — nothing to do.
        return Ok(());
    };
    if tag != TAG_JSON {
        bail!("first node frame wasn't a JSON request (tag {tag})");
    }
    let req: NodeRequest = serde_json::from_slice(&body).context("parse node request")?;

    if req.cmd == SUBSCRIBE_EVENTS {
        return run_event_writer(writer, broadcaster).await;
    }

    let out = dispatch(&mesh, &client, &disabled, req).await;
    match out {
        DispatchOut::Json(v) => {
            let body = serde_json::to_vec(&WireResponse::ok(v))?;
            write_frame(&mut writer, TAG_JSON, &body).await?;
        }
        DispatchOut::Bytes(b) => {
            write_frame(&mut writer, TAG_BYTES, &b).await?;
        }
        DispatchOut::Err(e) => {
            let body = serde_json::to_vec(&WireResponse::err(e))?;
            write_frame(&mut writer, TAG_JSON, &body).await?;
        }
    }
    Ok(())
}

/// Register this connection in the broadcaster, ack, then drain its receiver
/// and write each [`NodeEvent`] as a frame until the client disconnects.
async fn run_event_writer<W: AsyncWrite + Unpin>(
    mut writer: W,
    broadcaster: Broadcaster,
) -> Result<()> {
    // A small buffer: an event-flooded-but-slow subscriber sheds load (the
    // sink's `try_send` drops) rather than growing memory without bound.
    let (tx, mut rx) = mpsc::channel::<NodeEvent>(256);
    broadcaster.lock().await.push(tx);

    let ack = serde_json::to_vec(&WireResponse::ok(Value::Null))?;
    write_frame(&mut writer, TAG_JSON, &ack)
        .await
        .context("write subscribe ack")?;

    while let Some(ev) = rx.recv().await {
        match ev {
            NodeEvent::Restart => {
                let _ = write_frame(&mut writer, TAG_RESTART, &[]).await;
                break;
            }
            other => {
                let body = serde_json::to_vec(&other)?;
                if write_frame(&mut writer, TAG_EVENT, &body).await.is_err() {
                    // The client went away; let the dead sender be reaped on
                    // the next broadcast (`try_send`/`is_closed` retain check).
                    break;
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// dispatch — one command → DispatchOut
// ---------------------------------------------------------------------------

/// Pull a required arg from the request's args object, deserializing into `T`.
fn arg<T: serde::de::DeserializeOwned>(args: &Value, key: &str) -> Result<T, String> {
    let v = args
        .get(key)
        .ok_or_else(|| format!("missing argument: {key}"))?;
    serde_json::from_value(v.clone()).map_err(|e| format!("bad argument {key}: {e}"))
}

/// Pull an optional arg (absent or `null` → `None`).
fn opt<T: serde::de::DeserializeOwned>(args: &Value, key: &str) -> Result<Option<T>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => serde_json::from_value(v.clone())
            .map(Some)
            .map_err(|e| format!("bad argument {key}: {e}")),
    }
}

/// Map `Result<T, String>` from a Mesh method to a JSON-or-error `DispatchOut`,
/// serializing the success value.
fn json_result<T: Serialize>(r: Result<T, String>) -> DispatchOut {
    match r {
        Ok(v) => match serde_json::to_value(v) {
            Ok(j) => DispatchOut::Json(j),
            Err(e) => DispatchOut::Err(e.to_string()),
        },
        Err(e) => DispatchOut::Err(e),
    }
}

/// Run one command against the live node. Argument names mirror the GUI's
/// Tauri command parameters; return types mirror the underlying [`Mesh`] /
/// [`ControlClient`] / [`DisabledNetworks`] methods.
pub async fn dispatch(
    mesh: &Arc<Mesh>,
    client: &Arc<ControlClient>,
    disabled: &Arc<DisabledNetworks>,
    req: NodeRequest,
) -> DispatchOut {
    let a = &req.args;
    // A tiny helper to bail out of arg parsing into a DispatchOut::Err.
    macro_rules! try_arg {
        ($e:expr) => {
            match $e {
                Ok(v) => v,
                Err(e) => return DispatchOut::Err(e),
            }
        };
    }

    match req.cmd.as_str() {
        // ---- this machine ------------------------------------------------
        "scan_self" => {
            let me = mesh
                .resolve_local_id()
                .await
                .unwrap_or_else(|| "this".to_string());
            let node = NodeId::from(me.as_str());
            let inv = allmystuff_inventory::scan();
            DispatchOut::Json(json!({
                "node_id": me,
                "label": inv.host.hostname,
                "hostname": inv.host.hostname,
                "summary": allmystuff_bridge::node_summary(&inv),
                "capabilities": allmystuff_bridge::capabilities_with_screens(
                    &inv,
                    &node,
                    &crate::video::extra_screens(),
                ),
            }))
        }

        // ---- live mesh (presence + routing) ------------------------------
        "connect_route" => {
            let from: String = try_arg!(arg(a, "from"));
            let to: String = try_arg!(arg(a, "to"));
            let media: String = try_arg!(arg(a, "media"));
            let video: Option<Vec<String>> = try_arg!(opt(a, "video"));
            let session: Option<String> = try_arg!(opt(a, "session"));
            json_result(
                mesh.connect_term(from, to, media, video.unwrap_or_default(), session)
                    .await,
            )
        }
        "disconnect_route" => {
            let route_id: String = try_arg!(arg(a, "route_id"));
            json_result(mesh.disconnect(route_id).await)
        }
        "claim_node" => {
            let node: String = try_arg!(arg(a, "node"));
            json_result(mesh.claim(node).await)
        }
        "upgrade_node" => {
            let node: String = try_arg!(arg(a, "node"));
            json_result(mesh.request_upgrade(node).await)
        }
        "set_claimable" => {
            let claimable: bool = try_arg!(arg(a, "claimable"));
            json_result(mesh.set_claimable(claimable).await)
        }

        // ---- shares ------------------------------------------------------
        "share_grant" => {
            let person: Person = try_arg!(arg(a, "person"));
            let node: String = try_arg!(arg(a, "node"));
            let grant: Grant = try_arg!(arg(a, "grant"));
            json_result(mesh.share_grant(person, node.into(), grant).await)
        }
        "share_revoke" => {
            let person: String = try_arg!(arg(a, "person"));
            let grant_id: String = try_arg!(arg(a, "grant_id"));
            json_result(mesh.share_revoke(PersonId::from(person), grant_id).await)
        }
        "share_stop" => {
            let person: String = try_arg!(arg(a, "person"));
            json_result(mesh.share_stop(PersonId::from(person)).await)
        }

        // ---- input + clipboard ------------------------------------------
        "send_input" => {
            let route_id: String = try_arg!(arg(a, "route_id"));
            let action: InputAction = try_arg!(arg(a, "action"));
            json_result(mesh.send_input(route_id, action).await)
        }
        "clipboard_paste" => {
            let route_id: String = try_arg!(arg(a, "route_id"));
            json_result(mesh.clipboard_paste(route_id).await)
        }

        // ---- video plane -------------------------------------------------
        "video_watch" => {
            let route_id: String = try_arg!(arg(a, "route_id"));
            let decode: Option<bool> = try_arg!(opt(a, "decode"));
            DispatchOut::Json(json!(mesh.video_watch(route_id, decode.unwrap_or(false))))
        }
        "video_poll" => {
            let route_id: String = try_arg!(arg(a, "route_id"));
            DispatchOut::Bytes(mesh.video_poll(&route_id))
        }
        "video_unwatch" => {
            let route_id: String = try_arg!(arg(a, "route_id"));
            let token: u64 = try_arg!(arg(a, "token"));
            mesh.video_unwatch(&route_id, token);
            DispatchOut::Json(Value::Null)
        }
        "video_refresh" => {
            let route_id: String = try_arg!(arg(a, "route_id"));
            json_result(mesh.request_refresh(route_id).await)
        }
        "video_feedback" => {
            let route_id: String = try_arg!(arg(a, "route_id"));
            let recv_fps: u32 = try_arg!(arg(a, "recv_fps"));
            let decode_fails: u32 = try_arg!(arg(a, "decode_fails"));
            let queue_depth: u32 = try_arg!(arg(a, "queue_depth"));
            json_result(
                mesh.send_video_feedback(route_id, recv_fps, decode_fails, queue_depth)
                    .await,
            )
        }
        "tune_route" => {
            let route_id: String = try_arg!(arg(a, "route_id"));
            let max_edge: Option<u32> = try_arg!(opt(a, "max_edge"));
            let bitrate: Option<u32> = try_arg!(opt(a, "bitrate"));
            let fps: Option<u32> = try_arg!(opt(a, "fps"));
            json_result(mesh.request_tune(route_id, max_edge, bitrate, fps).await)
        }

        // ---- terminal plane ----------------------------------------------
        "term_send" => {
            let route_id: String = try_arg!(arg(a, "route_id"));
            let event: TermEvent = try_arg!(arg(a, "event"));
            json_result(mesh.term_send(route_id, event).await)
        }
        "term_watch" => {
            let route_id: String = try_arg!(arg(a, "route_id"));
            DispatchOut::Json(json!(mesh.term_watch(&route_id)))
        }
        "term_poll" => {
            let route_id: String = try_arg!(arg(a, "route_id"));
            DispatchOut::Bytes(mesh.term_poll(&route_id))
        }
        "term_unwatch" => {
            let route_id: String = try_arg!(arg(a, "route_id"));
            let token: u64 = try_arg!(arg(a, "token"));
            mesh.term_unwatch(&route_id, token);
            DispatchOut::Json(Value::Null)
        }
        "terminal_sessions" => {
            let node: String = try_arg!(arg(a, "node"));
            json_result(mesh.request_terminal_sessions(node).await)
        }

        // ---- files plane -------------------------------------------------
        "file_send" => {
            let route_id: String = try_arg!(arg(a, "route_id"));
            let event: FileEvent = try_arg!(arg(a, "event"));
            json_result(mesh.file_send(route_id, event).await)
        }
        "file_watch" => {
            let route_id: String = try_arg!(arg(a, "route_id"));
            DispatchOut::Json(json!(mesh.file_watch(&route_id)))
        }
        "file_poll" => {
            let route_id: String = try_arg!(arg(a, "route_id"));
            DispatchOut::Bytes(mesh.file_poll(&route_id))
        }
        "file_unwatch" => {
            let route_id: String = try_arg!(arg(a, "route_id"));
            let token: u64 = try_arg!(arg(a, "token"));
            mesh.file_unwatch(&route_id, token);
            DispatchOut::Json(Value::Null)
        }
        "file_download" => {
            let route_id: String = try_arg!(arg(a, "route_id"));
            let req_id: u64 = try_arg!(arg(a, "req"));
            let name: String = try_arg!(arg(a, "name"));
            json_result(mesh.file_download(route_id, req_id, &name))
        }

        // ---- sites (reverse proxy) ---------------------------------------
        "site_scan" => {
            let mesh = mesh.clone();
            match tokio::task::spawn_blocking(move || mesh.site_scan()).await {
                Ok(list) => json_result::<Vec<_>>(Ok(list)),
                Err(e) => DispatchOut::Err(e.to_string()),
            }
        }
        "site_exposed" => {
            json_result::<std::collections::BTreeMap<String, String>>(Ok(mesh.site_exposed()))
        }
        "site_set_exposed" => {
            let exposed: std::collections::BTreeMap<String, String> = try_arg!(arg(a, "exposed"));
            json_result::<std::collections::BTreeMap<String, String>>(Ok(mesh
                .site_set_exposed(exposed)
                .await))
        }
        "site_map" => {
            let node: String = try_arg!(arg(a, "node"));
            let port: u16 = try_arg!(arg(a, "port"));
            match mesh.site_map(node, port).await {
                Ok(local_port) => DispatchOut::Json(json!({ "localPort": local_port })),
                Err(e) => DispatchOut::Err(e),
            }
        }
        "site_unmap" => {
            let node: String = try_arg!(arg(a, "node"));
            let port: u16 = try_arg!(arg(a, "port"));
            json_result(mesh.site_unmap(node, port).await)
        }
        "site_mappings" => {
            let mappings: Vec<Value> = mesh
                .site_mappings()
                .into_iter()
                .map(|(node, port, local_port)| {
                    json!({ "node": node, "port": port, "localPort": local_port })
                })
                .collect();
            DispatchOut::Json(Value::Array(mappings))
        }
        "site_remote_list" => {
            let node: String = try_arg!(arg(a, "node"));
            json_result(mesh.site_remote_list(node).await)
        }
        "site_remote_set_exposed" => {
            let node: String = try_arg!(arg(a, "node"));
            let exposed: std::collections::BTreeMap<String, String> = try_arg!(arg(a, "exposed"));
            json_result(mesh.site_remote_set_exposed(node, exposed).await)
        }

        // ---- session + fleet + rooms -------------------------------------
        "session_snapshot" => DispatchOut::Json(mesh.snapshot()),
        "room_send" => {
            let members: Vec<String> = try_arg!(arg(a, "members"));
            let message: allmystuff_protocol::RoomMessage = try_arg!(arg(a, "message"));
            json_result(mesh.room_send(members, message).await)
        }
        "room_share_files" => {
            let members: Vec<String> = try_arg!(arg(a, "members"));
            let paths: Vec<String> = try_arg!(arg(a, "paths"));
            json_result::<Vec<_>>(Ok(mesh.room_share_files(members, paths)))
        }
        "room_set_share_peers" => {
            let tokens: Vec<String> = try_arg!(arg(a, "tokens"));
            let members: Vec<String> = try_arg!(arg(a, "members"));
            mesh.room_set_share_peers(tokens, members);
            DispatchOut::Json(Value::Null)
        }
        "room_unshare" => {
            let tokens: Vec<String> = try_arg!(arg(a, "tokens"));
            mesh.room_unshare(tokens);
            DispatchOut::Json(Value::Null)
        }
        "owned_roster" => DispatchOut::Json(mesh.owned_roster_value()),
        "fleet_leave" => json_result(mesh.fleet_leave().await),
        "fleet_kick" => {
            let device: String = try_arg!(arg(a, "device"));
            json_result(mesh.fleet_kick(device).await)
        }
        "fleet_set_name" => {
            let name: String = try_arg!(arg(a, "name"));
            json_result(mesh.fleet_set_name(name).await)
        }

        // ---- daemon passthroughs ----------------------------------------
        "mesh_status" => daemon_request(client, Request::Status).await,
        "mesh_identity" => daemon_request(client, Request::IdentityShow).await,
        "mesh_networks" => daemon_request(client, Request::NetworksList).await,
        "mesh_peers" => {
            let network: String = try_arg!(arg(a, "network"));
            daemon_request(client, Request::PeersList { network }).await
        }
        "mesh_config_show" => daemon_request(client, Request::ConfigShow).await,
        "mesh_roster_approve" => {
            let network: String = try_arg!(arg(a, "network"));
            let device_id: String = try_arg!(arg(a, "device_id"));
            let label: Option<String> = try_arg!(opt(a, "label"));
            daemon_request(
                client,
                Request::RosterApprove {
                    network,
                    device_id,
                    label,
                },
            )
            .await
        }
        "mesh_roster_remove" => {
            let network: String = try_arg!(arg(a, "network"));
            let device_id: String = try_arg!(arg(a, "device_id"));
            daemon_request(client, Request::RosterRemove { network, device_id }).await
        }
        "mesh_roster_list" => {
            let network: String = try_arg!(arg(a, "network"));
            daemon_request(client, Request::RosterList { network }).await
        }
        "mesh_network_id_generate" => daemon_request(client, Request::NetworkIdGenerate).await,
        "mesh_network_add" => {
            let config: Value = try_arg!(arg(a, "config"));
            sync_after(
                mesh,
                daemon_request(client, Request::NetworkAdd { config }).await,
            )
            .await
        }
        "mesh_network_update" => {
            let config: Value = try_arg!(arg(a, "config"));
            sync_after(
                mesh,
                daemon_request(client, Request::NetworkUpdate { config }).await,
            )
            .await
        }
        "mesh_network_remove" => {
            let network: String = try_arg!(arg(a, "network"));
            sync_after(
                mesh,
                daemon_request(client, Request::NetworkRemove { network }).await,
            )
            .await
        }
        "mesh_identity_set_label" => {
            let label: String = try_arg!(arg(a, "label"));
            let out = daemon_request(
                client,
                Request::IdentitySetLabel {
                    label: label.clone(),
                },
            )
            .await;
            if let DispatchOut::Json(_) = &out {
                mesh.set_label(label).await;
            }
            out
        }

        // ---- park store --------------------------------------------------
        "disabled_networks" => DispatchOut::Json(Value::Array(disabled.list())),
        "network_set_enabled" => {
            let network: String = try_arg!(arg(a, "network"));
            let enabled: bool = try_arg!(arg(a, "enabled"));
            network_set_enabled(mesh, client, disabled, network, enabled).await
        }

        other => DispatchOut::Err(format!("unknown node command: {other}")),
    }
}

/// One daemon round trip, unwrapped into a `DispatchOut`: `!ok` → `Err`,
/// else the response data (or `null`) as JSON. Mirrors the GUI's
/// `unwrap_response`.
async fn daemon_request(client: &Arc<ControlClient>, req: Request) -> DispatchOut {
    match client.request(&req).await {
        Ok(resp) if resp.ok => DispatchOut::Json(resp.data.unwrap_or(Value::Null)),
        Ok(resp) => DispatchOut::Err(resp.error.unwrap_or_else(|| "(no error message)".into())),
        Err(e) => DispatchOut::Err(e.to_string()),
    }
}

/// After a successful network add/update/remove, re-subscribe + re-advertise
/// so the change lights up this session immediately (mirrors the GUI's
/// `sync_networks` call after each). On error, pass the error through
/// untouched.
async fn sync_after(mesh: &Arc<Mesh>, out: DispatchOut) -> DispatchOut {
    if let DispatchOut::Json(_) = &out {
        mesh.sync_networks().await;
    }
    out
}

/// Switch a network off or back on without deleting it — the exact body of the
/// GUI's `network_set_enabled` command (gui/src-tauri/src/main.rs). Enable:
/// take the parked config, hand it back to the daemon, sync (re-park on
/// failure). Disable: snapshot the full config from `config_show`, park it,
/// leave the daemon, sync (un-park on failure).
async fn network_set_enabled(
    mesh: &Arc<Mesh>,
    client: &Arc<ControlClient>,
    disabled: &Arc<DisabledNetworks>,
    network: String,
    enabled: bool,
) -> DispatchOut {
    if enabled {
        let Some(config) = disabled.take(&network) else {
            return DispatchOut::Err(format!("'{network}' isn't a disabled network here"));
        };
        let rejoin = daemon_request(
            client,
            Request::NetworkAdd {
                config: config.clone(),
            },
        )
        .await;
        match rejoin {
            DispatchOut::Json(data) => {
                mesh.sync_networks().await;
                DispatchOut::Json(data)
            }
            other => {
                // Park it back so a failed re-join never loses the config.
                disabled.park(config);
                other
            }
        }
    } else {
        // Snapshot the full config *before* leaving — `config_show` is the
        // only place the daemon hands the whole thing back.
        let shown = match daemon_request(client, Request::ConfigShow).await {
            DispatchOut::Json(v) => v,
            other => return other,
        };
        let config = shown
            .pointer("/config/networks")
            .and_then(|v| v.as_array())
            .and_then(|nets| {
                nets.iter()
                    .find(|n| {
                        let id = n.get("id").and_then(|v| v.as_str()).unwrap_or_default();
                        let nid = n
                            .get("network_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default();
                        id == network || nid == network
                    })
                    .cloned()
            });
        let Some(config) = config else {
            return DispatchOut::Err(format!("unknown network: {network}"));
        };
        if !disabled.park(config) {
            return DispatchOut::Err(
                "couldn't save the network for later — not disabling it".into(),
            );
        }
        let left = daemon_request(
            client,
            Request::NetworkRemove {
                network: network.clone(),
            },
        )
        .await;
        match left {
            DispatchOut::Json(data) => {
                mesh.sync_networks().await;
                DispatchOut::Json(data)
            }
            other => {
                // Still joined — un-park so the books match reality.
                let _ = disabled.take(&network);
                other
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ensure_node_running — spawn + probe an `allmystuff-serve` node
// ---------------------------------------------------------------------------

/// Owned wrapper around a spawned `allmystuff-serve` node child. Dropping it
/// kills the child (mirrors [`crate::daemon_spawn::DaemonChild`]).
pub struct NodeChild {
    child: Option<Child>,
}

impl Drop for NodeChild {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
            tracing::info!("allmystuff node child terminated");
        }
    }
}

/// Tie the spawned node's lifetime to this process at the OS level, so a crash
/// or force-kill of the parent doesn't orphan the node (which would keep this
/// machine's identity live and swallow its traffic). Linux uses
/// `PR_SET_PDEATHSIG` (set in `pre_exec` at spawn); Windows a kill-on-close
/// job object; macOS relies on the `Drop` kill for clean exits. Mirrors
/// [`crate::daemon_spawn`]'s tie.
#[cfg(windows)]
fn tie_node_lifetime(child: &Child) {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            tracing::warn!("couldn't create a job object for the node — a crash may orphan it");
            return;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) != 0
            && AssignProcessToJobObject(job, child.as_raw_handle() as _) != 0;
        if ok {
            tracing::info!("node tied to this process (job object, kill-on-close)");
            // Leak the job handle so the kernel's close (on our exit) kills it.
        } else {
            tracing::warn!("couldn't tie the node to this process — a crash may orphan it");
            CloseHandle(job);
        }
    }
}

#[cfg(not(windows))]
fn tie_node_lifetime(_child: &Child) {
    // Linux is handled in `pre_exec` (PR_SET_PDEATHSIG); macOS has no
    // kernel-level equivalent.
}

/// Locate the `allmystuff-serve` node binary: next to the current executable
/// first (`allmystuff-serve{.exe}`), else on `$PATH`. Simpler than
/// [`crate::daemon_spawn::find_daemon_binary`] — the node ships beside us, not
/// as a bundled sidecar.
fn find_node_binary() -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        "allmystuff-serve.exe"
    } else {
        "allmystuff-serve"
    };
    if let Ok(cur) = std::env::current_exe() {
        if let Some(dir) = cur.parent() {
            let p = dir.join(exe);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(exe);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Make sure a node is running, spawning one if not. Returns `Ok(None)` when a
/// node already answers the control socket (we reuse it), `Ok(Some(child))`
/// when we started one (the handle kills it on drop). Mirrors
/// [`crate::daemon_spawn::ensure_daemon_running`]'s shape; the GUI will call
/// this in Phase B.
pub async fn ensure_node_running() -> Result<Option<NodeChild>> {
    if NodeClient::probe().await {
        tracing::info!("existing allmystuff node found on the control socket");
        return Ok(None);
    }

    let bin = find_node_binary().ok_or_else(|| {
        anyhow!(
            "couldn't find the `allmystuff-serve` node binary — it normally ships beside \
             this app; put it on PATH or run `allmystuff serve` yourself"
        )
    })?;
    tracing::info!(?bin, "spawning allmystuff node");

    let mut cmd = Command::new(&bin);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    // Windowless parent: don't give the node its own console window.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    // Linux half of the lifetime tie: SIGKILL the node when this process dies.
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt as _;
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
    }
    let child = cmd
        .spawn()
        .with_context(|| format!("spawn {}", bin.display()))?;
    tie_node_lifetime(&child);
    let handle = NodeChild { child: Some(child) };

    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    while std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(150)).await;
        if NodeClient::probe().await {
            tracing::info!("allmystuff node up");
            return Ok(Some(handle));
        }
    }
    tracing::warn!("node did not answer within 8s; leaving it running — callers will retry");
    Ok(Some(handle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Round-trip a frame through an in-memory duplex pipe and assert the tag
    /// and payload survive intact.
    async fn round_trip(tag: u8, payload: Vec<u8>) {
        let (mut a, mut b) = tokio::io::duplex(1024 * 1024);
        let p2 = payload.clone();
        let writer = tokio::spawn(async move {
            write_frame(&mut a, tag, &p2).await.unwrap();
        });
        let (got_tag, got_payload) = read_frame(&mut b).await.unwrap().expect("a frame");
        writer.await.unwrap();
        assert_eq!(got_tag, tag);
        assert_eq!(got_payload, payload);
    }

    #[tokio::test]
    async fn frame_round_trip_json() {
        let body = serde_json::to_vec(&json!({ "ok": true, "n": 7 })).unwrap();
        round_trip(TAG_JSON, body).await;
    }

    #[tokio::test]
    async fn frame_round_trip_empty_and_bytes() {
        round_trip(TAG_BYTES, Vec::new()).await;
        round_trip(TAG_BYTES, vec![0, 1, 2, 3, 255, 254]).await;
    }

    #[tokio::test]
    async fn frame_round_trip_100kb_blob() {
        let blob: Vec<u8> = (0..100_000u32).map(|i| (i % 256) as u8).collect();
        round_trip(TAG_BYTES, blob).await;
    }

    #[tokio::test]
    async fn read_frame_clean_eof_is_none() {
        let (a, mut b) = tokio::io::duplex(64);
        drop(a); // EOF before any byte of a frame
        assert!(read_frame(&mut b).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn read_frame_rejects_oversized_length() {
        let (mut a, mut b) = tokio::io::duplex(64);
        // A length past the ceiling must error before any allocation.
        let bogus = (MAX_FRAME_LEN as u32 + 1).to_be_bytes();
        a.write_all(&bogus).await.unwrap();
        a.flush().await.unwrap();
        drop(a);
        assert!(read_frame(&mut b).await.is_err());
    }

    #[test]
    fn node_request_serde_round_trip() {
        let req = NodeRequest {
            cmd: "connect_route".into(),
            args: json!({ "from": "a", "to": "b", "media": "video" }),
        };
        let bytes = serde_json::to_vec(&req).unwrap();
        let back: NodeRequest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.cmd, "connect_route");
        assert_eq!(back.args["media"], "video");
        // A request with no args field defaults to Null.
        let bare: NodeRequest = serde_json::from_str(r#"{"cmd":"scan_self"}"#).unwrap();
        assert_eq!(bare.cmd, "scan_self");
        assert_eq!(bare.args, Value::Null);
    }

    #[test]
    fn node_event_serde_round_trip() {
        let emit = NodeEvent::Emit {
            event: "allmystuff://session".into(),
            payload: json!({ "peers": [] }),
        };
        let bytes = serde_json::to_vec(&emit).unwrap();
        match serde_json::from_slice::<NodeEvent>(&bytes).unwrap() {
            NodeEvent::Emit { event, payload } => {
                assert_eq!(event, "allmystuff://session");
                assert_eq!(payload["peers"], json!([]));
            }
            _ => panic!("expected Emit"),
        }
        let restart = serde_json::to_vec(&NodeEvent::Restart).unwrap();
        assert!(matches!(
            serde_json::from_slice::<NodeEvent>(&restart).unwrap(),
            NodeEvent::Restart
        ));
    }
}
