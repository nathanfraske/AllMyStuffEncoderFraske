//! Public, non-PTY contracts checked against independently frozen literals.

mod byte_queues {
    pub use allmystuff_byte_queues::ByteQueues;
}

#[allow(dead_code)]
#[rustfmt::skip]
#[path = "baseline/terminal_stub.rs"]
mod old_stub;

#[cfg(feature = "host")]
struct ViewerSpawner {
    _not_send_or_sync: std::marker::PhantomData<std::rc::Rc<()>>,
}

#[cfg(feature = "host")]
impl allmystuff_terminal::TaskSpawner for ViewerSpawner {
    fn spawn<F>(_: F) -> tokio::task::JoinHandle<()>
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        panic!("viewer-only contract unexpectedly spawned a task")
    }
}

#[cfg(feature = "host")]
type NewHost = allmystuff_terminal::TerminalHost<ViewerSpawner>;
#[cfg(not(feature = "host"))]
type NewHost = allmystuff_terminal::TerminalHost;

trait Viewer {
    fn ensure_queue(&self, route: &str);
    fn watch_output(&self, route: &str) -> u64;
    fn unwatch(&self, route: &str, token: u64);
    fn poll(&self, route: &str) -> Vec<u8>;
    fn enqueue(&self, route: &str, bytes: Vec<u8>) -> bool;
    fn close(&self, session: &str);
    fn stop(&self, route: &str);
}

macro_rules! viewer_adapter {
    ($ty:ty) => {
        impl Viewer for $ty {
            fn ensure_queue(&self, route: &str) {
                <$ty>::ensure_queue(self, route);
            }
            fn watch_output(&self, route: &str) -> u64 {
                <$ty>::watch_output(self, route)
            }
            fn unwatch(&self, route: &str, token: u64) {
                <$ty>::unwatch(self, route, token);
            }
            fn poll(&self, route: &str) -> Vec<u8> {
                <$ty>::poll(self, route)
            }
            fn enqueue(&self, route: &str, bytes: Vec<u8>) -> bool {
                <$ty>::enqueue(self, route, bytes)
            }
            fn close(&self, session: &str) {
                <$ty>::close(self, session);
            }
            fn stop(&self, route: &str) {
                <$ty>::stop(self, route);
            }
        }
    };
}

viewer_adapter!(old_stub::TerminalHost);
viewer_adapter!(allmystuff_terminal::viewer::TerminalHost);
#[cfg(feature = "host")]
viewer_adapter!(NewHost);

fn viewers() -> Vec<Box<dyn Viewer>> {
    vec![
        Box::new(old_stub::TerminalHost::default()),
        Box::new(allmystuff_terminal::viewer::TerminalHost::new()),
        #[cfg(feature = "host")]
        Box::new(NewHost::new()),
    ]
}

fn hex(value: &str) -> Vec<u8> {
    assert_eq!(value.len() % 2, 0);
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn contract() -> serde_json::Value {
    serde_json::from_str(include_str!("baseline/contract_vectors.json")).unwrap()
}

#[test]
fn session_info_preserves_snake_case_order_and_opaque_identity_bytes() {
    let old = old_stub::SessionInfo {
        session_id: "s\0é".into(),
        title: "shell".into(),
        created_unix: 42,
        attachers: 2,
    };
    let new = allmystuff_terminal::SessionInfo {
        session_id: "s\0é".into(),
        title: "shell".into(),
        created_unix: 42,
        attachers: 2,
    };
    let literals = contract();
    let expected = literals["session_info_json"].as_str().unwrap().as_bytes();
    assert_eq!(serde_json::to_vec(&old).unwrap(), expected);
    assert_eq!(serde_json::to_vec(&new).unwrap(), expected);
    assert_eq!(serde_json::to_vec(&new.clone()).unwrap(), expected);
}

#[test]
fn output_message_variants_and_clone_keep_the_original_shape() {
    let old = [
        old_stub::OutMsg::Data(vec![0, 27, 255]),
        old_stub::OutMsg::Resize {
            cols: 65535,
            rows: 0,
        },
        old_stub::OutMsg::Exit(None),
        old_stub::OutMsg::Exit(Some(-1)),
    ];
    let new = [
        allmystuff_terminal::OutMsg::Data(vec![0, 27, 255]),
        allmystuff_terminal::OutMsg::Resize {
            cols: 65535,
            rows: 0,
        },
        allmystuff_terminal::OutMsg::Exit(None),
        allmystuff_terminal::OutMsg::Exit(Some(-1)),
    ];
    let literals = contract();
    for ((old, new), expected) in old
        .iter()
        .zip(&new)
        .zip(literals["outmsg_debug"].as_array().unwrap())
    {
        assert_eq!(format!("{old:?}"), expected.as_str().unwrap());
        assert_eq!(format!("{new:?}"), expected.as_str().unwrap());
        assert_eq!(format!("{:?}", new.clone()), expected.as_str().unwrap());
    }
    let mut clone = new[0].clone();
    if let allmystuff_terminal::OutMsg::Data(bytes) = &mut clone {
        bytes[0] = 9;
    } else {
        panic!("Data changed variant on clone");
    }
    assert_eq!(format!("{:?}", new[0]), "Data([0, 27, 255])");
}

#[test]
fn attachment_fields_retain_an_owned_replay_and_broadcast_receiver() {
    let (sender, rx) = tokio::sync::broadcast::channel(2);
    let mut attachment = allmystuff_terminal::TermAttach {
        session_id: "fixture".into(),
        scrollback: vec![0, 255],
        rx,
        created: false,
    };
    assert_eq!(attachment.session_id, "fixture");
    assert_eq!(attachment.scrollback, [0, 255]);
    assert!(!attachment.created);
    sender
        .send(allmystuff_terminal::OutMsg::Exit(Some(3)))
        .unwrap();
    assert!(matches!(
        attachment.rx.try_recv().unwrap(),
        allmystuff_terminal::OutMsg::Exit(Some(3))
    ));
    drop(sender);
    assert!(matches!(
        attachment.rx.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Closed)
    ));
}

