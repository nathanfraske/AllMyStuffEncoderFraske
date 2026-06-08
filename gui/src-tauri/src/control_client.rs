//! Client half of the `myownmesh` daemon's control protocol. The wire
//! types live in `allmystuff-protocol` (a mirror of the daemon's
//! `control.rs`); this module is just the transport — connect, write one
//! line, read one line — over the local socket (`interprocess`).
//!
//! Two shapes, exactly like the MyOwnMesh GUI's client:
//!
//!  * [`ControlClient::request`] — short-lived round trip for every
//!    one-shot command.
//!  * [`ControlClient::subscribe_events`] — a long-lived stream that
//!    forwards each `ServerOut` line to a channel until the daemon
//!    disconnects.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use interprocess::local_socket::tokio::prelude::*;
#[cfg(unix)]
use interprocess::local_socket::GenericFilePath;
#[cfg(not(unix))]
use interprocess::local_socket::GenericNamespaced;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

pub use allmystuff_protocol::{Request, Response};

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
                addr: SocketAddr::Name(allmystuff_protocol::control::default_pipe_name().to_string()),
            })
        }
    }

    /// One-shot request → response. Opens a socket, writes one JSON line,
    /// reads one back, closes. No pooling (a local round trip is cheap and
    /// pooling muddies daemon-restart semantics).
    pub async fn request(&self, req: &Request) -> Result<Response> {
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
        let n = tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut buf))
            .await
            .context("daemon response timed out")??;
        if n == 0 {
            bail!("daemon closed the connection without a response");
        }
        serde_json::from_str(buf.trim()).with_context(|| format!("parse response: {buf}"))
    }

    /// Subscribe to the daemon's event stream. Forwards each line to `tx`
    /// as opaque JSON; returns after the initial ack.
    pub async fn subscribe_events(&self, tx: mpsc::Sender<serde_json::Value>) -> Result<()> {
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
