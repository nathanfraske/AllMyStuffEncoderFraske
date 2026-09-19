//! Compatibility paths for the shared node IPC client.
//!
//! The terminal facade retains string errors and silent event handling.

use std::time::Duration;

pub use allmystuff_node_client::{terminal::NodeClient, NodeEvent};

/// Wait until the node socket answers, or `timeout` elapses. Returns whether a
/// node is up.
pub async fn wait_for_socket(timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if NodeClient::probe().await {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
