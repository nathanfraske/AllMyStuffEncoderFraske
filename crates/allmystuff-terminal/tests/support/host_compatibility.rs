//! Channel-only comparisons with frozen host code. No child process or PTY.

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

type Task = Pin<Box<dyn Future<Output = ()> + Send>>;

thread_local! {
    static CAPTURED: RefCell<Option<Vec<Task>>> = const { RefCell::new(None) };
}

struct ProbeSpawner;

impl crate::TaskSpawner for ProbeSpawner {
    fn spawn<F>(future: F) -> tokio::task::JoinHandle<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        spawn_original(future)
    }
}

fn spawn_original<F>(future: F) -> tokio::task::JoinHandle<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    CAPTURED.with(|slot| {
        slot.borrow_mut()
            .as_mut()
            .expect("compatibility spawn probe")
            .push(Box::pin(future));
    });
    // Capture first: bridge tests poll manually, while reaper tests later run
    // the captured future on their private paused runtime. A detached empty
    // task supplies this handle without a global registry or worker thread.
    tokio::runtime::Handle::current().spawn(async {})
}

#[derive(Debug)]
struct ProbeKiller {
    sid: String,
    calls: Arc<parking_lot::Mutex<Vec<String>>>,
}

impl portable_pty::ChildKiller for ProbeKiller {
    fn kill(&mut self) -> std::io::Result<()> {
        self.calls.lock().push(self.sid.clone());
        Ok(())
    }
    fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
        Box::new(Self {
            sid: self.sid.clone(),
            calls: self.calls.clone(),
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Attachment {
    session_id: String,
    scrollback: Vec<u8>,
    created: bool,
}

trait Model {
    fn seed(&mut self, sid: &str, route: &str, size: (u16, u16), capacity: usize);
    fn attach(&mut self, sid: &str, route: &str, size: (u16, u16)) -> Attachment;
    fn capped_open(&self, sid: Option<&str>) -> String;
    fn emit(&self, sid: &str, bytes: &[u8]);
    fn write(&self, route: &str, bytes: Vec<u8>) -> bool;
    fn resize(&self, route: &str, cols: u16, rows: u16) -> bool;
    fn detach(&self, route: &str);
    fn close(&self, sid: &str);
    fn stop(&self, route: &str);
    fn is_attached(&self, route: &str) -> bool;
    fn ensure_queue(&self, route: &str);
    fn enqueue(&self, route: &str, data: Vec<u8>) -> bool;
    fn poll(&self, route: &str) -> Vec<u8>;
    fn remove_attacher_only(&self, sid: &str, route: &str);
    fn dangling_route(&self, route: &str, sid: &str);
    fn disconnect_control(&mut self, sid: &str);
    fn set_generation(&self, sid: &str, generation: u64);
    fn controls(&self, sid: &str) -> (Vec<String>, bool);
    fn events(&mut self, sid: &str) -> Vec<String>;
    fn attachment_events(&mut self, route: &str) -> Vec<String>;
    fn state(&self, sid: &str) -> serde_json::Value;
    fn list(&self) -> serde_json::Value;
    fn next_session(&self) -> u64;
    fn kills(&self) -> Vec<String>;
}

enum Message {
    Data(Vec<u8>),
    Resize(u16, u16),
    Exit(Option<i32>),
}

trait Bridge {
    fn send(&self, msg: Message);
    fn close_source(&mut self);
    fn close_sink(&mut self);
    fn output_capacity(&self) -> usize;
    fn queued(&self) -> usize;
    fn subscribers(&self) -> usize;
    fn take(&mut self) -> (Vec<String>, bool);
}

#[allow(dead_code)]
#[rustfmt::skip]
#[path = "frozen_host.rs"]
mod old;

mod new {
    use super::super::*;

    type FixtureHost = TerminalHost<super::ProbeSpawner>;

    fn fixture_bridge(attach: TermAttach) -> tokio::sync::mpsc::Receiver<OutMsg> {
        bridge_to_mpsc::<super::ProbeSpawner>(attach)
    }

    include!("host_harness.rs");
}

fn models() -> [Box<dyn Model>; 2] {
    [old::model(), new::model()]
}

fn hex(value: &str) -> Vec<u8> {
    assert_eq!(value.len() % 2, 0);
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn poll(task: &mut Task) -> Poll<()> {
    task.as_mut().poll(&mut Context::from_waker(Waker::noop()))
}

type BridgeFactory = fn(Vec<u8>, usize) -> Box<dyn Bridge>;

fn captured_bridge(factory: BridgeFactory, replay: Vec<u8>, cap: usize) -> (Box<dyn Bridge>, Task) {
    capture(|| factory(replay, cap))
}

fn capture<T>(action: impl FnOnce() -> T) -> (T, Task) {
    CAPTURED.with(|slot| assert!(slot.replace(Some(Vec::new())).is_none()));
    let result = action();
    let mut tasks = CAPTURED.with(|slot| slot.take().unwrap());
    assert_eq!(tasks.len(), 1);
    (result, tasks.pop().unwrap())
}

#[test]
fn constants_preserve_both_queue_limits_scrollback_idle_delay_and_session_cap() {
    let expected = [8192, 256, 256, 4_194_304, 262_144, 3_600_000, 32];
    assert_eq!(old::constants(), expected);
    assert_eq!(new::constants(), expected);
}

#[test]
fn literal_scrollback_traces_preserve_zero_exact_tail_and_binary_rules() {
    let vectors: serde_json::Value =
        serde_json::from_str(include_str!("../baseline/scrollback_vectors.json")).unwrap();
    for case in vectors.as_array().unwrap() {
        let cap = case["cap"].as_u64().unwrap() as usize;
        let chunks: Vec<_> = case["append_hex"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| hex(v.as_str().unwrap()))
            .collect();
        let expected: Vec<_> = case["snapshots_hex"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| hex(v.as_str().unwrap()))
            .collect();
        assert_eq!(old::scrollback(cap, &chunks), expected);
        assert_eq!(new::scrollback(cap, &chunks), expected);
    }
}

#[test]
fn full_size_scrollback_snapshots_remain_owned_and_cap_in_bytes() {
    let chunks = [vec![0x61; 262_144], vec![0xff; 3], Vec::new()];
    for snapshots in [
        old::scrollback(262_144, &chunks),
        new::scrollback(262_144, &chunks),
    ] {
        assert_eq!(snapshots[0], chunks[0]);
        assert_eq!(snapshots[1].len(), 262_144);
        assert!(snapshots[1][..262_141].iter().all(|&b| b == 0x61));
        assert_eq!(&snapshots[1][262_141..], &[255, 255, 255]);
        assert_eq!(snapshots[1], snapshots[2]);
    }
}

#[test]
fn literal_resize_vectors_keep_independent_minima_and_maximum_sentinel_fallback() {
    let vectors: serde_json::Value =
        serde_json::from_str(include_str!("../baseline/resize_vectors.json")).unwrap();
    for case in vectors.as_array().unwrap() {
        let mut sizes: Vec<_> = case["sizes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| (v[0].as_u64().unwrap() as u16, v[1].as_u64().unwrap() as u16))
            .collect();
        let expected = (
            case["expected"][0].as_u64().unwrap() as u16,
            case["expected"][1].as_u64().unwrap() as u16,
        );
        assert_eq!(old::size(&sizes), expected);
        assert_eq!(new::size(&sizes), expected);
        sizes.reverse();
        assert_eq!(old::size(&sizes), expected);
        assert_eq!(new::size(&sizes), expected);
    }
}

#[test]
fn input_preserves_binary_empty_order_and_exact_256_slot_rejection() {
    for mut model in models() {
        model.seed("s", "a", (80, 24), 256);
        assert!(model.write("a", vec![0, 27, 255]));
        assert!(model.write("a", Vec::new()));
        for _ in 2..256 {
            assert!(model.write("a", vec![1]));
        }
        assert!(!model.write("a", vec![99]));
        let (messages, closed) = model.controls("s");
        assert!(!closed);
        assert_eq!(messages.len(), 256);
        assert_eq!(&messages[..2], &["Data([0, 27, 255])", "Data([])"]);
        assert!(messages[2..].iter().all(|v| v == "Data([1])"));
        assert!(model.write("a", vec![2]));
        assert_eq!(model.controls("s"), (vec!["Data([2])".into()], false));
    }
}

#[test]
fn disconnected_control_refuses_input_without_removing_session_or_route() {
    for mut model in models() {
        model.seed("s", "a", (80, 24), 1);
        model.disconnect_control("s");
        assert!(!model.write("a", vec![7]));
        assert!(model.is_attached("a"));
        assert_eq!(
            model.state("s")["attachers"],
            serde_json::json!({"a": [80, 24]})
        );
        assert!(model.kills().is_empty());
    }
}

#[test]
fn equal_resize_still_queues_control_without_rebroadcasting() {
    for mut model in models() {
        model.seed("s", "a", (80, 24), 1);
        assert!(model.resize("a", 80, 24));
        assert!(!model.resize("a", 80, 24));
        assert_eq!(model.controls("s"), (vec!["Resize 80x24".into()], false));
        assert!(model.events("s").is_empty());
    }
}

#[test]
fn full_resize_updates_state_and_broadcast_even_when_control_send_fails() {
    for mut model in models() {
        model.seed("s", "a", (80, 24), 1);
        assert!(model.write("a", vec![1]));
        assert!(!model.resize("a", 0, 0));
        assert_eq!(
            model.state("s")["attachers"],
            serde_json::json!({"a": [0, 0]})
        );
        assert_eq!(model.state("s")["last_size"], serde_json::json!([1, 1]));
        assert_eq!(model.events("s"), ["Resize { cols: 1, rows: 1 }"]);
        assert_eq!(model.controls("s"), (vec!["Data([1])".into()], false));
        assert!(model.resize("a", 0, 0));
        assert_eq!(model.controls("s"), (vec!["Resize 1x1".into()], false));
        assert!(model.events("s").is_empty());
    }
}

#[test]
fn disconnected_resize_still_publishes_the_new_reconciled_size() {
    for mut model in models() {
        model.seed("s", "a", (80, 24), 1);
        model.disconnect_control("s");
        assert!(!model.resize("a", 120, 40));
        assert_eq!(model.state("s")["last_size"], serde_json::json!([120, 40]));
        assert_eq!(model.events("s"), ["Resize { cols: 120, rows: 40 }"]);
        assert!(model.is_attached("a"));
    }
}

#[test]
fn mapped_resize_reinserts_a_missing_attacher_but_absent_session_refuses() {
    for mut model in models() {
        model.seed("s", "a", (80, 24), 2);
        model.remove_attacher_only("s", "a");
        assert!(model.resize("a", 120, 20));
        assert_eq!(
            model.state("s")["attachers"],
            serde_json::json!({"a": [120, 20]})
        );
        model.dangling_route("missing", "absent");
        assert!(model.is_attached("missing"));
        assert!(!model.resize("missing", 1, 1));
        assert!(!model.write("missing", vec![1]));
    }
}

#[test]
fn attach_replays_before_live_data_and_inherits_size_without_minting_an_id() {
    for mut model in models() {
        model.seed("s\0é", "a", (120, 40), 4);
        model.emit("s\0é", &[0, 255]);
        let attach = model.attach("s\0é", "b", (0, 65535));
        assert_eq!(
            attach,
            Attachment {
                session_id: "s\0é".into(),
                scrollback: vec![0, 255],
                created: false
            }
        );
        assert_eq!(
            model.state("s\0é")["attachers"],
            serde_json::json!({"a": [120, 40], "b": [120, 40]})
        );
        assert!(model.attachment_events("b").is_empty());
        model.emit("s\0é", &[7]);
        assert_eq!(model.attachment_events("b"), ["Data([7])"]);
        assert_eq!(
            model.controls("s\0é"),
            (vec!["Resize 120x40".into()], false)
        );
        assert_eq!(model.next_session(), 1);
        assert_eq!(model.state("s\0é")["generation"], 1);
    }
}

#[test]
fn attach_succeeds_when_reconcile_queue_is_full_and_repeated_route_stays_one_attacher() {
    for mut model in models() {
        model.seed("s", "a", (120, 40), 1);
        assert!(model.write("a", vec![1]));
        assert!(!model.attach("s", "b", (1, 1)).created);
        assert!(!model.attach("s", "b", (65535, 0)).created);
        assert_eq!(
            model.state("s")["attachers"],
            serde_json::json!({"a": [120, 40], "b": [120, 40]})
        );
        assert_eq!(model.controls("s"), (vec!["Data([1])".into()], false));
        assert_eq!(model.state("s")["generation"], 1);
    }
}

#[test]
fn session_cap_rejects_creation_before_id_allocation_but_allows_existing_attach() {
    for mut model in models() {
        for n in 0..32 {
            model.seed(&format!("s{n}"), &format!("r{n}"), (80, 24), 2);
        }
        for id in [None, Some(""), Some("unknown")] {
            assert_eq!(
                model.capped_open(id),
                "too many terminal sessions open here (32); close one before opening another"
            );
        }
        assert!(!model.attach("s0", "extra", (1, 1)).created);
        assert_eq!(
            model.state("s0")["attachers"],
            serde_json::json!({"r0": [80, 24], "extra": [80, 24]})
        );
        assert_eq!(model.next_session(), 1);
        assert!(model.kills().is_empty());
    }
}

#[test]
fn reusing_a_route_for_another_session_retains_the_old_attacher_entry() {
    for mut model in models() {
        model.seed("first", "a", (80, 24), 4);
        model.seed("second", "b", (100, 30), 4);
        model.attach("first", "same-route", (1, 1));
        model.attach("second", "same-route", (1, 1));
        assert_eq!(
            model.state("first")["attachers"]["same-route"],
            serde_json::json!([80, 24])
        );
        assert_eq!(
            model.state("second")["attachers"]["same-route"],
            serde_json::json!([100, 30])
        );
        model.close("second");
        assert!(!model.is_attached("same-route"));
        assert_eq!(
            model.state("first")["attachers"]["same-route"],
            serde_json::json!([80, 24])
        );
    }
}

#[test]
fn session_listing_preserves_metadata_and_counts_without_promising_hashmap_order() {
    for mut model in models() {
        model.seed("z", "a", (80, 24), 4);
        model.seed("a", "b", (80, 24), 4);
        model.attach("z", "c", (1, 1));
        assert_eq!(
            model.list(),
            serde_json::json!([
                {"session_id":"a","title":"title:a","created_unix":42,"attachers":1},
                {"session_id":"z","title":"title:z","created_unix":42,"attachers":2}
            ])
        );
    }
}

#[test]
fn close_kills_once_despite_full_control_queue_and_preserves_viewer_buffers() {
    for mut model in models() {
        model.seed("s", "a", (80, 24), 1);
        model.attach("s", "b", (1, 1));
        for route in ["a", "b"] {
            model.ensure_queue(route);
            assert!(model.enqueue(route, vec![7]));
        }
        model.close("s");
        model.close("s");
        assert_eq!(model.kills(), ["s"]);
        assert!(!model.is_attached("a"));
        assert!(!model.is_attached("b"));
        assert_eq!(model.controls("s"), (vec!["Resize 80x24".into()], true));
        assert_eq!(model.events("s"), ["closed"]);
        for route in ["a", "b"] {
            assert_eq!(model.poll(route), [1, 0, 0, 0, 7]);
        }
    }
}

#[test]
fn stop_closes_shared_session_but_drops_only_the_requested_viewer_queue() {
    for mut model in models() {
        model.seed("s", "a", (80, 24), 4);
        model.attach("s", "b", (1, 1));
        for route in ["a", "b"] {
            model.ensure_queue(route);
            assert!(model.enqueue(route, vec![7]));
        }
        model.stop("a");
        assert_eq!(model.kills(), ["s"]);
        assert_eq!(
            model.controls("s"),
            (vec!["Resize 80x24".into(), "Shutdown".into()], true)
        );
        assert!(model.poll("a").is_empty());
        assert_eq!(model.poll("b"), [1, 0, 0, 0, 7]);
        assert!(!model.is_attached("b"));
    }
}

#[test]
fn detach_one_viewer_reconciles_survivor_and_removes_only_its_queue_without_spawn() {
    for mut model in models() {
        model.seed("s", "a", (120, 40), 8);
        model.attach("s", "b", (1, 1));
        assert!(model.resize("b", 80, 20));
        let _ = model.controls("s");
        let _ = model.events("s");
        for route in ["a", "b"] {
            model.ensure_queue(route);
            assert!(model.enqueue(route, vec![7]));
        }
        model.detach("b");
        assert_eq!(model.controls("s"), (vec!["Resize 120x40".into()], false));
        assert_eq!(model.events("s"), ["Resize { cols: 120, rows: 40 }"]);
        assert!(model.poll("b").is_empty());
        assert_eq!(model.poll("a"), [1, 0, 0, 0, 7]);
        assert_eq!(
            model.state("s")["attachers"],
            serde_json::json!({"a": [120, 40]})
        );
        assert!(model.kills().is_empty());
    }
}

#[test]
fn last_detach_mutates_maps_and_queue_before_the_spawn_policy_panics() {
    for mut model in models() {
        model.seed("s", "a", (80, 24), 2);
        model.ensure_queue("a");
        assert!(model.enqueue("a", vec![7]));
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| model.detach("a")))
            .unwrap_err();
        let text = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap();
        assert!(text.contains("compatibility spawn probe"));
        assert!(!model.is_attached("a"));
        assert!(model.poll("a").is_empty());
        assert_eq!(model.state("s")["attachers"], serde_json::json!({}));
        assert_eq!(model.state("s")["generation"], 1);
        assert!(model.kills().is_empty());
        assert_eq!(model.controls("s"), (Vec::new(), false));
        assert!(!model.attach("s", "new", (120, 40)).created);
        assert_eq!(
            model.state("s")["attachers"],
            serde_json::json!({"new": [80, 24]})
        );
        assert_eq!(model.state("s")["generation"], 1);
    }
}

#[test]
fn missing_session_close_and_detach_clean_dangling_routes_without_spawning() {
    for model in models() {
        model.dangling_route("a", "absent");
        model.ensure_queue("a");
        assert!(model.enqueue("a", vec![7]));
        model.close("absent");
        assert!(!model.is_attached("a"));
        assert_eq!(model.poll("a"), [1, 0, 0, 0, 7]);
        model.dangling_route("a", "absent");
        assert!(model.enqueue("a", vec![8]));
        model.detach("a");
        assert!(!model.is_attached("a"));
        assert!(model.poll("a").is_empty());
        assert!(model.kills().is_empty());
    }
}

fn paused_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
        .unwrap()
}

#[test]
fn idle_reaper_waits_the_original_hour_then_kills_and_shuts_down_empty_session() {
    for mut model in models() {
        paused_runtime().block_on(async {
            model.seed("s", "a", (80, 24), 2);
            let (_, mut future) = capture(|| model.detach("a"));
            assert!(poll(&mut future).is_pending());
            let task = tokio::spawn(future);
            tokio::task::yield_now().await;
            tokio::time::advance(std::time::Duration::from_millis(3_599_999)).await;
            assert!(!task.is_finished());
            assert!(model.kills().is_empty());
            assert_eq!(model.state("s")["attachers"], serde_json::json!({}));
            // Cross the deadline by one millisecond to respect Tokio's timer
            // tick granularity. Exact equality scheduling is not asserted.
            tokio::time::advance(std::time::Duration::from_millis(2)).await;
            task.await.unwrap();
            assert_eq!(model.kills(), ["s"]);
            assert!(model.state("s").is_null());
            assert_eq!(model.controls("s"), (vec!["Shutdown".into()], true));
        });
    }
}

#[test]
fn idle_reaper_keeps_a_reattached_session_without_bumping_generation() {
    for mut model in models() {
        paused_runtime().block_on(async {
            model.seed("s", "a", (120, 40), 2);
            let (_, mut future) = capture(|| model.detach("a"));
            assert!(poll(&mut future).is_pending());
            let task = tokio::spawn(future);
            tokio::task::yield_now().await;
            model.attach("s", "b", (1, 1));
            tokio::time::advance(std::time::Duration::from_millis(3_600_001)).await;
            task.await.unwrap();
            assert!(model.kills().is_empty());
            assert!(model.is_attached("b"));
            assert_eq!(model.state("s")["generation"], 1);
            assert_eq!(
                model.state("s")["attachers"],
                serde_json::json!({"b": [80, 24]})
            );
        });
    }
}

#[test]
fn idle_reaper_checks_generation_even_for_a_still_empty_session() {
    for mut model in models() {
        paused_runtime().block_on(async {
            model.seed("s", "a", (80, 24), 2);
            let (_, mut future) = capture(|| model.detach("a"));
            assert!(poll(&mut future).is_pending());
            let task = tokio::spawn(future);
            tokio::task::yield_now().await;
            // Synthetic mismatch reaches the existing guard. Production
            // creation still sets one, and never increments this field.
            model.set_generation("s", 2);
            tokio::time::advance(std::time::Duration::from_millis(3_600_001)).await;
            task.await.unwrap();
            assert!(model.kills().is_empty());
            assert_eq!(model.state("s")["generation"], 2);
            assert_eq!(model.state("s")["attachers"], serde_json::json!({}));
        });
    }
}

#[test]
fn original_reaper_can_reap_a_recycled_empty_id_with_the_same_literal_generation() {
    for mut model in models() {
        paused_runtime().block_on(async {
            model.seed("s", "a", (80, 24), 2);
            let (_, mut future) = capture(|| model.detach("a"));
            assert!(poll(&mut future).is_pending());
            let task = tokio::spawn(future);
            tokio::task::yield_now().await;
            model.close("s");
            assert_eq!(model.kills(), ["s"]);
            model.seed("s", "b", (100, 30), 2);
            // Hold the newer idle task without polling it: the first timer
            // alone observes the replacement session's unchanged generation.
            let (_, later_timer) = capture(|| model.detach("b"));
            assert_eq!(model.state("s")["generation"], 1);
            tokio::time::advance(std::time::Duration::from_millis(3_600_001)).await;
            task.await.unwrap();
            assert_eq!(model.kills(), ["s", "s"]);
            assert!(model.state("s").is_null());
            assert_eq!(model.controls("s"), (vec!["Shutdown".into()], true));
            drop(later_timer);
        });
    }
}

#[test]
fn bridge_replays_one_chunk_then_keeps_forwarding_after_exit_until_source_closes() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    for factory in [old::bridge as BridgeFactory, new::bridge] {
        let (mut bridge, mut task) = captured_bridge(factory, vec![0, 255], 8);
        assert_eq!(bridge.output_capacity(), 256);
        bridge.send(Message::Resize(0, 65535));
        bridge.send(Message::Exit(Some(-1)));
        bridge.send(Message::Data(vec![7]));
        assert!(poll(&mut task).is_pending());
        assert_eq!(
            bridge.take(),
            (
                vec![
                    "Data([0, 255])".into(),
                    "Resize { cols: 0, rows: 65535 }".into(),
                    "Exit(Some(-1))".into(),
                    "Data([7])".into()
                ],
                false
            )
        );
        assert_eq!(bridge.subscribers(), 1);
        bridge.close_source();
        assert!(poll(&mut task).is_ready());
        assert_eq!(bridge.take(), (Vec::new(), true));
    }
}

#[test]
fn empty_replay_is_omitted_and_already_closed_source_finishes() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    for factory in [old::bridge as BridgeFactory, new::bridge] {
        let (mut bridge, mut task) = captured_bridge(factory, Vec::new(), 2);
        bridge.close_source();
        assert!(poll(&mut task).is_ready());
        assert_eq!(bridge.take(), (Vec::new(), true));
    }
}

