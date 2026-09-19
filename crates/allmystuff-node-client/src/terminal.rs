//! The terminal client's original string errors and silent event handling.

use serde_json::Value;
use tokio::sync::mpsc;

use crate::{client, error, NodeEvent};

/// Compatibility client for `amst`; every request opens its own connection.
pub struct NodeClient {
    inner: client::Client,
}

impl NodeClient {
    /// Resolve the node socket address without connecting.
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            inner: client::Client::new().map_err(error::Error::into_terminal)?,
        })
    }

    /// True when a node is listening on the control socket.
    pub async fn probe() -> bool {
        client::Client::probe().await
    }

    /// One connection and one request, expecting a JSON response.
    pub async fn request(&self, cmd: &str, args: Value) -> Result<Value, String> {
        self.inner
            .request(cmd, args)
            .await
            .map_err(error::Error::into_terminal)
    }

    /// One connection and one request, expecting a raw byte response.
    pub async fn request_bytes(&self, cmd: &str, args: Value) -> Result<Vec<u8>, String> {
        self.inner
            .request_bytes(cmd, args)
            .await
            .map_err(error::Error::into_terminal)
    }

    /// Await subscription acknowledgement, then forward events until disconnect.
    pub async fn subscribe_events(&self, tx: mpsc::Sender<NodeEvent>) -> Result<(), String> {
        self.inner
            .subscribe_events(tx, client::EventPolicy::Terminal)
            .await
            .map_err(error::Error::into_terminal)
    }

    #[cfg(test)]
    pub(crate) fn for_test_address(addr: crate::address::SocketAddr) -> Self {
        Self {
            inner: client::Client::at_address(addr),
        }
    }
}
