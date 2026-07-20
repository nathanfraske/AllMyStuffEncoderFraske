//! Client half of the `myownmesh` daemon's control protocol. The wire
//! types live in `allmystuff-protocol` (a mirror of the daemon's
//! `control.rs`); this module is just the transport — connect, write one
//! line, read one line — over the local socket (`interprocess`).
//!
//! Three shapes — the first two exactly like the MyOwnMesh GUI's client:
//!
//!  * [`ControlClient::request`] — short-lived round trip for every
//!    one-shot command.
//!  * [`ControlClient::subscribe_events`] — a long-lived stream that
//!    forwards each `ServerOut` line to a channel until the daemon
//!    disconnects.
//!  * [`MediaPipe`] — a long-lived *request* connection for the media
//!    plane, where per-send connect + round-trip would sit inside every
//!    frame.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use interprocess::local_socket::tokio::prelude::*;
#[cfg(unix)]
use interprocess::local_socket::GenericFilePath;
#[cfg(not(unix))]
use interprocess::local_socket::GenericNamespaced;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use allmystuff_protocol::control::{
    decode_inbound_frame, encode_media_frame, InboundFrame, MAX_MEDIA_FRAME_BYTES,
    MEDIA_KIND_AUDIO, MEDIA_KIND_VIDEO,
};
pub use allmystuff_protocol::{Request, Response};

/// A decoded local media-pipe frame plus process-local profiler correlation.
/// `profile_id` is never part of [`InboundFrame`] and is never serialized.
pub(crate) struct ProfiledInboundFrame {
    pub frame: InboundFrame,
    pub profile_id: u64,
    profile_ts_us: Option<u64>,
    enqueued_at: Option<std::time::Instant>,
}

impl ProfiledInboundFrame {
    /// Record residence in the node's bounded media-dispatch queue at the
    /// dequeue boundary. Admission backpressure is measured separately by the
    /// producer, so the two waits do not overlap.
    pub(crate) fn record_dispatch_wait(&mut self) {
        crate::pipeline_profile::record_since(
            &self.frame.from,
            self.profile_id,
            self.profile_ts_us,
            crate::pipeline_profile::Stage::InboundDispatchWait,
            self.enqueued_at.take(),
        );
    }
}

struct InboundProfileAu {
    rtp_timestamp: u32,
    frame_id: u64,
    last_fragment: std::time::Instant,
}

/// Reuse one process-local id for paced fragments of the same access unit.
/// The cache exists only in the opt-in profiler path and is bounded by peer and
/// stream counts; it never changes the decoded frame or any protocol bytes.
fn inbound_profile_id(
    cache: &mut std::collections::HashMap<String, std::collections::HashMap<u8, InboundProfileAu>>,
    frame: &InboundFrame,
) -> u64 {
    const MAX_PEERS: usize = 256;
    const FRAGMENT_TTL: Duration = Duration::from_secs(1);

    let now = std::time::Instant::now();
    if !cache.contains_key(frame.from.as_str()) {
        cache.retain(|_, streams| {
            streams.retain(|_, au| now.saturating_duration_since(au.last_fragment) < FRAGMENT_TTL);
            !streams.is_empty()
        });
        if cache.len() >= MAX_PEERS {
            cache.clear();
        }
    }
    let streams = cache.entry(frame.from.clone()).or_default();
    if let Some(au) = streams.get_mut(&frame.stream) {
        if au.rtp_timestamp == frame.rtp_timestamp
            && now.saturating_duration_since(au.last_fragment) < FRAGMENT_TTL
        {
            au.last_fragment = now;
            return au.frame_id;
        }
    }
    let frame_id = crate::pipeline_profile::next_frame_id();
    streams.insert(
        frame.stream,
        InboundProfileAu {
            rtp_timestamp: frame.rtp_timestamp,
            frame_id,
            last_fragment: now,
        },
    );
    frame_id
}