#[test]
fn bridge_lag_skips_evicted_messages_and_keeps_retained_binary_and_exit() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    for factory in [old::bridge as BridgeFactory, new::bridge] {
        let (mut bridge, mut task) = captured_bridge(factory, vec![9], 2);
        bridge.send(Message::Data(vec![1]));
        bridge.send(Message::Data(vec![2]));
        bridge.send(Message::Data(vec![0, 255]));
        bridge.send(Message::Exit(None));
        bridge.close_source();
        assert!(poll(&mut task).is_ready());
        assert_eq!(
            bridge.take(),
            (
                vec![
                    "Data([9])".into(),
                    "Data([0, 255])".into(),
                    "Exit(None)".into()
                ],
                true
            )
        );
    }
}

#[test]
fn bridge_stalls_at_256_messages_and_resumes_in_order_after_the_sink_drains() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    for factory in [old::bridge as BridgeFactory, new::bridge] {
        let (mut bridge, mut task) = captured_bridge(factory, Vec::new(), 512);
        for n in 0u16..300 {
            bridge.send(Message::Data(n.to_le_bytes().to_vec()));
        }
        // A bounded number of polls also accommodates Tokio's cooperative
        // yield budget without a scheduler race or a wall-clock wait.
        for _ in 0..8 {
            assert!(poll(&mut task).is_pending());
            if bridge.queued() == 256 {
                break;
            }
        }
        assert_eq!(bridge.queued(), 256);
        let expected: Vec<_> = (0u16..300)
            .map(|n| format!("Data({:?})", n.to_le_bytes()))
            .collect();
        assert_eq!(bridge.take(), (expected[..256].to_vec(), false));
        for _ in 0..8 {
            assert!(poll(&mut task).is_pending());
            if bridge.queued() == 44 {
                break;
            }
        }
        assert_eq!(bridge.take(), (expected[256..].to_vec(), false));
        bridge.close_source();
        assert!(poll(&mut task).is_ready());
        assert_eq!(bridge.take(), (Vec::new(), true));
    }
}

#[test]
fn closed_sink_with_empty_replay_waits_for_live_input_before_unsubscribing() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    for factory in [old::bridge as BridgeFactory, new::bridge] {
        let (mut bridge, mut task) = captured_bridge(factory, Vec::new(), 2);
        bridge.close_sink();
        assert!(poll(&mut task).is_pending());
        assert_eq!(bridge.subscribers(), 1);
        bridge.send(Message::Data(vec![7]));
        assert!(poll(&mut task).is_ready());
        assert_eq!(bridge.subscribers(), 0);
        assert_eq!(bridge.take(), (Vec::new(), true));
    }
}

#[test]
fn closed_sink_rejects_nonempty_replay_and_drops_the_broadcast_subscription() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    for factory in [old::bridge as BridgeFactory, new::bridge] {
        let (mut bridge, mut task) = captured_bridge(factory, vec![7], 2);
        bridge.close_sink();
        assert!(poll(&mut task).is_ready());
        assert_eq!(bridge.subscribers(), 0);
        assert_eq!(bridge.take(), (Vec::new(), true));
    }
}
