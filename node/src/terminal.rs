//! Compatibility paths for terminal hosting on the node's registered runtime.

pub use allmystuff_terminal::host::{OutMsg, SessionInfo, TermAttach};

/// Terminal tasks use the existing node runtime at their original spawn sites.
#[doc(hidden)]
pub struct NodeSpawner;

impl allmystuff_terminal::TaskSpawner for NodeSpawner {
    fn spawn<F>(future: F) -> tokio::task::JoinHandle<()>
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        crate::spawn(future)
    }
}

pub type TerminalHost = allmystuff_terminal::host::TerminalHost<NodeSpawner>;