/// Where the daemon's control socket lives. Recomputed locally (via the
/// protocol crate) so the GUI never has to link `myownmesh-core`.
enum SocketAddr {
    #[cfg(unix)]
    Path(std::path::PathBuf),
    #[cfg(not(unix))]
    Name(String),
}

pub struct ControlClient {
    addr: SocketAddr,
}

impl ControlClient {
    pub fn new() -> Result<Self> {
        #[cfg(unix)]
        {
            let path = allmystuff_protocol::control::default_socket_path()
                .context("resolve daemon socket path")?;
            Ok(Self {
                addr: SocketAddr::Path(path),
            })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {
                addr: SocketAddr::Name(
                    allmystuff_protocol::control::default_pipe_name().to_string(),
                ),
            })
        }
    }

    /// A client for a daemon listening on an explicit socket path, for the
    /// one host that can't use the default: the mobile shell, whose sandbox
    /// forbids `$HOME`-root writes and whose container paths overrun the
    /// 104-byte `sun_path` limit — it parks the socket under the short
    /// `$TMPDIR` and hands the same path to the embedded daemon's config.
    #[cfg(unix)]
    pub fn with_path(path: std::path::PathBuf) -> Self {
        Self {
            addr: SocketAddr::Path(path),
        }
    }

    /// One-shot request → response. Opens a socket, writes one JSON line,
    /// reads one back, closes. No pooling (a local round trip is cheap and
    /// pooling muddies daemon-restart semantics).
    pub async fn request(&self, req: &Request) -> Result<Response> {
        self.request_with_timeout(req, Duration::from_secs(5)).await
    }

    /// [`Self::request`] with a caller-sized read deadline — for the ops
    /// whose reply legitimately takes longer than the 5 s default: a
    /// `NetworkConnectPeer { wait_ms, .. }` holding for ACTIVE, or a
    /// `ChannelSendReliable` holding for the peer's delivery ack. Size
    /// it past the op's own deadline (`wait_ms` / `ttl_ms`), never
    /// equal to it, so the daemon's honest timeout answer wins over the
    /// socket's.
    pub async fn request_with_timeout(
        &self,
        req: &Request,
        read_timeout: Duration,
    ) -> Result<Response> {
        let stream = self.connect().await?;
        let (reader, mut writer) = stream.split();
        let mut reader = BufReader::new(reader);

        let line = serde_json::to_string(req)? + "\n";
        writer
            .write_all(line.as_bytes())
            .await
            .context("write request")?;
        writer.flush().await.context("flush request")?;

        let mut buf = String::new();
        let n = tokio::time::timeout(read_timeout, reader.read_line(&mut buf))
            .await
            .context("daemon response timed out")??;
        if n == 0 {
            bail!("daemon closed the connection without a response");
        }
        serde_json::from_str(buf.trim()).with_context(|| format!("parse response: {buf}"))
    }

    /// Subscribe to the daemon's event stream. Forwards each line to `tx`
    /// as opaque JSON; returns after the initial ack.
    pub async fn subscribe_events(
        &self,
        tx: mpsc::Sender<serde_json::Value>,
    ) -> Result<allmystuff_protocol::ClientId> {
        let stream = self.connect().await?;
        let (reader, mut writer) = stream.split();
        let mut reader = BufReader::new(reader);

        let line = serde_json::to_string(&Request::EventsSubscribe)? + "\n";
        writer
            .write_all(line.as_bytes())
            .await
            .context("write subscribe")?;
        writer.flush().await.context("flush subscribe")?;

        let mut ack = String::new();
        let n = reader.read_line(&mut ack).await.context("read ack")?;
        if n == 0 {
            bail!("daemon closed the connection before the subscribe ack");
        }
        let parsed: Response =
            serde_json::from_str(ack.trim()).with_context(|| format!("parse ack: {ack}"))?;
        if !parsed.ok {
            return Err(anyhow!(
                "subscribe rejected: {}",
                parsed.error.unwrap_or_else(|| "(no error)".into())
            ));
        }
        // The ack carries this connection's client_id (as the daemon's
        // `c<n>` string); we pass it back on ChannelSubscribe so channel
        // frames route to this event socket.
        let client_id: allmystuff_protocol::ClientId = parsed
            .data
            .as_ref()
            .and_then(|d| d.get("client_id"))
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| anyhow!("subscribe ack missing client_id"))?;

        tokio::spawn(async move {
            // Keep the writer half alive for the lifetime of the read loop.
            let _writer_keepalive = writer;
            let mut buf = String::new();
            loop {
                buf.clear();
                match reader.read_line(&mut buf).await {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!("event stream read failed: {e}");
                        break;
                    }
                }
                let value: serde_json::Value = match serde_json::from_str(buf.trim()) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!("malformed event line: {e} — {buf}");
                        continue;
                    }
                };
                if tx.send(value).await.is_err() {
                    break;
                }
            }
        });

        Ok(client_id)
    }

    /// Open a dedicated binary **media-source** pipe for `client_id` (the id
    /// from [`subscribe_events`]). After the handshake the daemon pushes
    /// length-prefixed inbound media frames (`[u32 len][body]`) for everything
    /// that client subscribed to; this reads them and forwards each decoded
    /// [`InboundFrame`] to `tx`. Inbound H.264/Opus then carries no base64.
    /// The spawned reader ends when the daemon closes the pipe or `tx` is
    /// dropped; the caller can reconnect on the next session.
    ///
    /// [`subscribe_events`]: ControlClient::subscribe_events
    pub async fn subscribe_media_source(
        &self,
        client_id: allmystuff_protocol::ClientId,
        tx: mpsc::Sender<InboundFrame>,
    ) -> Result<()> {
        self.subscribe_media_source_inner(client_id, tx, |frame, _, _, _| frame)
            .await
    }

    /// Internal variant that keeps process-local timing correlation beside the
    /// decoded frame. Keeping this separate preserves the public
    /// `subscribe_media_source` API and does not add anything to the media or
    /// control wire formats.
    pub(crate) async fn subscribe_profiled_media_source(
        &self,
        client_id: allmystuff_protocol::ClientId,
        tx: mpsc::Sender<ProfiledInboundFrame>,
    ) -> Result<()> {
        self.subscribe_media_source_inner(
            client_id,
            tx,
            |frame, profile_id, profile_ts_us, enqueued_at| ProfiledInboundFrame {
                frame,
                profile_id,
                profile_ts_us,
                enqueued_at,
            },
        )
        .await
    }

    async fn subscribe_media_source_inner<T, F>(
        &self,
        client_id: allmystuff_protocol::ClientId,
        tx: mpsc::Sender<T>,
        wrap: F,
    ) -> Result<()>
    where
        T: Send + 'static,
        F: Fn(InboundFrame, u64, Option<u64>, Option<std::time::Instant>) -> T + Send + 'static,
    {
        let stream = self.connect().await?;
        let (reader, mut writer) = stream.split();
        let mut reader = BufReader::new(reader);

        let line = serde_json::to_string(&Request::MediaSourcePipe { client_id })? + "\n";
        writer
            .write_all(line.as_bytes())
            .await
            .context("write media-source handshake")?;
        writer
            .flush()
            .await
            .context("flush media-source handshake")?;

        let mut ack = String::new();
        let n = reader
            .read_line(&mut ack)
            .await
            .context("read media-source ack")?;
        if n == 0 {
            bail!("daemon closed the connection before the media-source ack");
        }
        let parsed: Response = serde_json::from_str(ack.trim())
            .with_context(|| format!("parse media-source ack: {ack}"))?;
        if !parsed.ok {
            return Err(anyhow!(
                "media-source rejected: {}",
                parsed.error.unwrap_or_else(|| "(no error)".into())
            ));
        }

        tokio::spawn(async move {
            // Hold the writer half open for the lifetime of the read loop
            // (dropping it would half-close the pipe).
            let _writer_keepalive = writer;
            let mut profile_aus = std::collections::HashMap::new();
            loop {
                // Includes the expected inter-frame wait plus both local-pipe
                // reads. It is intentionally named wait/read: this is not a
                // claim that the socket spent the whole span executing I/O.
                let pipe_started = crate::pipeline_profile::stamp();
                let mut len_buf = [0u8; 4];
                if reader.read_exact(&mut len_buf).await.is_err() {
                    break;
                }
                let len = u32::from_le_bytes(len_buf) as usize;
                if len > MAX_MEDIA_FRAME_BYTES {
                    tracing::warn!("media-source frame too large ({len} bytes) — closing pipe");
                    break;
                }
                let mut body = vec![0u8; len];
                if reader.read_exact(&mut body).await.is_err() {
                    break;
                }
                let parse_started = crate::pipeline_profile::stamp();
                let Some(frame) = decode_inbound_frame(&body) else {
                    tracing::warn!("malformed media-source frame ({len} bytes) — skipped");
                    continue;
                };
                // Snapshot both observed spans before recording either; a
                // five-second summary or full trace queue must not inflate the
                // parse measurement it is describing.
                let parsed_at = crate::pipeline_profile::stamp();
                let profile_video =
                    frame.kind == MEDIA_KIND_VIDEO && crate::pipeline_profile::enabled();
                let profile_id = if profile_video {
                    inbound_profile_id(&mut profile_aus, &frame)
                } else {
                    0
                };
                let profile_ts_us = profile_video
                    .then(|| u64::from(frame.rtp_timestamp).saturating_mul(1_000) / 90);
                if profile_video {
                    if let (Some(read_started), Some(parse_started)) = (pipe_started, parse_started)
                    {
                        crate::pipeline_profile::record_at(
                            &frame.from,
                            profile_id,
                            profile_ts_us,
                            crate::pipeline_profile::Stage::InboundPipeWaitRead,
                            parse_started.saturating_duration_since(read_started),
                            parse_started,
                        );
                    }
                    if let (Some(started), Some(ended)) = (parse_started, parsed_at) {
                        crate::pipeline_profile::record_at(
                            &frame.from,
                            profile_id,
                            profile_ts_us,
                            crate::pipeline_profile::Stage::InboundParseBusy,
                            ended.saturating_duration_since(started),
                            ended,
                        );
                    }
                }
                let admission_started =
                    profile_video.then(crate::pipeline_profile::stamp).flatten();
                let permit = match tx.reserve().await {
                    Ok(permit) => permit,
                    Err(_) => break,
                };
                if profile_video {
                    crate::pipeline_profile::record_since(
                        &frame.from,
                        profile_id,
                        profile_ts_us,
                        crate::pipeline_profile::Stage::InboundDispatchBackpressure,
                        admission_started,
                    );
                }
                let enqueued_at = profile_video.then(crate::pipeline_profile::stamp).flatten();
                permit.send(wrap(frame, profile_id, profile_ts_us, enqueued_at));
            }
        });

        Ok(())
    }

    async fn connect(&self) -> Result<LocalSocketStream> {
        let name = match &self.addr {
            #[cfg(unix)]
            SocketAddr::Path(p) => p
                .as_path()
                .to_fs_name::<GenericFilePath>()
                .context("socket path → fs_name")?,
            #[cfg(not(unix))]
            SocketAddr::Name(n) => n
                .as_str()
                .to_ns_name::<GenericNamespaced>()
                .context("socket name → ns_name")?,
        };
        LocalSocketStream::connect(name)
            .await
            .context("connect daemon socket — is `myownmesh serve` running?")
    }
}

