    /// Regression guard for the screen/audio outage: the engine fires tasks
    /// from capture/audio OS threads (e.g. the DXGI status callback), where a
    /// bare `tokio::spawn` panics with "no reactor running". Every engine spawn
    /// goes through [`crate::spawn`], which must work off-runtime via the handle
    /// `start` registers. Spawn from a plain `std::thread` (no ambient runtime)
    /// and confirm the task actually runs.
    #[test]
    fn engine_spawn_runs_tasks_from_a_non_runtime_thread() {
        let rt = tokio::runtime::Runtime::new().expect("build runtime");
        crate::set_runtime(rt.handle().clone());
        // Keep the runtime (and the registered handle) alive for the process —
        // OnceLock holds the handle, and this is the only test that sets it.
        std::mem::forget(rt);

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            // No ambient runtime here — `tokio::spawn` would panic.
            crate::spawn(async move {
                let _ = tx.send(());
            });
        })
        .join()
        .unwrap();

        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("spawned task should run on the registered runtime");
    }

    /// Two routes attaching to one terminal session — the multi-attach
    /// contract the mesh now drives — both see the shell's output, either can
    /// type into the one shell, and the host's session list reports them as a
    /// single shared session. This drives the same [`TerminalHost::open`] the
    /// `start_terminal_host` pump uses (without needing a live daemon), so it
    /// guards the mesh's view of sharing end to end.
    #[cfg(all(unix, feature = "host"))]
    #[test]
    fn two_routes_share_one_session_through_the_host() {
        use crate::terminal::OutMsg;
        use std::time::{Duration, Instant};

        // The mesh's idle reaper / spawns need a runtime registered.
        let rt = tokio::runtime::Runtime::new().expect("build runtime");
        crate::set_runtime(rt.handle().clone());
        std::mem::forget(rt);

        let client = Arc::new(ControlClient::new().expect("resolve control socket path"));
        let mesh = Mesh::new(client, Arc::new(NoopSink));

        // First route creates the session; second attaches to the same id —
        // exactly what an Offer carrying `session: Some(id)` resolves to.
        let a = mesh
            .terminal
            .open(Some("shared"), "routeA", 80, 24)
            .expect("create session");
        assert!(a.created, "first open creates the session");
        let b = mesh
            .terminal
            .open(Some("shared"), "routeB", 80, 24)
            .expect("attach to session");
        assert!(!b.created, "second open attaches to the shared session");

        // The host's picker list reports one shared session with two viewers.
        let infos = mesh.terminal_session_infos();
        let shared = infos
            .iter()
            .find(|s| s.session_id == "shared")
            .expect("session listed");
        assert_eq!(shared.attachers, 2, "both routes counted as attachers");

        // Either route can type into the one shell, and both pumps see it.
        let mut rxa = a.rx;
        let mut rxb = b.rx;
        assert!(mesh.terminal.write("routeB", b"echo via-B\n".to_vec()));

        let saw = |rx: &mut tokio::sync::broadcast::Receiver<OutMsg>, needle: &str| -> bool {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut seen = Vec::new();
            while Instant::now() < deadline {
                match rx.try_recv() {
                    Ok(OutMsg::Data(b)) => {
                        seen.extend_from_slice(&b);
                        if String::from_utf8_lossy(&seen).contains(needle) {
                            return true;
                        }
                    }
                    Ok(OutMsg::Resize { .. }) => {}
                    Ok(OutMsg::Exit(_)) => return false,
                    Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                        std::thread::sleep(Duration::from_millis(20))
                    }
                    Err(_) => return false,
                }
            }
            false
        };
        assert!(saw(&mut rxa, "via-B"), "route A sees route B's input");
        assert!(saw(&mut rxb, "via-B"), "route B sees its own echo");

        // Detaching one viewer keeps the shell alive for the other.
        mesh.terminal.detach("routeA");
        assert_eq!(
            mesh.terminal_session_infos()
                .iter()
                .find(|s| s.session_id == "shared")
                .map(|s| s.attachers),
            Some(1),
            "session survives one detach with the remaining attacher",
        );
        mesh.terminal.close("shared");
    }

    #[test]
    fn dedup_collapses_duplicate_terminal_frames_by_seq() {
        // The dedup that collapses a frame delivered on several shared
        // networks back to one (both directions): the sending side numbers a
        // route's frames strictly increasing, so a seq already taken is a
        // duplicate. A different route, and the other direction's map, each
        // keep an independent counter.
        let client = Arc::new(ControlClient::new().expect("resolve control socket path"));
        let mesh = Mesh::new(client, Arc::new(NoopSink));
        let out = &mesh.term_rx_seq;
        let inp = &mesh.term_in_seq;

        assert!(Mesh::accept_term_seq(out, "r", 0), "first frame is fresh");
        assert!(
            !Mesh::accept_term_seq(out, "r", 0),
            "same seq again is a duplicate"
        );
        assert!(Mesh::accept_term_seq(out, "r", 1), "the next seq is fresh");
        assert!(
            !Mesh::accept_term_seq(out, "r", 1),
            "and its duplicate drops"
        );
        assert!(
            !Mesh::accept_term_seq(out, "r", 0),
            "an older straggler drops too"
        );
        assert!(Mesh::accept_term_seq(out, "r", 2), "advancing is fresh");
        assert!(
            Mesh::accept_term_seq(out, "r", 9),
            "a forward jump (sender skipped after a lag) is still fresh"
        );
        assert!(
            Mesh::accept_term_seq(out, "other", 0),
            "a different route has its own independent counter"
        );
        // The input map (host taking keystrokes) is wholly independent of the
        // output map — the same route+seq is fresh again here.
        assert!(
            Mesh::accept_term_seq(inp, "r", 0),
            "input dedup is independent of output dedup"
        );
        assert!(
            !Mesh::accept_term_seq(inp, "r", 0),
            "but still drops its own duplicates"
        );
    }

    #[test]
    fn terminal_routes_are_recognized_by_shape() {
        // Generic media + a `…:terminal` source = a terminal session.
        let term = term_route("host:terminal", "me:term-view:1", MediaKind::Generic);
        assert!(is_terminal_route(&term));

        // Generic data that isn't a terminal stays untouched (the escape
        // hatch keeps working for whatever apps wire through it)…
        let generic = term_route("host:thing", "me:other", MediaKind::Generic);
        assert!(!is_terminal_route(&generic));

        // …and a `:terminal` id under any *other* media is not a terminal
        // (the media is part of the contract, not just the suffix).
        let display = term_route("host:terminal", "me:term-view:1", MediaKind::Display);
        assert!(!is_terminal_route(&display));
    }
