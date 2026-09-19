// Included inside both the frozen host and the extracted host's test adapter.
// This builds channel-only sessions. It never constructs a PTY or a child.

pub(super) fn model() -> Box<dyn super::Model> {
    Box::new(Fixture {
        host: FixtureHost::new(),
        controls: HashMap::new(),
        events: HashMap::new(),
        attachments: HashMap::new(),
        kills: Arc::new(Mutex::new(Vec::new())),
    })
}

struct Fixture {
    host: FixtureHost,
    controls: HashMap<String, std::sync::mpsc::Receiver<CtlMsg>>,
    events: HashMap<String, tokio::sync::broadcast::Receiver<OutMsg>>,
    attachments: HashMap<String, tokio::sync::broadcast::Receiver<OutMsg>>,
    kills: Arc<Mutex<Vec<String>>>,
}

fn take_events(rx: &mut tokio::sync::broadcast::Receiver<OutMsg>) -> Vec<String> {
    let mut result = Vec::new();
    loop {
        match rx.try_recv() {
            Ok(msg) => result.push(format!("{msg:?}")),
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(n)) => {
                result.push(format!("lagged:{n}"));
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                result.push("closed".into());
                break;
            }
        }
    }
    result
}

impl super::Model for Fixture {
    fn seed(&mut self, sid: &str, route: &str, size: (u16, u16), capacity: usize) {
        let (ctl_tx, ctl_rx) = std::sync::mpsc::sync_channel(capacity);
        let (out_tx, out_rx) = tokio::sync::broadcast::channel(OUT_QUEUE);
        let mut attachers = HashMap::new();
        attachers.insert(route.into(), Attacher { cols: size.0, rows: size.1 });
        let session = PtySession {
            ctl_tx,
            killer: Box::new(super::ProbeKiller { sid: sid.into(), calls: self.kills.clone() }),
            out_tx,
            scrollback: Arc::new(Mutex::new(Scrollback::new(SCROLLBACK_CAP))),
            attachers,
            generation: 1,
            title: format!("title:{sid}"),
            created_unix: 42,
            last_size: size,
        };
        assert!(self.host.sessions.lock().insert(sid.into(), session).is_none());
        self.host.route_to_session.lock().insert(route.into(), sid.into());
        self.controls.insert(sid.into(), ctl_rx);
        self.events.insert(sid.into(), out_rx);
    }

    fn attach(&mut self, sid: &str, route: &str, size: (u16, u16)) -> super::Attachment {
        // A failed precondition must never fall through into native openpty.
        assert!(self.host.sessions.lock().contains_key(sid));
        let a = self.host.open_with(Some(sid), route, size.0, size.1, Vec::new()).unwrap();
        self.attachments.insert(route.into(), a.rx);
        super::Attachment { session_id: a.session_id, scrollback: a.scrollback, created: a.created }
    }

    fn capped_open(&self, sid: Option<&str>) -> String {
        assert!(self.host.sessions.lock().len() >= 32);
        assert!(sid.is_none_or(|s| !self.host.sessions.lock().contains_key(s)));
        self.host.open_with(sid, "new-route", 0, u16::MAX, Vec::new()).err().unwrap()
    }

    fn emit(&self, sid: &str, bytes: &[u8]) {
        let sessions = self.host.sessions.lock();
        let session = sessions.get(sid).unwrap();
        let mut sb = session.scrollback.lock();
        sb.append(bytes);
        let _ = session.out_tx.send(OutMsg::Data(bytes.to_vec()));
    }

    fn write(&self, route: &str, bytes: Vec<u8>) -> bool { self.host.write(route, bytes) }
    fn resize(&self, route: &str, cols: u16, rows: u16) -> bool { self.host.resize(route, cols, rows) }
    fn detach(&self, route: &str) { self.host.detach(route); }
    fn close(&self, sid: &str) { self.host.close(sid); }
    fn stop(&self, route: &str) { self.host.stop(route); }
    fn is_attached(&self, route: &str) -> bool { self.host.is_attached(route) }
    fn ensure_queue(&self, route: &str) { self.host.ensure_queue(route); }
    fn enqueue(&self, route: &str, data: Vec<u8>) -> bool { self.host.enqueue(route, data) }
    fn poll(&self, route: &str) -> Vec<u8> { self.host.poll(route) }

    fn remove_attacher_only(&self, sid: &str, route: &str) {
        self.host.sessions.lock().get_mut(sid).unwrap().attachers.remove(route);
    }

    fn dangling_route(&self, route: &str, sid: &str) {
        self.host.route_to_session.lock().insert(route.into(), sid.into());
    }