/// A persistent connection dedicated to the media plane's sends.
///
/// [`ControlClient::request`]'s connect-send-await-close shape is right
/// for one-shot commands and wrong for a 24 fps frame stream: every ≤40 KiB
/// video chunk paid a socket connect plus a full round trip, *serially* —
/// several RTTs of dead air inside each frame. The daemon serves a
/// connection's request lines in order (`handle_client` loops), so this
/// pipe writes them back-to-back and drains the responses on a background
/// reader, which logs daemon refusals (rate-limited) instead of stalling
/// the send path to hear them.
///
/// Backpressure survives: when the daemon stops keeping up, the socket
/// buffer fills, `send` awaits, the bounded video queue behind it fills,
/// and the capture side drops frames — freshness over latency, unchanged.
/// Any write failure drops the connection; the next send reconnects.
pub struct MediaPipe {
    client: Arc<ControlClient>,
    writer: tokio::sync::Mutex<Option<interprocess::local_socket::tokio::SendHalf>>,
}

impl MediaPipe {
    pub fn new(client: Arc<ControlClient>) -> Self {
        MediaPipe {
            client,
            writer: tokio::sync::Mutex::new(None),
        }
    }

    /// Queue one request down the pipe, (re)connecting first if needed.
    /// `Ok` means the bytes reached the socket; the daemon's verdict
    /// arrives later via the reader task's (rate-limited) log line.
    pub async fn send(&self, req: &Request) -> Result<()> {
        self.send_inner(req, None).await
    }