#[test]
fn construction_and_missing_routes_need_no_runtime_or_shell() {
    let host = NewHost::default();
    assert!(host.list_sessions().is_empty());
    assert!(!host.is_attached("missing"));
    assert!(!host.write("missing", vec![0, 255]));
    assert!(!host.resize("missing", 0, 65535));
    host.close("missing");
    host.detach("missing");
    host.stop("missing");
    assert!(host.poll("missing").is_empty());
}

#[cfg(feature = "host")]
#[test]
fn host_marker_does_not_add_send_or_sync_constraints() {
    fn require_send_sync<T: Send + Sync>() {}
    require_send_sync::<NewHost>();
}

#[test]
fn unwatched_output_is_dropped_without_implicitly_creating_a_queue() {
    for host in viewers() {
        assert!(!host.enqueue("r", b"lost".to_vec()));
        assert!(host.poll("r").is_empty());
        assert_eq!(host.watch_output("r"), 1);
        assert!(host.poll("r").is_empty());
    }
}

#[test]
fn literal_framing_preserves_binary_and_empty_chunks() {
    let vectors: serde_json::Value =
        serde_json::from_str(include_str!("baseline/viewer_vectors.json")).unwrap();
    for case in vectors["literal_frames"].as_array().unwrap() {
        for host in viewers() {
            host.ensure_queue("r");
            for (index, chunk) in case["chunks_hex"].as_array().unwrap().iter().enumerate() {
                assert_eq!(host.enqueue("r", hex(chunk.as_str().unwrap())), index == 0);
            }
            assert_eq!(host.poll("r"), hex(case["poll_hex"].as_str().unwrap()));
            assert!(host.poll("r").is_empty());
        }
    }
}

#[test]
fn eager_and_replaced_watchers_retain_bytes_but_scope_unwatch() {
    for host in viewers() {
        host.ensure_queue("r");
        assert!(host.enqueue("r", b"a".to_vec()));
        assert_eq!(host.watch_output("r"), 1);
        assert_eq!(host.watch_output("other"), 2);
        assert_eq!(host.watch_output("r"), 3);
        host.unwatch("r", 1);
        host.unwatch("r", 0);
        assert!(!host.enqueue("r", b"b".to_vec()));
        assert_eq!(host.poll("r"), hex("01000000610100000062"));
        host.unwatch("r", 3);
        assert!(!host.enqueue("r", b"lost".to_vec()));
        assert!(host.enqueue("other", b"c".to_vec()));
        assert_eq!(host.poll("other"), hex("0100000063"));
    }
}

#[test]
fn repeated_ensure_preserves_the_queue_and_current_owner() {
    for host in viewers() {
        assert_eq!(host.watch_output("r"), 1);
        assert!(host.enqueue("r", vec![]));
        host.ensure_queue("r");
        host.ensure_queue("r");
        host.unwatch("r", 0);
        assert!(!host.enqueue("r", b"ab".to_vec()));
        assert_eq!(host.poll("r"), hex("00000000020000006162"));
        host.unwatch("r", 1);
        assert!(!host.enqueue("r", b"lost".to_vec()));
    }
}

#[test]
fn zero_unwatch_removes_an_unclaimed_eager_queue() {
    for host in viewers() {
        host.ensure_queue("r");
        assert!(host.enqueue("r", b"a".to_vec()));
        host.unwatch("r", 0);
        assert!(host.poll("r").is_empty());
        assert!(!host.enqueue("r", b"b".to_vec()));
    }
}

