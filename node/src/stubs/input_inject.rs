//! No-op twin of [`crate::input_inject`] for capture-less builds
//! (`--no-default-features`, currently iOS).
//!
//! There is no OS injector in this build. The stub still mirrors the host
//! implementation's route-generation API so shared mesh code fails closed
//! when a route ends or a same-id replacement takes over.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use allmystuff_session::InputAction;
use parking_lot::Mutex;

#[derive(Default)]
pub struct Injector {
    session_epoch: AtomicU64,
    next_route_epoch: AtomicU64,
    route_epochs: Mutex<HashMap<String, u64>>,
}

/// A process-local snapshot of one input route lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputLease {
    session_epoch: u64,
    route_epoch: u64,
}

impl Injector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one active route lifetime without opening an OS injector.
    pub fn activate_route(&self, route: &str) {
        let route_epoch = self
            .next_route_epoch
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1)
            .max(1);
        self.route_epochs
            .lock()
            .insert(route.to_string(), route_epoch);
    }

    /// Capture the current route lifetime. Unknown and ended routes remain
    /// unregistered, so the shared mesh gate drops their events.
    pub fn lease(&self, route: &str) -> Option<InputLease> {
        let route_epoch = self.route_epochs.lock().get(route).copied()?;
        Some(InputLease {
            session_epoch: self.session_epoch.load(Ordering::SeqCst),
            route_epoch,
        })
    }

    /// Drop every event. Checking the lease preserves the host
    /// implementation's stale-lifetime diagnostic without touching OS input.
    pub fn apply(&self, route: &str, _action: InputAction, lease: InputLease) {
        if self.lease(route) == Some(lease) {
            tracing::debug!("input event for {route} dropped: capture-less build");
        } else {
            tracing::debug!("stale input event for {route} dropped: capture-less build");
        }
    }

    /// Nothing is ever pressed, but ending a route still invalidates its lease.
    pub fn release_route(&self, route: &str) {
        self.route_epochs.lock().remove(route);
    }

    /// Invalidate every lease from the previous daemon session.
    pub fn release_all(&self) {
        self.session_epoch.fetch_add(1, Ordering::SeqCst);
        self.route_epochs.lock().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_lifetimes_are_fenced_even_without_an_os_injector() {
        let injector = Injector::new();
        assert_eq!(injector.lease("route"), None);

        injector.activate_route("route");
        let first = injector.lease("route").expect("first route lifetime");

        injector.activate_route("route");
        let replacement = injector.lease("route").expect("replacement route lifetime");
        assert_ne!(first, replacement);

        injector.release_route("route");
        assert_eq!(injector.lease("route"), None);
    }

    #[test]
    fn daemon_reset_invalidates_every_route_lifetime() {
        let injector = Injector::new();
        injector.activate_route("a");
        injector.activate_route("b");
        assert!(injector.lease("a").is_some());
        assert!(injector.lease("b").is_some());

        injector.release_all();
        assert_eq!(injector.lease("a"), None);
        assert_eq!(injector.lease("b"), None);
    }
}
