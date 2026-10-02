//! Socket and line primitives for host adapters that supply their own policy.
//!
//! These primitives do not manufacture owned event registrations. A host must
//! check its policy on the same connection before sending protected operations.

use std::time::Duration;

use allmystuff_protocol::Response;
use anyhow::{anyhow, bail, Context, Result};
use interprocess::local_socket::tokio::prelude::*;
#[cfg(unix)]
use interprocess::local_socket::GenericFilePath;
#[cfg(not(unix))]
use interprocess::local_socket::GenericNamespaced;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

/// An explicit native endpoint; this crate never discovers a default address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Endpoint {
    #[cfg(unix)]
    Path(std::path::PathBuf),
    #[cfg(not(unix))]
    Name(String),
}

pub const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);
pub const STATUS_ACK_LIMIT: usize = 16 * 1024;
/// Local bound for the characterized candidate event adapter, not a daemon
/// resource grant or an application's future media policy.
pub const CANDIDATE_EVENT_LINE_LIMIT: usize = 1024 * 1024;

pub async fn connect_endpoint(endpoint: &Endpoint) -> Result<LocalSocketStream> {
    let name = match endpoint {
        #[cfg(unix)]
        Endpoint::Path(p) => p
            .as_path()
            .to_fs_name::<GenericFilePath>()
            .context("socket path → fs_name")?,
        #[cfg(not(unix))]
        Endpoint::Name(n) => n
            .as_str()
            .to_ns_name::<GenericNamespaced>()
            .context("socket name → ns_name")?,
    };
    tokio::time::timeout(CONTROL_TIMEOUT, LocalSocketStream::connect(name))
        .await
        .context("daemon socket connect timed out")?
        .context("connect daemon socket — is `myownmesh serve` running?")
}

/// Write and read one JSON exchange with a deadline. Errors omit remote JSON.
pub async fn round_trip<R, W>(
    reader: &mut R,
    writer: &mut W,
    line: &str,
    read_timeout: Duration,
    limit: Option<usize>,
) -> Result<Response>
where
    R: tokio::io::AsyncBufRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    tokio::time::timeout(read_timeout, async {
        writer
            .write_all(line.as_bytes())
            .await
            .context("write daemon request")?;
        writer.flush().await.context("flush daemon request")?;
        let bytes = read_json_line(reader, limit).await?;
        // Never put remote JSON or deserializer values in diagnostics: ACKs
        // and even malformed response envelopes can contain capability C.
        serde_json::from_slice(&bytes).map_err(|_| anyhow!("invalid daemon response JSON"))
    })
    .await
    .context("daemon request timed out")?
}

/// Read one JSON line, optionally bounding bytes before allocation can grow.
pub async fn read_json_line<R>(reader: &mut R, limit: Option<usize>) -> Result<Vec<u8>>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    let mut bytes = Vec::new();
    let count = match limit {
        Some(limit) => {
            let read_limit = (limit as u64)
                .checked_add(1)
                .ok_or_else(|| anyhow!("daemon JSON line limit is too large"))?;
            (&mut *reader)
                .take(read_limit)
                .read_until(b'\n', &mut bytes)
                .await?
        }
        None => reader.read_until(b'\n', &mut bytes).await?,
    };
    if count == 0 {
        bail!("daemon closed the connection without a response");
    }
    if limit.is_some_and(|limit| count > limit) {
        bail!("daemon JSON line exceeds the adapter limit");
    }
    Ok(bytes)
}
