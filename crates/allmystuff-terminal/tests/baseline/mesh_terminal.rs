    /// The host side of a terminal route going active: spawn this user's
    /// shell and pump its output to the viewer. The owner/fleet gate
    /// already ran at offer time ([`terminal_offer_refusal`]); it's
    /// re-checked here — and on every inbound byte — so a session can
    /// never outlive the authorization that allowed it.
    fn start_terminal_host(self: &Arc<Self>, route: &Route) {
        let viewer = node_of(route.to.as_str());
        let peer = self.route_peer(&route.id).unwrap_or(viewer);
        let rid = route.id.clone();
        if !self.sender_may_drive(&peer, DrivePlane::Terminal) {
            tracing::warn!(
                "route {rid} — terminal for non-controller {} refused",
                short_id(&peer)
            );
            let mesh = self.clone();
            crate::spawn(async move {
                let _ = mesh.disconnect(rid).await;
            });
            return;
        }
        // One pump per viewer route. A duplicate `StartMedia` for this route
        // — the offer arriving on more than one shared network, say — must
        // not spawn a second pump onto it: two pumps fan the one shell's
        // output out twice (doubled/tripled terminal). The first start wins;
        // later duplicates are ignored until the pump ends and releases.
        if !self.term_pumps.lock().insert(rid.clone()) {
            tracing::debug!(
                "route {rid} — terminal pump already running; ignoring duplicate start"
            );
            return;
        }
        // The session the viewer asked to attach to: `Some(id)` joins that
        // shared shell (tmux-style — scrollback replayed, keyboard shared),
        // `None` mints a fresh one. The default emulator size is 80×24; the
        // viewer's first resize reconciles the shared PTY to its real size.
        let requested = self.requested_term_session(&route.id);
        match self
            .terminal
            .open(requested.as_deref(), &rid, TERM_INIT_COLS, TERM_INIT_ROWS)
        {
            Ok(attach) => {
                let session_id = attach.session_id.clone();
                tracing::info!(
                    "route {rid} active — {} terminal session {session_id} for {} ({} now attached)",
                    if attach.created { "hosting new" } else { "attaching to" },
                    short_id(&peer),
                    self.terminal
                        .list_sessions()
                        .iter()
                        .find(|s| s.session_id == session_id)
                        .map(|s| s.attachers)
                        .unwrap_or(1),
                );
                // Record the resolved id on our (host) route and echo it to
                // the viewer on a follow-up Accept, so its UI learns which
                // shell this is (and how to re-attach). Best-effort: the
                // first Accept already started the viewer's media.
                self.record_and_announce_term_session(&route.id, &peer, &session_id);
                let mesh = self.clone();
                crate::spawn(async move {
                    mesh.clone()
                        .pump_term_attach(rid.clone(), peer, attach)
                        .await;
                    // The pump ended (viewer detached, shell exited) — release
                    // the route so a genuine fresh start can pump again.
                    mesh.term_pumps.lock().remove(&rid);
                });
            }
            Err(e) => {
                // The shell never opened — release the route we just claimed.
                self.term_pumps.lock().remove(&rid);
                // Tell the viewer in its own terms — a terminal renders a
                // line of text better than a silently vanished route — then
                // tear the route down.
                tracing::warn!("route {rid} — shell didn't start: {e}");
                let mesh = self.clone();
                crate::spawn(async move {
                    let note = format!("[couldn't start a shell here: {e}]\r\n");
                    for frame in [
                        TermFrame::new(
                            &rid,
                            0,
                            TermEvent::Data {
                                bytes: note.into_bytes(),
                            },
                        ),
                        TermFrame::new(&rid, 1, TermEvent::Exit { code: None }),
                    ] {
                        if let Ok(payload) = serde_json::to_value(&frame) {
                            let _ = mesh.send_media_value(&peer, payload).await;
                        }
                    }
                    let _ = mesh.disconnect(rid).await;
                });
            }
        }
    }

    /// The terminal session this route asked to attach to, from the session
    /// snapshot — `Some(id)` for an explicit attach, `None` for "new shell".
    fn requested_term_session(&self, route_id: &str) -> Option<String> {
        self.state
            .lock()
            .session
            .as_ref()
            .and_then(|s| s.route(route_id))
            .and_then(|r| r.term_session.clone())
    }

    /// Record the resolved terminal session id on this (host) route, then
    /// echo it to the viewer with a follow-up `Accept` so its UI learns the
    /// shared id (for "shared with N" and re-attach). The first Accept the
    /// session auto-sent already started the viewer's media; this one only
    /// carries the resolved id.
    fn record_and_announce_term_session(
        self: &Arc<Self>,
        route_id: &str,
        peer: &str,
        session: &str,
    ) {
        {
            let mut st = self.state.lock();
            if let Some(s) = st.session.as_mut() {
                s.set_term_session(route_id, session.to_string());
            }
        }
        self.emit_snapshot();
        let mesh = self.clone();
        let peer = peer.to_string();
        let route_id = route_id.to_string();
        let session = session.to_string();
        crate::spawn(async move {
            let _ = mesh
                .send_control(
                    &peer,
                    &ControlMessage::Route(RouteControl::Accept {
                        route_id,
                        session: Some(session),
                        paced_video: false,
                    }),
                )
                .await;
        });
    }

    /// Pump one attacher's view of a shared terminal session to its viewer:
    /// replay the scrollback first (a fresh attach paints the current
    /// screen), then forward the session's live broadcast — this attacher's
    /// own pump to its own viewer route, so several viewers on one session
    /// each get the output (and, via `term_send`→`terminal.write`, each type
    /// into the one shell). `Lagged` skips ahead (output is live media);
    /// `Closed`/`Exit` ends *this* viewer's pump only.
    async fn pump_term_attach(
        self: Arc<Self>,
        rid: String,
        peer: String,
        attach: crate::terminal::TermAttach,
    ) {
        use tokio::sync::broadcast::error::RecvError;
        let crate::terminal::TermAttach {
            scrollback, mut rx, ..
        } = attach;
        let mut seq: u64 = 0;
        let mut last_ok = std::time::Instant::now();
        let mut last_warn = std::time::Instant::now() - WARN_EVERY;

        // Replay the current screen to *this* viewer before the live stream.
        if !scrollback.is_empty() {
            for frame in TermFrame::data_frames(&rid, seq, &scrollback, MAX_TERM_DATA_BYTES) {
                seq = frame.seq + 1;
                if let Ok(payload) = serde_json::to_value(&frame) {
                    let _ = self.send_media_value(&peer, payload).await;
                }
            }
        }

        loop {
            let msg = match rx.recv().await {
                Ok(msg) => msg,
                // A slow attacher fell behind the broadcast ring — output is
                // live media, so skip ahead rather than wedge the shell.
                Err(RecvError::Lagged(n)) => {
                    tracing::debug!("terminal {rid} — viewer lagged {n} chunks; skipping ahead");
                    continue;
                }
                // The session ended (shell exited / closed) — end this pump.
                Err(RecvError::Closed) => return,
            };
            // This viewer detached (closed its tab, or its route was torn
            // down) — stop pumping to it. The shell lives on for the other
            // attachers; the last one leaving arms the idle reaper. Checked
            // here so a closed viewer's pump never keeps streaming to a dead
            // route.
            if !self.terminal.is_attached(&rid) {
                return;
            }
            match msg {
                OutMsg::Data(bytes) => {
                    for frame in TermFrame::data_frames(&rid, seq, &bytes, MAX_TERM_DATA_BYTES) {
                        seq = frame.seq + 1;
                        let Ok(payload) = serde_json::to_value(&frame) else {
                            continue;
                        };
                        match self.send_media_value(&peer, payload).await {
                            Ok(()) => last_ok = std::time::Instant::now(),
                            Err(e) => {
                                if last_warn.elapsed() >= WARN_EVERY {
                                    last_warn = std::time::Instant::now();
                                    tracing::warn!(
                                        "terminal output to {} failed: {e}",
                                        short_id(&peer)
                                    );
                                }
                                // Nothing else reaps a session whose viewer
                                // silently vanished (peer drops never reach
                                // the session) — the pump is the watchdog.
                                // Detach this viewer only; the shell lives on
                                // for the other attachers (or a re-attach
                                // that replays scrollback), never killed
                                // because one viewer's link blipped.
                                if last_ok.elapsed() > TERM_SEND_PATIENCE {
                                    tracing::warn!(
                                        "terminal {rid} — viewer unreachable; detaching (shell kept for reattach)"
                                    );
                                    self.terminal.detach(&rid);
                                    return;
                                }
                            }
                        }
                    }
                }
                OutMsg::Resize { cols, rows } => {
                    // The shared PTY's authoritative size changed — tell this
                    // viewer so it renders (letterboxes) to the one shell's
                    // size and its wrapping matches everyone else's.
                    let frame = TermFrame::new(&rid, seq, TermEvent::Resize { cols, rows });
                    seq += 1;
                    if let Ok(payload) = serde_json::to_value(&frame) {
                        let _ = self.send_media_value(&peer, payload).await;
                    }
                }
                OutMsg::Exit(code) => {
                    tracing::info!("terminal {rid} — shell ended ({code:?})");
                    let frame = TermFrame::new(&rid, seq, TermEvent::Exit { code });
                    if let Ok(payload) = serde_json::to_value(&frame) {
                        let _ = self.send_media_value(&peer, payload).await;
                    }
                    // The shell ended for *everyone* on this session — tear
                    // this viewer's route down. Other attachers' pumps see
                    // the same `Exit`/`Closed` and end on their own.
                    let _ = self.disconnect(rid.clone()).await;
                    return;
                }
            }
        }
    }

    /// A **loopback** terminal route going active: a terminal to the very
    /// machine we're sitting at, where this node is both shell *and* viewer.
    /// There's no peer, so instead of framing the PTY's output onto the mesh
    /// we feed it straight into the local viewer queue (the same one the
    /// remote viewer path enqueues into) and poke the window — the Terminal
    /// UI can't tell a loopback session from a remote one. Keystrokes and
    /// resizes from the window short-circuit to `terminal.write/resize`
    /// locally (see [`Self::term_send`]). The owner/fleet gate is re-cleared
    /// for consistency with the remote host path — it's our own machine, so
    /// it passes.
    fn start_terminal_loopback(self: &Arc<Self>, route: &Route) {
        let rid = route.id.clone();
        // The peer here is ourselves; the gate must still pass (owner or a
        // fleet member always controls their own machine), and re-running it
        // keeps the loopback path honest with the remote one.
        let peer = self
            .route_peer(&rid)
            .unwrap_or_else(|| node_of(route.to.as_str()));
        if !self.sender_may_drive(&peer, DrivePlane::Terminal) {
            tracing::warn!(
                "route {rid} — local terminal refused (not owner/fleet of this machine)"
            );
            let mesh = self.clone();
            crate::spawn(async move {
                let _ = mesh.disconnect(rid).await;
            });
            return;
        }
        // One pump per route, exactly as the remote host path: a duplicate
        // local `StartMedia` must not spawn a second loopback pump onto this
        // route (which would double the window's output). First start wins.
        if !self.term_pumps.lock().insert(rid.clone()) {
            tracing::debug!(
                "route {rid} — local terminal pump already running; ignoring duplicate"
            );
            return;
        }
        // Buffer output from the very first byte — the shell's prompt is
        // produced right after Accept, before the window has subscribed, and
        // a dropped terminal byte never heals.
        self.terminal.ensure_queue(&rid);
        // The session this local window asked to attach to: `Some(id)` lets
        // two local windows share one local shell (multi-attach to yourself),
        // `None` mints a fresh one — the same session model as the remote
        // host path, just feeding the local queue instead of the mesh.
        let requested = self.requested_term_session(&rid);
        match self
            .terminal
            .open(requested.as_deref(), &rid, TERM_INIT_COLS, TERM_INIT_ROWS)
        {
            Ok(attach) => {
                let session_id = attach.session_id.clone();
                tracing::info!(
                    "route {rid} active — local terminal session {session_id} ({})",
                    if attach.created {
                        "new shell"
                    } else {
                        "attached"
                    },
                );
                // Record the resolved id locally so a snapshot surfaces it
                // (the loopback UI shows the same "shared with N" line); there
                // is no peer to Accept back to.
                {
                    let mut st = self.state.lock();
                    if let Some(s) = st.session.as_mut() {
                        s.set_term_session(&rid, session_id.clone());
                    }
                }
                self.emit_snapshot();
                let crate::terminal::TermAttach {
                    scrollback, mut rx, ..
                } = attach;
                // Replay the current screen into this window's queue first
                // (an attach to an already-running local shell paints it),
                // then pump the shared broadcast in.
                if !scrollback.is_empty() && self.terminal.enqueue(&rid, scrollback) {
                    self.sink.emit("allmystuff://term-ready", json!(rid));
                }
                let mesh = self.clone();
                crate::spawn(async move {
                    use tokio::sync::broadcast::error::RecvError;
                    loop {
                        let msg = match rx.recv().await {
                            Ok(msg) => msg,
                            Err(RecvError::Lagged(_)) => continue,
                            Err(RecvError::Closed) => break,
                        };
                        match msg {
                            OutMsg::Data(bytes) => {
                                // Straight into the local viewer queue. A
                                // queue going empty → non-empty is the cue to
                                // poke the window, exactly as the inbound
                                // remote viewer path does.
                                if mesh.terminal.enqueue(&rid, bytes) {
                                    mesh.sink.emit("allmystuff://term-ready", json!(rid));
                                }
                            }
                            OutMsg::Resize { cols, rows } => {
                                // Two local windows sharing one shell: tell this
                                // window the shared size so it letterboxes to it.
                                mesh.sink.emit(
                                    "allmystuff://term-resize",
                                    json!({ "route": rid, "cols": cols, "rows": rows }),
                                );
                            }
                            OutMsg::Exit(code) => {
                                tracing::info!("local terminal {rid} — shell ended ({code:?})");
                                mesh.sink.emit(
                                    "allmystuff://term-exit",
                                    json!({ "route": rid, "code": code }),
                                );
                                let _ = mesh.disconnect(rid.clone()).await;
                                break;
                            }
                        }
                    }
                    // Pump ended — release the route so a fresh start can pump.
                    mesh.term_pumps.lock().remove(&rid);
                });
            }
            Err(e) => {
                // The shell never opened — release the route we just claimed.
                self.term_pumps.lock().remove(&rid);
                // Render the failure to the window in its own terms — a line
                // of text, then the exit — then tear the route down.
                tracing::warn!("route {rid} — local shell didn't start: {e}");
                let note = format!("[couldn't start a shell here: {e}]\r\n");
                if self.terminal.enqueue(&rid, note.into_bytes()) {
                    self.sink.emit("allmystuff://term-ready", json!(rid));
                }
                self.sink.emit(
                    "allmystuff://term-exit",
                    json!({ "route": rid, "code": serde_json::Value::Null }),
                );
                let mesh = self.clone();
                crate::spawn(async move {
                    let _ = mesh.disconnect(rid).await;
                });
            }
        }
    }

    /// Whether a terminal frame on `route` is fresh (record its seq and take
    /// it) or a duplicate to drop — used both for output the viewer takes
    /// (`term_rx_seq`) and input the host takes (`term_in_seq`). Each sending
    /// side numbers a route's frames strictly increasing, so any seq at or
    /// below the last we took is the same send arriving again over another
    /// shared network (control and media ride them all). A forward jump (the
    /// sender skipped ahead after a broadcast lag) is still fresh.
    fn accept_term_seq(seqs: &Mutex<HashMap<String, u64>>, route: &str, seq: u64) -> bool {
        let mut seqs = seqs.lock();
        match seqs.get(route) {
            Some(&last) if seq <= last => false,
            _ => {
                seqs.insert(route.to_string(), seq);
                true
            }
        }
    }

    /// One inbound terminal frame. Which side we are comes from the route
    /// itself: keystrokes/resizes landing on the *host* (the route sources
    /// here) clear the same two gates as input injection — live route from
    /// this exact sender, and the sender being an authorized controller;
    /// output/exit landing on the *viewer* (the route sinks here) goes to
    /// the watching terminal window.
    fn handle_term_frame(&self, from: &str, frame: TermFrame) {
        let Some(me) = self.local_node_id() else {
            return;
        };
        let (hosts_here, views_here) = {
            let st = self.state.lock();
            let Some(r) = st.session.as_ref().and_then(|s| s.route(&frame.route)) else {
                return;
            };
            if !(r.is_active()
                && is_terminal_route(&r.route)
                && pubkey_part(r.peer.as_str()) == pubkey_part(from))
            {
                tracing::debug!(
                    "terminal frame for {} refused (route not live here)",
                    frame.route
                );
                return;
            }
            (
                route_sources_on(&r.route, &me),
                route_sinks_on(&r.route, &me),
            )
        };
        if hosts_here {
            if !self.sender_may_drive(from, DrivePlane::Terminal) {
                tracing::warn!("dropped terminal input from {from}: not an authorized controller");
                return;
            }
            // Drop a duplicate keystroke/resize: the viewer numbers its
            // outbound frames strictly increasing, so a seq we've already
            // applied is the same send redelivered on another shared network.
            // Without this the PTY is written N times and the shell echoes
            // `aaaa` for one keypress.
            if !Self::accept_term_seq(&self.term_in_seq, &frame.route, frame.seq) {
                return;
            }
            match frame.event {
                TermEvent::Data { bytes } => {
                    let _ = self.terminal.write(&frame.route, bytes);
                }
                TermEvent::Resize { cols, rows } => {
                    let _ = self.terminal.resize(&frame.route, cols, rows);
                }
                // Ending the shell is the host's report, never the
                // viewer's request — a viewer ends a session by tearing
                // the route down.
                TermEvent::Exit { .. } => {}
                // A terminal event a newer viewer introduced — ignore it.
                TermEvent::Unknown => {}
            }
        } else if views_here {
            // Drop a duplicate delivery (see `accept_term_seq`): the same send
            // arriving again over another shared network. Without this the
            // window paints every byte — and the shell appears to echo every
            // keystroke — once per shared network: the doubled/tripled terminal.
            if !Self::accept_term_seq(&self.term_rx_seq, &frame.route, frame.seq) {
                return;
            }
            match frame.event {
                TermEvent::Data { bytes } => {
                    if self.terminal.enqueue(&frame.route, bytes) {
                        // Queue went empty → non-empty: poke the window to
                        // drain (a lost poke costs latency, never bytes —
                        // the safety poll catches up).
                        self.sink
                            .emit("allmystuff://term-ready", json!(frame.route));
                    }
                }
                TermEvent::Exit { code } => {
                    self.sink.emit(
                        "allmystuff://term-exit",
                        json!({ "route": frame.route, "code": code }),
                    );
                }
                TermEvent::Resize { cols, rows } => {
                    // The host's authoritative shared size — the window renders
                    // (letterboxes) to it so its wrapping matches the one shell.
                    self.sink.emit(
                        "allmystuff://term-resize",
                        json!({ "route": frame.route, "cols": cols, "rows": rows }),
                    );
                }
                // A terminal event a newer host introduced — ignore it.
                TermEvent::Unknown => {}
            }
        }
    }

    /// Front-end command: keystrokes/resizes from a terminal window down
    /// its active terminal route. This machine must be the route's
    /// *viewer* (its sink side); `Exit` is the host's word and is refused.
    pub async fn term_send(
        self: &Arc<Self>,
        route_id: String,
        event: TermEvent,
    ) -> Result<(), String> {
        let me = self.local_node_id().ok_or("mesh not ready")?;
        let (peer, loopback) = {
            let st = self.state.lock();
            let r = st
                .session
                .as_ref()
                .and_then(|s| s.route(&route_id))
                .ok_or("unknown route")?;
            // Endpoint self-checks compare *canonically*: the UI builds the
            // route's host endpoint from the suffixed display id while `me` is
            // the bare node id, so a raw `==` misses a genuine self-route (see
            // `same_node`). This machine must be the route's viewer…
            if !(r.is_active()
                && is_terminal_route(&r.route)
                && same_node(&node_of(r.route.to.as_str()), &me))
            {
                return Err("route isn't an active terminal session here".into());
            }
            // …and a terminal whose *source* is this machine too has no peer to
            // frame to: the shell is hosted right here, so input/resize go
            // straight to the local PTY rather than out over the mesh. The raw
            // `==` this replaces left a loopback ConPTY blank on Windows — the
            // viewer's cursor-position reply (CSI 6 n) was framed to a
            // non-existent peer, and ConPTY withholds all output until that
            // reply lands.
            let loopback = same_node(&node_of(r.route.from.as_str()), &me);
            (r.peer.to_string(), loopback)
        };
        if loopback {
            match event {
                TermEvent::Data { bytes } => {
                    return self
                        .terminal
                        .write(&route_id, bytes)
                        .then_some(())
                        .ok_or_else(|| "local terminal PTY is no longer accepting input".into());
                }
                TermEvent::Resize { cols, rows } => {
                    return self
                        .terminal
                        .resize(&route_id, cols, rows)
                        .then_some(())
                        .ok_or_else(|| "local terminal PTY is no longer accepting resize".into());
                }
                TermEvent::Exit { .. } => {
                    return Err("exit is reported by the host, not sent".into())
                }
                TermEvent::Unknown => return Err("unknown terminal event".into()),
            }
        }
        match event {
            TermEvent::Data { bytes } => {
                // A paste can be arbitrarily large: chunk to the channel
                // budget and await each send, so big pastes throttle
                // themselves instead of flooding the daemon.
                let frames = TermFrame::data_frames(&route_id, 0, &bytes, MAX_TERM_DATA_BYTES);
                let first = self
                    .term_seq
                    .fetch_add(frames.len() as u64, Ordering::Relaxed);
                for (i, mut frame) in frames.into_iter().enumerate() {
                    frame.seq = first + i as u64;
                    let payload = serde_json::to_value(&frame).map_err(|e| e.to_string())?;
                    self.send_media_value(&peer, payload).await?;
                }
                Ok(())
            }
            TermEvent::Resize { .. } => {
                let seq = self.term_seq.fetch_add(1, Ordering::Relaxed);
                let frame = TermFrame::new(&route_id, seq, event);
                let payload = serde_json::to_value(&frame).map_err(|e| e.to_string())?;
                self.send_media_value(&peer, payload).await
            }
            TermEvent::Exit { .. } => Err("exit is reported by the host, not sent".into()),
            // We never originate an `Unknown` event; reject it for exhaustiveness.
            TermEvent::Unknown => Err("unknown terminal event".into()),
        }
    }

    /// A terminal window claims an active route's buffered output (returns
    /// the token scoping its unwatch). Pure plumbing to [`TerminalHost`].
    pub fn term_watch(&self, route_id: &str) -> u64 {
        self.terminal.watch_output(route_id)
    }

    pub fn term_unwatch(&self, route_id: &str, token: u64) {
        self.terminal.unwatch(route_id, token);
    }

    /// Drain buffered terminal output (`[u32 le len][bytes]…`), emptied by
    /// the window on each `allmystuff://term-ready` poke or safety poll.
    pub fn term_poll(&self, route_id: &str) -> Vec<u8> {
        self.terminal.poll(route_id)
    }

    /// Front-end command: ask `node` for its open terminal sessions so the
    /// picker can offer to *attach* to one (multi-attach) instead of always
    /// minting a new shell. For a remote machine this fires a
    /// [`RouteControl::TerminalSessionsRequest`]; the host's answer arrives
    /// asynchronously as an `allmystuff://terminal-sessions` event. For the
    /// **local** machine there's no peer to ask — we answer at once from our
    /// own [`TerminalHost`], returning the list directly (and `None` for a
    /// remote ask, whose reply rides the event). Gated owner/fleet exactly
    /// like opening a terminal — the host re-checks it too.
    pub async fn request_terminal_sessions(
        self: &Arc<Self>,
        node: String,
    ) -> Result<Option<Vec<TerminalSessionInfo>>, String> {
        let me = self.local_node_id().ok_or("mesh not ready")?;
        if pubkey_part(&node) == pubkey_part(&me) {
            // Our own shells — answer straight from the local host.
            return Ok(Some(self.terminal_session_infos()));
        }
        self.send_control(
            &node,
            &ControlMessage::Route(RouteControl::TerminalSessionsRequest),
        )
        .await?;
        Ok(None)
    }

    /// The local terminal host's open sessions in the protocol's wire shape.
    fn terminal_session_infos(&self) -> Vec<TerminalSessionInfo> {
        self.terminal
            .list_sessions()
            .into_iter()
            .map(|s| TerminalSessionInfo {
                session_id: s.session_id,
                title: s.title,
                created_unix: s.created_unix,
                attachers: s.attachers,
            })
            .collect()
    }

    /// Answer a viewer's [`RouteControl::TerminalSessionsRequest`]: reply on
    /// the control channel with this host's open terminal sessions — gated by
    /// the same owner/fleet check the terminal host itself uses, so a
    /// stranger on the mesh can't even enumerate our shells.
    async fn handle_terminal_sessions_request(self: &Arc<Self>, from: &str) {
        if !self.sender_may_drive(from, DrivePlane::Terminal) {
            tracing::warn!(
                "terminal-sessions request from {} ignored: not owner/fleet",
                short_id(from)
            );
            return;
        }
        let sessions = self.terminal_session_infos();
        let _ = self
            .send_control(
                from,
                &ControlMessage::Route(RouteControl::TerminalSessions { sessions }),
            )
            .await;
    }