    /// The same local JSON pipe with process-local profiler correlation.
    /// Neither the id nor any timing is serialized into the request.
    pub async fn send_profiled(&self, req: &Request, route: &str, frame_id: u64) -> Result<()> {
        self.send_inner(req, Some((route, frame_id))).await
    }

    async fn send_inner(&self, req: &Request, profile: Option<(&str, u64)>) -> Result<()> {
        let profile = profile.filter(|_| crate::pipeline_profile::enabled());
        let frame_id = profile
            .map(|(_, id)| {
                if id == 0 {
                    crate::pipeline_profile::next_frame_id()
                } else {
                    id
                }
            })
            .unwrap_or(0);
        let serialize_started = profile.and_then(|_| crate::pipeline_profile::stamp());
        let line = serde_json::to_string(req)? + "\n";
        if let Some((route, _)) = profile {
            crate::pipeline_profile::record_since(
                route,
                frame_id,
                None,
                crate::pipeline_profile::Stage::OutboundSerializeBusy,
                serialize_started,
            );
        }
        let pipe_wait_started = profile.and_then(|_| crate::pipeline_profile::stamp());
        let mut writer = self.writer.lock().await;
        if let Some((route, _)) = profile {
            crate::pipeline_profile::record_since(
                route,
                frame_id,
                None,
                crate::pipeline_profile::Stage::OutboundPipeWait,
                pipe_wait_started,
            );
        }
        if writer.is_none() {
            let connect_started = profile.and_then(|_| crate::pipeline_profile::stamp());
            let stream = self.client.connect().await?;
            let (reader, send_half) = stream.split();
            spawn_response_drain(reader);
            *writer = Some(send_half);
            if let Some((route, _)) = profile {
                crate::pipeline_profile::record_since(
                    route,
                    frame_id,
                    None,
                    crate::pipeline_profile::Stage::OutboundPipeConnectWait,
                    connect_started,
                );
            }
        }
        let w = writer.as_mut().expect("connected above");
        // Bounded: a daemon that stops *reading* (wedged, not dead) never
        // errors the write — it just never completes, silently stalling
        // every media send behind this mutex forever. The timeout converts
        // that into the same drop-and-reconnect a write error gets. A
        // healthy local-socket write completes in microseconds; seconds of
        // blockage is a wedged peer, not backpressure.
        let pipe_write_started = profile.and_then(|_| crate::pipeline_profile::stamp());
        let outcome = tokio::time::timeout(PIPE_WRITE_TIMEOUT, async {
            w.write_all(line.as_bytes()).await?;
            w.flush().await
        })
        .await;
        if let Some((route, _)) = profile {
            crate::pipeline_profile::record_since(
                route,
                frame_id,
                None,
                crate::pipeline_profile::Stage::OutboundPipeWrite,
                pipe_write_started,
            );
        }
        match outcome {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => {
                *writer = None;
                Err(anyhow!("media pipe write: {e}"))
            }
            Err(_) => {
                *writer = None;
                Err(anyhow!(
                    "media pipe write timed out after {}s — dropping the connection",
                    PIPE_WRITE_TIMEOUT.as_secs()
                ))
            }
        }
    }
}

