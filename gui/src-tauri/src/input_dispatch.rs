use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Weak,
    },
};

use allmystuff_node::node_control::NodeClient;
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, watch};

/// The local GUI-to-node input handoff.
///
/// Key and button transitions use one FIFO per route. Pointer motion uses a
/// separate latest-only lane so a backed-up cursor cannot delay a keyup or
/// mouse-button release. This is local Tauri IPC scheduling only; the action
/// and route sent to the node are unchanged.
pub(crate) struct InputDispatcher {
    inner: Arc<DispatcherInner>,
}

struct DispatcherInner {
    node: Arc<NodeClient>,
    routes: Mutex<HashMap<String, RouteEntry>>,
    next_generation: AtomicU64,
}

struct RouteEntry {
    generation: u64,
    senders: RouteSenders,
    discrete_pending: usize,
    motion_pending: bool,
}

#[derive(Clone)]
struct RouteSenders {
    discrete: mpsc::UnboundedSender<DiscreteInput>,
    motion: watch::Sender<Option<Value>>,
}

struct RouteReceivers {
    discrete: mpsc::UnboundedReceiver<DiscreteInput>,
    motion: watch::Receiver<Option<Value>>,
}

struct DiscreteInput {
    action: Value,
    acknowledged: oneshot::Sender<Result<(), String>>,
}

enum EnqueueReceipt {
    Discrete(oneshot::Receiver<Result<(), String>>),
    Motion,
}

impl RouteSenders {
    fn new() -> (Self, RouteReceivers) {
        let (discrete_tx, discrete_rx) = mpsc::unbounded_channel();
        let (motion_tx, motion_rx) = watch::channel(None);
        (
            Self {
                discrete: discrete_tx,
                motion: motion_tx,
            },
            RouteReceivers {
                discrete: discrete_rx,
                motion: motion_rx,
            },
        )
    }

    fn enqueue(&self, action: Value, ordered: bool) -> Result<EnqueueReceipt, String> {
        if ordered || !is_pointer_motion(&action) {
            let (acknowledged, receipt) = oneshot::channel();
            self.discrete
                .send(DiscreteInput {
                    action,
                    acknowledged,
                })
                .map_err(|_| "input route FIFO is closed".to_owned())?;
            Ok(EnqueueReceipt::Discrete(receipt))
        } else {
            self.motion.send_replace(Some(action));
            Ok(EnqueueReceipt::Motion)
        }
    }
}

impl InputDispatcher {
    pub(crate) fn new(node: Arc<NodeClient>) -> Self {
        Self {
            inner: Arc::new(DispatcherInner {
                node,
                routes: Mutex::new(HashMap::new()),
                next_generation: AtomicU64::new(1),
            }),
        }
    }

    /// Accept an event into the local route lane. Success is the enqueue
    /// acknowledgement used by the webview's held-input trackers.
    pub(crate) async fn enqueue(
        &self,
        route_id: String,
        action: Value,
        ordered: bool,
    ) -> Result<(), String> {
        let receipt = {
            let discrete = ordered || !is_pointer_motion(&action);
            let mut routes = self.inner.routes.lock();
            let entry = routes.entry(route_id.clone()).or_insert_with(|| {
                let generation = self.inner.next_generation.fetch_add(1, Ordering::Relaxed);
                let (senders, receivers) = RouteSenders::new();
                spawn_route_workers(
                    Arc::downgrade(&self.inner),
                    route_id.clone(),
                    generation,
                    receivers,
                );
                RouteEntry {
                    generation,
                    senders,
                    discrete_pending: 0,
                    motion_pending: false,
                }
            });

            if discrete {
                entry.discrete_pending += 1;
            } else {
                entry.motion_pending = true;
            }
            match entry.senders.enqueue(action, ordered) {
                Ok(receipt) => receipt,
                Err(error) => {
                    let generation = entry.generation;
                    if discrete {
                        entry.discrete_pending = entry.discrete_pending.saturating_sub(1);
                    } else {
                        entry.motion_pending = false;
                    }
                    retire_if_idle(&mut routes, &route_id, generation);
                    return Err(error);
                }
            }
        };

        match receipt {
            EnqueueReceipt::Discrete(acknowledged) => acknowledged
                .await
                .map_err(|_| "input route FIFO closed before acknowledgement".to_owned())?,
            EnqueueReceipt::Motion => Ok(()),
        }
    }
}

fn is_pointer_motion(action: &Value) -> bool {
    matches!(
        action.get("kind").and_then(Value::as_str),
        Some("mouse_move" | "mouse_move_rel")
    )
}

fn spawn_route_workers(
    inner: Weak<DispatcherInner>,
    route_id: String,
    generation: u64,
    receivers: RouteReceivers,
) {
    let RouteReceivers {
        mut discrete,
        mut motion,
    } = receivers;

    let discrete_inner = inner.clone();
    let discrete_route = route_id.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(input) = discrete.recv().await {
            let Some(inner) = discrete_inner.upgrade() else {
                return;
            };
            let result = forward(&inner.node, &discrete_route, input.action).await;
            if let Err(error) = &result {
                tracing::warn!(
                    route = %discrete_route,
                    "queued discrete input delivery failed: {error}"
                );
            }
            let _ = input.acknowledged.send(result);
            finish_discrete(&inner, &discrete_route, generation);
        }
    });

    tauri::async_runtime::spawn(async move {
        while motion.changed().await.is_ok() {
            let action = motion.borrow_and_update().clone();
            let Some(action) = action else {
                continue;
            };
            let Some(inner) = inner.upgrade() else {
                return;
            };
            if let Err(error) = forward(&inner.node, &route_id, action).await {
                tracing::debug!(
                    route = %route_id,
                    "coalesced pointer motion delivery failed: {error}"
                );
            }
            finish_motion(&inner, &route_id, generation, &motion);
        }
    });
}

