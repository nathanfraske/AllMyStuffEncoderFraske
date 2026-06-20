//! Machine-wide single-instance guard for the AllMyStuff node.
//!
//! Exactly one node may run per machine: the desktop app's in-process node
//! **or** the headless `allmystuff-serve` ("Always On" service) — never both.
//! Two nodes advertise the same device identity into the same mesh and fight
//! over presence and routes, and the upshot is that *no* system can connect.
//! That's the whole point of the daemon/node split: one node per machine.
//!
//! The guard is a bound **loopback** listener rather than a lock file. It's
//! dependency-free, works identically on Linux/macOS/Windows, and — crucially
//! — the OS frees it the instant the holder exits, **even on a crash**, so a
//! stale guard can never wedge a machine into "nothing can start." It only
//! ever binds `127.0.0.1`, so it never touches the network or trips a firewall
//! prompt.

use std::net::TcpListener;

/// The loopback port that stands in for the lock — high and app-specific to
/// dodge collisions, `127.0.0.1` only. (Not a service: nothing connects to it;
/// the bind itself is the mutex.)
const LOCK_PORT: u16 = 47653;

/// Held for as long as this process should be *the* node on this machine.
/// Dropping it — including by the process exiting — frees the machine for the
/// next node to take over (e.g. the Always-On service resuming when the
/// desktop app closes).
pub struct NodeInstanceLock(#[allow(dead_code)] TcpListener);

/// Try to become this machine's single node. `Some` = we own it and should
/// bring the mesh up; `None` = another node (the desktop app, or the service)
/// already holds it, so the caller must yield instead of starting a second
/// mesh.
pub fn acquire() -> Option<NodeInstanceLock> {
    // Loopback only — never `0.0.0.0` — so this is a pure local mutex. A second
    // live bind to the same port fails on every platform (std doesn't set
    // SO_REUSEADDR on Windows, and on unix it doesn't permit two live
    // listeners), which is exactly the exclusion we want.
    TcpListener::bind(("127.0.0.1", LOCK_PORT))
        .ok()
        .map(NodeInstanceLock)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_is_refused_while_the_first_is_held() {
        let Some(first) = acquire() else {
            // Something else on the test host already holds the port — skip
            // rather than fail spuriously in a busy CI sandbox.
            return;
        };
        assert!(
            acquire().is_none(),
            "a second node must not be able to take the machine while the first holds it"
        );
        drop(first);
        // …and once the holder is gone, the next node can take over.
        assert!(
            acquire().is_some(),
            "the machine must free up for the next node once the holder drops"
        );
    }
}