#[test]
fn exact_four_mib_payload_cap_excludes_framing_and_empty_chunks() {
    for host in viewers() {
        host.ensure_queue("r");
        assert!(host.enqueue("r", vec![]));
        assert!(!host.enqueue("r", vec![0xa5; 4_194_304]));
        let actual = host.poll("r");
        assert_eq!(actual.len(), 4_194_312);
        assert_eq!(&actual[..8], &[0, 0, 0, 0, 0, 0, 64, 0]);
        assert!(actual[8..].iter().all(|&b| b == 0xa5));
    }
}

#[test]
fn oversized_single_chunk_is_evicted_but_still_requests_a_poll() {
    for host in viewers() {
        host.ensure_queue("r");
        assert!(host.enqueue("r", vec![0xa5; 4_194_305]));
        assert!(host.poll("r").is_empty());
        assert!(host.enqueue("r", vec![255]));
        assert_eq!(host.poll("r"), hex("01000000ff"));
    }
}

#[test]
fn overflow_evicts_whole_old_chunks_and_restarts_the_empty_transition() {
    for host in viewers() {
        host.ensure_queue("r");
        assert!(host.enqueue("r", vec![0xa5; 4_194_303]));
        assert!(!host.enqueue("r", vec![0, 255]));
        assert_eq!(host.poll("r"), hex("0200000000ff"));
        assert!(host.enqueue("r", vec![7]));
        assert_eq!(host.poll("r"), hex("0100000007"));
    }
}

#[test]
fn queue_budget_and_watcher_tokens_are_per_route_and_host() {
    for host in viewers() {
        assert_eq!(host.watch_output("a"), 1);
        assert_eq!(host.watch_output("b"), 2);
        assert!(host.enqueue("a", vec![1; 4_194_304]));
        assert!(host.enqueue("b", vec![2; 4_194_304]));
        for (key, byte) in [("a", 1), ("b", 2)] {
            let data = host.poll(key);
            assert_eq!(&data[..4], &[0, 0, 64, 0]);
            assert_eq!(data.len(), 4_194_308);
            assert!(data[4..].iter().all(|&b| b == byte));
        }
    }
}

#[test]
fn closing_a_nonexistent_session_retains_both_viewer_queues() {
    for host in viewers() {
        host.ensure_queue("r");
        host.ensure_queue("other");
        assert!(host.enqueue("r", b"a".to_vec()));
        assert!(host.enqueue("other", b"b".to_vec()));
        host.close("r");
        assert_eq!(host.poll("r"), hex("0100000061"));
        assert_eq!(host.poll("other"), hex("0100000062"));
    }
}

#[test]
fn stopping_a_viewer_route_removes_only_that_queue() {
    for host in viewers() {
        host.ensure_queue("r");
        host.ensure_queue("other");
        assert!(host.enqueue("r", b"a".to_vec()));
        assert!(host.enqueue("other", b"b".to_vec()));
        host.stop("r");
        host.stop("r");
        assert!(host.poll("r").is_empty());
        assert!(!host.enqueue("r", vec![]));
        assert_eq!(host.poll("other"), hex("0100000062"));
    }
}

#[test]
fn detach_keeps_the_original_host_and_stub_viewer_difference() {
    let old = old_stub::TerminalHost::new();
    old.ensure_queue("r");
    assert!(old.enqueue("r", b"a".to_vec()));
    old.detach("r");
    assert_eq!(old.poll("r"), hex("0100000061"));

    let new = NewHost::new();
    new.ensure_queue("r");
    assert!(new.enqueue("r", b"a".to_vec()));
    new.detach("r");
    #[cfg(feature = "host")]
    assert!(new.poll("r").is_empty());
    #[cfg(not(feature = "host"))]
    assert_eq!(new.poll("r"), hex("0100000061"));
}

#[test]
fn explicit_viewer_refuses_hosting_even_when_host_feature_is_unified() {
    let old = old_stub::TerminalHost::new();
    let new = allmystuff_terminal::viewer::TerminalHost::new();
    let literals = contract();
    let expected = literals["errors"]["disabled_host"].as_str().unwrap();
    for session in [None, Some(""), Some("s\0é")] {
        assert_eq!(old.open(session, "r\0", 0, 65535).err().unwrap(), expected);
        assert_eq!(new.open(session, "r\0", 0, 65535).err().unwrap(), expected);
    }
    assert_eq!(old.spawn("r").err().unwrap(), expected);
    assert_eq!(new.spawn("r").err().unwrap(), expected);
    new.ensure_queue("r");
    assert!(new.enqueue("r", b"a".to_vec()));
    assert_eq!(new.spawn("r").err().unwrap(), expected);
    assert_eq!(new.poll("r"), hex("0100000061"));
    assert!(new.list_sessions().is_empty());
    assert!(!new.is_attached("r"));
}
