//! The AllMyStuff node's binary local IPC client, without the node engine.
//!
//! [`NodeClient`] retains the node/desktop client's `anyhow` errors and event
//! diagnostics. [`terminal::NodeClient`] retains the terminal client's string
//! errors and silent event handling. Both use the same wire and connection
//! implementation. Each request opens one connection; event subscriptions
//! retain their writer half until the reader exits.
//!
//! Socket binding, access control, command dispatch and process supervision
//! belong to the host. This is separate from MyOwnMesh's daemon transport.

pub mod address;
mod client;
mod error;
pub mod terminal;
mod wire;

use anyhow::Result;
use interprocess::local_socket::tokio::prelude::LocalSocketStream;
use serde_json::Value;
use tokio::sync::mpsc;

pub use wire::{
    read_frame, write_frame, NodeEvent, NodeRequest, WireResponse, MAX_FRAME_LEN, SUBSCRIBE_EVENTS,
    TAG_BYTES, TAG_EVENT, TAG_JSON, TAG_RESTART,
};

/// Client with the node/desktop's original error chains and event diagnostics.
pub struct NodeClient {
    inner: client::Client,
}

impl NodeClient {
    /// Resolve the node socket address without connecting.
    pub fn new() -> Result<Self> {
        Ok(Self {
            inner: client::Client::new().map_err(error::Error::into_node)?,
        })
    }

    /// Open the resolved control socket for host-side owner inspection.
    ///
    /// This preserves the node host's existing connection and error context;
    /// ordinary commands can use [`Self::request`] or [`Self::request_bytes`].
    pub async fn connect(&self) -> Result<LocalSocketStream> {
        self.inner.connect().await.map_err(error::Error::into_node)
    }

    /// One connection and one request, expecting a JSON response.
    pub async fn request(&self, cmd: &str, args: Value) -> Result<Value> {
        self.inner
            .request(cmd, args)
            .await
            .map_err(error::Error::into_node)
    }

    /// One connection and one request, expecting a raw byte response.
    pub async fn request_bytes(&self, cmd: &str, args: Value) -> Result<Vec<u8>> {
        self.inner
            .request_bytes(cmd, args)
            .await
            .map_err(error::Error::into_node)
    }

    /// Await subscription acknowledgement, then forward events until disconnect.
    pub async fn subscribe_events(&self, tx: mpsc::Sender<NodeEvent>) -> Result<()> {
        self.inner
            .subscribe_events(tx, client::EventPolicy::Node)
            .await
            .map_err(error::Error::into_node)
    }

    /// True when a node is listening on the control socket.
    pub async fn probe() -> bool {
        client::Client::probe().await
    }

    #[cfg(test)]
    pub(crate) fn for_test_address(addr: address::SocketAddr) -> Self {
        Self {
            inner: client::Client::at_address(addr),
        }
    }
}

#[cfg(test)]
#[path = "../tests/support/compatibility.rs"]
mod compatibility;