fn finish_discrete(inner: &DispatcherInner, route_id: &str, generation: u64) {
    let mut routes = inner.routes.lock();
    let Some(entry) = routes.get_mut(route_id) else {
        return;
    };
    if entry.generation != generation {
        return;
    }
    entry.discrete_pending = entry.discrete_pending.saturating_sub(1);
    retire_if_idle(&mut routes, route_id, generation);
}

fn finish_motion(
    inner: &DispatcherInner,
    route_id: &str,
    generation: u64,
    receiver: &watch::Receiver<Option<Value>>,
) {
    // Enqueue mutates the watch value while holding the same map lock. Testing
    // for a newer value under that lock closes the send/retire race.
    let mut routes = inner.routes.lock();
    let Some(entry) = routes.get_mut(route_id) else {
        return;
    };
    if entry.generation != generation {
        return;
    }
    if receiver.has_changed().unwrap_or(false) {
        return;
    }
    entry.motion_pending = false;
    retire_if_idle(&mut routes, route_id, generation);
}

fn retire_if_idle(
    routes: &mut HashMap<String, RouteEntry>,
    route_id: &str,
    generation: u64,
) -> bool {
    let idle = routes.get(route_id).is_some_and(|entry| {
        entry.generation == generation && entry.discrete_pending == 0 && !entry.motion_pending
    });
    if idle {
        routes.remove(route_id);
    }
    idle
}

async fn forward(node: &NodeClient, route_id: &str, action: Value) -> Result<(), String> {
    node.request(
        "send_input",
        json!({ "route_id": route_id, "action": action }),
    )
    .await
    .map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(kind: &str, value: i64) -> Value {
        json!({ "kind": kind, "value": value })
    }

    fn entry(generation: u64, discrete_pending: usize, motion_pending: bool) -> RouteEntry {
        let (senders, _receivers) = RouteSenders::new();
        RouteEntry {
            generation,
            senders,
            discrete_pending,
            motion_pending,
        }
    }

    #[tokio::test]
    async fn discrete_lane_is_fifo() {
        let (senders, mut receivers) = RouteSenders::new();
        senders.enqueue(action("key", 1), false).unwrap();
        senders.enqueue(action("mouse_button", 2), false).unwrap();
        senders.enqueue(action("key", 3), false).unwrap();

        assert_eq!(
            receivers.discrete.recv().await.unwrap().action,
            action("key", 1)
        );
        assert_eq!(
            receivers.discrete.recv().await.unwrap().action,
            action("mouse_button", 2)
        );
        assert_eq!(
            receivers.discrete.recv().await.unwrap().action,
            action("key", 3)
        );
    }

    #[tokio::test]
    async fn pointer_motion_is_latest_only_and_separate_from_release() {
        let (senders, mut receivers) = RouteSenders::new();
        senders.enqueue(action("mouse_move", 1), false).unwrap();
        senders.enqueue(action("mouse_move", 2), false).unwrap();
        senders.enqueue(action("mouse_button", 3), false).unwrap();

        assert_eq!(
            receivers.discrete.recv().await.unwrap().action,
            action("mouse_button", 3)
        );
        receivers.motion.changed().await.unwrap();
        assert_eq!(
            receivers.motion.borrow_and_update().clone(),
            Some(action("mouse_move", 2))
        );
    }

    #[tokio::test]
    async fn ordered_pointer_reseat_uses_discrete_fifo() {
        let (senders, mut receivers) = RouteSenders::new();
        senders.enqueue(action("mouse_move", 1), true).unwrap();
        senders.enqueue(action("mouse_button", 2), false).unwrap();

        assert_eq!(
            receivers.discrete.recv().await.unwrap().action,
            action("mouse_move", 1)
        );
        assert_eq!(
            receivers.discrete.recv().await.unwrap().action,
            action("mouse_button", 2)
        );
    }

    #[test]
    fn idle_route_retires_but_old_generation_cannot_delete_successor() {
        let mut routes = HashMap::new();
        routes.insert("route-a".to_owned(), entry(2, 0, false));

        assert!(!retire_if_idle(&mut routes, "route-a", 1));
        assert_eq!(routes.get("route-a").map(|entry| entry.generation), Some(2));
        assert!(retire_if_idle(&mut routes, "route-a", 2));
        assert!(!routes.contains_key("route-a"));
    }

    #[test]
    fn route_stays_registered_while_either_lane_has_work() {
        let mut routes = HashMap::new();
        routes.insert("route-a".to_owned(), entry(3, 1, false));
        assert!(!retire_if_idle(&mut routes, "route-a", 3));

        routes.insert("route-a".to_owned(), entry(4, 0, true));
        assert!(!retire_if_idle(&mut routes, "route-a", 4));
    }
}