/// How long one pipe write may block before the connection is declared
/// wedged and dropped for reconnect. Healthy local IPC flushes in
/// microseconds; genuine backpressure shows as *slow* progress, not a
/// multi-second single write.
const PIPE_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// A persistent connection dedicated to **binary** media-track sends — the
/// H.264 and Opus lanes. The first send converts the connection with a single
/// [`Request::MediaTrackPipe`] line; everything after is length-prefixed binary
/// frames (`[u32 len][body]`, see `allmystuff_protocol::encode_media_frame`) —
/// no base64 (+33% and a CPU pass) and no per-frame JSON of a multi-KB string.
/// MJPEG, PCM and route signalling stay on the JSON [`MediaPipe`], untouched.
/// Backpressure and reconnect match [`MediaPipe`]: a full socket awaits, a
/// failed write drops the connection and the next send reconnects.
pub struct MediaTrackPipe {
    client: Arc<ControlClient>,
    writer: tokio::sync::Mutex<Option<interprocess::local_socket::tokio::SendHalf>>,
}

impl MediaTrackPipe {
    pub fn new(client: Arc<ControlClient>) -> Self {
        MediaTrackPipe {
            client,
            writer: tokio::sync::Mutex::new(None),
        }
    }

    /// Stream one H.264 access unit to `peer`'s video lane `stream`.
    pub async fn send_video(
        &self,
        network: &str,
        peer: &str,
        stream: u8,
        duration_us: u64,
        data: &[u8],
    ) -> Result<()> {
        self.send_frame(
            MEDIA_KIND_VIDEO,
            network,
            peer,
            stream,
            duration_us,
            data,
            None,
        )
        .await
    }

