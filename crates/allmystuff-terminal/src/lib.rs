//! Shared terminal sessions and viewer queues, with caller-owned task spawning.
//!
//! The default build provides the functional [`viewer`] queues and the existing
//! hosting refusal. The `host` feature adds PTY sessions through `host`.
//! Authentication, route transport and application runtime registration remain
//! responsibilities of the caller.

pub mod viewer;

#[cfg(feature = "host")]
pub mod host;

#[cfg(feature = "host")]
pub use host::{OutMsg, SessionInfo, TermAttach, TerminalHost};
#[cfg(not(feature = "host"))]
pub use viewer::{OutMsg, SessionInfo, TermAttach, TerminalHost};

/// Spawn the terminal's two asynchronous tasks on a caller-owned runtime.
///
/// Called only where the idle reaper and broadcast bridge originally spawned.
/// Constructing a [`host::TerminalHost`] does not consult this policy. The
/// returned handle is dropped immediately, retaining Tokio's detached-task
/// behavior; implementations must return a handle for the supplied future.
#[cfg(feature = "host")]
pub trait TaskSpawner {
    fn spawn<F>(future: F) -> tokio::task::JoinHandle<()>
    where
        F: std::future::Future<Output = ()> + Send + 'static;
}

// The retained host tests originally registered the node's process-wide
// runtime. This test-only copy preserves that fixture behavior without adding
// a runtime registry to the reusable production library.
#[cfg(all(test, feature = "host"))]
mod test_runtime {
    static RUNTIME: std::sync::OnceLock<tokio::runtime::Handle> = std::sync::OnceLock::new();

    pub(crate) struct Spawner;

    impl super::TaskSpawner for Spawner {
        fn spawn<F>(future: F) -> tokio::task::JoinHandle<()>
        where
            F: std::future::Future<Output = ()> + Send + 'static,
        {
            RUNTIME
                .get()
                .expect("allmystuff-node runtime not registered — Mesh::start calls set_runtime()")
                .spawn(future)
        }
    }

    pub(crate) fn set_runtime(handle: tokio::runtime::Handle) {
        let _ = RUNTIME.set(handle);
    }
}

#[cfg(all(test, feature = "host"))]
use test_runtime::set_runtime;