    fn disconnect_control(&mut self, sid: &str) {
        drop(self.controls.remove(sid).unwrap());
    }

    fn set_generation(&self, sid: &str, generation: u64) {
        self.host.sessions.lock().get_mut(sid).unwrap().generation = generation;
    }

    fn controls(&self, sid: &str) -> (Vec<String>, bool) {
        let mut messages = Vec::new();
        loop {
            match self.controls.get(sid).unwrap().try_recv() {
                Ok(CtlMsg::Data(bytes)) => messages.push(format!("Data({bytes:?})")),
                Ok(CtlMsg::Resize { cols, rows }) => messages.push(format!("Resize {cols}x{rows}")),
                Ok(CtlMsg::Shutdown) => messages.push("Shutdown".into()),
                Err(std::sync::mpsc::TryRecvError::Empty) => return (messages, false),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return (messages, true),
            }
        }
    }

    fn events(&mut self, sid: &str) -> Vec<String> {
        take_events(self.events.get_mut(sid).unwrap())
    }

    fn attachment_events(&mut self, route: &str) -> Vec<String> {
        take_events(self.attachments.get_mut(route).unwrap())
    }

    fn state(&self, sid: &str) -> serde_json::Value {
        let sessions = self.host.sessions.lock();
        let Some(s) = sessions.get(sid) else { return serde_json::Value::Null; };
        let attachers: std::collections::BTreeMap<_, _> = s.attachers.iter()
            .map(|(route, a)| (route.clone(), [a.cols, a.rows])).collect();
        let scrollback = s.scrollback.lock().snapshot();
        serde_json::json!({
            "attachers": attachers, "generation": s.generation,
            "last_size": [s.last_size.0, s.last_size.1], "scrollback": scrollback,
        })
    }

    fn list(&self) -> serde_json::Value {
        let mut rows = self.host.list_sessions();
        // Production intentionally exposes HashMap iteration order. Only the
        // comparison adapter sorts; no ordering is promised for that API.
        rows.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        serde_json::to_value(rows).unwrap()
    }

    fn next_session(&self) -> u64 { self.host.next_session.load(Ordering::Relaxed) }
    fn kills(&self) -> Vec<String> { self.kills.lock().clone() }
}

pub(super) fn constants() -> [u64; 7] {
    [READ_BUF as u64, OUT_QUEUE as u64, CTL_QUEUE as u64, MAX_QUEUED_BYTES as u64,
     SCROLLBACK_CAP as u64, SESSION_IDLE_REAP_MS, MAX_LOCAL_SESSIONS as u64]
}

pub(super) fn scrollback(cap: usize, chunks: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let mut ring = Scrollback::new(cap);
    chunks.iter().map(|chunk| { ring.append(chunk); ring.snapshot() }).collect()
}

pub(super) fn size(sizes: &[(u16, u16)]) -> (u16, u16) {
    let attachers = sizes.iter().enumerate().map(|(i, &(cols, rows))| {
        (i.to_string(), Attacher { cols, rows })
    }).collect();
    reconcile_size(&attachers)
}

pub(super) fn bridge(replay: Vec<u8>, capacity: usize) -> Box<dyn super::Bridge> {
    let (sender, rx) = tokio::sync::broadcast::channel(capacity);
    let attachment = TermAttach {
        session_id: "channel-only".into(), scrollback: replay, rx, created: false,
    };
    Box::new(ChannelBridge { sender: Some(sender), rx: fixture_bridge(attachment) })
}

struct ChannelBridge {
    sender: Option<tokio::sync::broadcast::Sender<OutMsg>>,
    rx: tokio::sync::mpsc::Receiver<OutMsg>,
}

impl super::Bridge for ChannelBridge {
    fn send(&self, msg: super::Message) {
        let msg = match msg {
            super::Message::Data(bytes) => OutMsg::Data(bytes),
            super::Message::Resize(cols, rows) => OutMsg::Resize { cols, rows },
            super::Message::Exit(code) => OutMsg::Exit(code),
        };
        self.sender.as_ref().unwrap().send(msg).unwrap();
    }
    fn close_source(&mut self) { self.sender = None; }
    fn close_sink(&mut self) { self.rx.close(); }
    fn output_capacity(&self) -> usize { self.rx.max_capacity() }
    fn queued(&self) -> usize { self.rx.len() }
    fn subscribers(&self) -> usize { self.sender.as_ref().unwrap().receiver_count() }
    fn take(&mut self) -> (Vec<String>, bool) {
        let mut messages = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(msg) => messages.push(format!("{msg:?}")),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => return (messages, false),
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return (messages, true),
            }
        }
    }
}