    /// Internal profiler-correlated form of [`Self::send_video`]. Correlation
    /// remains process-local and does not alter the daemon or peer wire bytes.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn send_profiled_video(
        &self,
        network: &str,
        peer: &str,
        stream: u8,
        duration_us: u64,
        data: &[u8],
        profile_route: &str,
        profile_id: u64,
    ) -> Result<()> {
        self.send_frame(
            MEDIA_KIND_VIDEO,
            network,
            peer,
            stream,
            duration_us,
            data,
            Some((profile_route, profile_id)),
        )
        .await
    }

    /// Stream one Opus frame to `peer`'s audio lane `stream`.
    pub async fn send_audio(
        &self,
        network: &str,
        peer: &str,
        stream: u8,
        duration_us: u64,
        data: &[u8],
    ) -> Result<()> {
        self.send_frame(
            MEDIA_KIND_AUDIO,
            network,
            peer,
            stream,
            duration_us,
            data,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn send_frame(
        &self,
        kind: u8,
        network: &str,
        peer: &str,
        stream: u8,
        duration_us: u64,
        data: &[u8],
        profile: Option<(&str, u64)>,
    ) -> Result<()> {
        let profile = profile.filter(|_| crate::pipeline_profile::enabled());
        let profile_id = profile
            .map(|(_, id)| {
                if id == 0 {
                    crate::pipeline_profile::next_frame_id()
                } else {
                    id
                }
            })
            .unwrap_or(0);
        let serialize_started = profile.and_then(|_| crate::pipeline_profile::stamp());
        let body = encode_media_frame(kind, stream, duration_us, network, peer, data);
        if let Some((route, _)) = profile {
            crate::pipeline_profile::record_since(
                route,
                profile_id,
                None,
                crate::pipeline_profile::Stage::OutboundSerializeBusy,
                serialize_started,
            );
        }
        let pipe_wait_started = profile.and_then(|_| crate::pipeline_profile::stamp());
        let mut writer = self.writer.lock().await;
        if let Some((route, _)) = profile {
            crate::pipeline_profile::record_since(
                route,
                profile_id,
                None,
                crate::pipeline_profile::Stage::OutboundPipeWait,
                pipe_wait_started,
            );
        }
        if writer.is_none() {
            let connect_started = profile.and_then(|_| crate::pipeline_profile::stamp());
            let conn = self.client.connect().await?;
            let (reader, mut send_half) = conn.split();
            spawn_response_drain(reader);
            // Convert the fresh connection to the binary media-track protocol.
            let line = serde_json::to_string(&Request::MediaTrackPipe)? + "\n";
            let hs = tokio::time::timeout(PIPE_WRITE_TIMEOUT, async {
                send_half.write_all(line.as_bytes()).await?;
                send_half.flush().await
            })
            .await;
            match hs {
                Ok(r) => r.context("media-track handshake")?,
                Err(_) => {
                    return Err(anyhow!(
                        "media-track handshake timed out after {}s",
                        PIPE_WRITE_TIMEOUT.as_secs()
                    ))
                }
            }
            *writer = Some(send_half);
            if let Some((route, _)) = profile {
                crate::pipeline_profile::record_since(
                    route,
                    profile_id,
                    None,
                    crate::pipeline_profile::Stage::OutboundPipeConnectWait,
                    connect_started,
                );
            }
        }
        // Header and body go out under one lock so frames never interleave.
        // Bounded like the JSON pipe: a hung-but-open daemon socket must
        // cost a reconnect, not a silent forever-stall of every audio and
        // video send behind this mutex (the one silent-freeze vector the
        // encoder pass left open).
        let w = writer.as_mut().expect("connected above");
        let len = (body.len() as u32).to_le_bytes();
        let pipe_write_started = profile.and_then(|_| crate::pipeline_profile::stamp());
        let outcome = tokio::time::timeout(PIPE_WRITE_TIMEOUT, async {
            w.write_all(&len).await?;
            w.write_all(&body).await?;
            w.flush().await
        })
        .await;
        if let Some((route, _)) = profile {
            crate::pipeline_profile::record_since(
                route,
                profile_id,
                None,
                crate::pipeline_profile::Stage::OutboundPipeWrite,
                pipe_write_started,
            );
        }
        match outcome {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => {
                *writer = None;
                Err(anyhow!("media-track write: {e}"))
            }
            Err(_) => {
                *writer = None;
                Err(anyhow!(
                    "media-track write timed out after {}s — dropping the connection",
                    PIPE_WRITE_TIMEOUT.as_secs()
                ))
            }
        }
    }
}

/// Drain one pipe connection's response lines, surfacing refusals. Media
/// send failures repeat at frame rate when a peer drops mid-stream, so
/// warnings are rate-limited; the task ends with its socket.
fn spawn_response_drain(reader: interprocess::local_socket::tokio::RecvHalf) {
    tokio::spawn(async move {
        const WARN_EVERY: Duration = Duration::from_secs(5);
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        let mut last_warn: Option<std::time::Instant> = None;
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let Ok(resp) = serde_json::from_str::<Response>(line.trim()) else {
                continue;
            };
            if !resp.ok && last_warn.is_none_or(|t| t.elapsed() >= WARN_EVERY) {
                last_warn = Some(std::time::Instant::now());
                tracing::warn!(
                    "media send refused by daemon: {}",
                    resp.error.unwrap_or_else(|| "(no error)".into())
                );
            }
        }
    });
}
