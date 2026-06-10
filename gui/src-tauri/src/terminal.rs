//! Mesh-native terminal sessions — the backend of "Open Terminal".
//!
//! Two halves, one struct:
//!
//!  * **Host** (the machine whose shell runs): [`TerminalHost::spawn`]
//!    opens a real PTY (openpty on Unix, ConPTY on Windows — `portable-pty`
//!    picks at runtime) and runs this user's shell in it. Three small
//!    blocking threads own the blocking ends — reader, control (writer +
//!    resize + kill), and wait (authoritative exit) — and meet the async
//!    world over bounded channels, so PTY backpressure propagates all the
//!    way to the shell instead of ballooning memory.
//!  * **Viewer** (the machine looking at it): inbound output is buffered
//!    per route and pulled by the terminal window with the same
//!    poke-then-pull watcher pattern the video plane uses — a lost "ready"
//!    poke costs latency, never bytes.
//!
//! No sshd, no credentials: the mesh already proved who the peer is, and
//! the caller gates everything on the owner/fleet rule before any of this
//! runs.

use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, PtySize};

/// What a hosted PTY produces for the mesh pump.
#[derive(Debug)]
pub enum OutMsg {
    /// A chunk of PTY output (≤ [`READ_BUF`] bytes).
    Data(Vec<u8>),
    /// The shell ended (`None` = killed by signal / no status).
    Exit(Option<i32>),
}

/// What the mesh feeds a hosted PTY.
enum CtlMsg {
    Data(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    Shutdown,
}

/// One PTY read at a time — small enough that a slow viewer throttles the
/// shell quickly, big enough that `cat bigfile` isn't syscall-bound.
const READ_BUF: usize = 8 * 1024;
/// Output chunks in flight host-side before the reader thread blocks
/// (which in turn fills the kernel PTY buffer and blocks the shell —
/// end-to-end flow control, the same shape ssh gives you).
const OUT_QUEUE: usize = 64;
/// Keystrokes/resizes queued before writes are dropped (a shell wedged in
/// flow-stop shouldn't stall the shared mesh loop).
const CTL_QUEUE: usize = 256;
/// A viewer window that never drains caps its buffer here; beyond it the
/// oldest chunks go (the terminal is live media, not a transcript).
const MAX_QUEUED_BYTES: usize = 4 * 1024 * 1024;

struct PtySession {
    ctl_tx: std::sync::mpsc::SyncSender<CtlMsg>,
    /// Kills the child directly even when the control thread is busy
    /// mid-write — `stop` must never wait on a wedged shell.
    killer: Box<dyn ChildKiller + Send + Sync>,
}

/// Viewer-side buffer of PTY output for one route, drained by the
/// terminal window. Mirrors the video plane's `VideoWatcher`: the `token`
/// scopes `unwatch` to the claim that made it, so a late unwatch can't
/// tear down a newer watcher. Token `0` is the *eager* queue `start_media`
/// creates the moment the route goes active — the shell's first prompt
/// arrives before the window has subscribed, and unlike a video frame a
/// dropped byte never heals.
struct TermWatcher {
    token: u64,
    queue: VecDeque<Vec<u8>>,
    queued_bytes: usize,
}

#[derive(Default)]
pub struct TerminalHost {
    sessions: Mutex<HashMap<String, PtySession>>,
    watchers: Mutex<HashMap<String, TermWatcher>>,
    watch_tokens: AtomicU64,
}

impl TerminalHost {
    pub fn new() -> Self {
        TerminalHost {
            sessions: Mutex::new(HashMap::new()),
            watchers: Mutex::new(HashMap::new()),
            // 0 is the eager queue's reserved token.
            watch_tokens: AtomicU64::new(1),
        }
    }

    // ---- host side ----------------------------------------------------

    /// Spawn this user's shell in a fresh PTY for `route_id`. Returns the
    /// output stream the caller pumps to the viewer; the session ends with
    /// exactly one [`OutMsg::Exit`] (unless [`stop`](Self::stop) cut it
    /// short, which closes the stream instead).
    pub fn spawn(&self, route_id: &str) -> Result<tokio::sync::mpsc::Receiver<OutMsg>, String> {
        self.spawn_with(route_id, default_shell_commands())
    }

    /// [`spawn`](Self::spawn) with explicit command candidates (first that
    /// spawns wins) — the test seam, and how the per-OS shell fallbacks
    /// (`$SHELL -l` → `$SHELL` → `/bin/sh`; `pwsh` → `powershell` → `cmd`)
    /// are expressed.
    fn spawn_with(
        &self,
        route_id: &str,
        candidates: Vec<CommandBuilder>,
    ) -> Result<tokio::sync::mpsc::Receiver<OutMsg>, String> {
        if self.sessions.lock().contains_key(route_id) {
            return Err("terminal already running for this route".into());
        }
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("openpty failed: {e:#}"))?;

        let mut child = None;
        let mut last_err = String::from("no shell candidates");
        for cmd in candidates {
            let label = format!("{:?}", cmd.get_argv());
            match pair.slave.spawn_command(cmd) {
                Ok(c) => {
                    child = Some(c);
                    break;
                }
                Err(e) => {
                    tracing::debug!("terminal shell candidate {label} failed: {e:#}");
                    last_err = format!("{e:#}");
                }
            }
        }
        let mut child = child.ok_or_else(|| format!("couldn't start a shell: {last_err}"))?;
        // The slave end lives on inside the child; holding ours open would
        // keep the reader from ever seeing EOF.
        drop(pair.slave);

        let master = pair.master;
        let mut reader = master
            .try_clone_reader()
            .map_err(|e| format!("pty reader: {e:#}"))?;
        let mut writer = master
            .take_writer()
            .map_err(|e| format!("pty writer: {e:#}"))?;
        let killer = child.clone_killer();
        let mut ctl_killer = child.clone_killer();

        let (out_tx, out_rx) = tokio::sync::mpsc::channel::<OutMsg>(OUT_QUEUE);
        let (ctl_tx, ctl_rx) = std::sync::mpsc::sync_channel::<CtlMsg>(CTL_QUEUE);

        // Reader: PTY output → bounded channel. A full channel blocks here,
        // the kernel PTY buffer fills, the shell's writes block — flow
        // control without a byte counter in sight.
        let rid = route_id.to_string();
        let reader_tx = out_tx.clone();
        spawn_named(&format!("amst-term-read {rid}"), move || {
            let mut buf = [0u8; READ_BUF];
            loop {
                match reader.read(&mut buf) {
                    // EOF — on Unix when the shell exits; on Windows often
                    // only once the master drops (the control thread's job).
                    Ok(0) => break,
                    Ok(n) => {
                        if reader_tx
                            .blocking_send(OutMsg::Data(buf[..n].to_vec()))
                            .is_err()
                        {
                            break; // pump gone — session is over
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        // Control: keystrokes + resizes + shutdown. Owns the writer *and
        // the master* — dropping the master on the way out is what
        // unblocks a ConPTY reader that never EOFs.
        let rid = route_id.to_string();
        spawn_named(&format!("amst-term-ctl {rid}"), move || {
            let _master = master;
            while let Ok(msg) = ctl_rx.recv() {
                match msg {
                    CtlMsg::Data(bytes) => {
                        use std::io::Write as _;
                        if writer
                            .write_all(&bytes)
                            .and_then(|()| writer.flush())
                            .is_err()
                        {
                            // Writer dead = shell gone; the wait thread
                            // reports it. Stop accepting input.
                            break;
                        }
                    }
                    CtlMsg::Resize { cols, rows } => {
                        let _ = _master.resize(PtySize {
                            rows,
                            cols,
                            pixel_width: 0,
                            pixel_height: 0,
                        });
                    }
                    CtlMsg::Shutdown => {
                        let _ = ctl_killer.kill();
                        break;
                    }
                }
            }
        });

        // Wait: the authoritative end of the session on every OS. The
        // brief linger lets the reader drain the PTY's final bytes so the
        // exit notice lands *after* the shell's goodbye, not racing it.
        let rid = route_id.to_string();
        spawn_named(&format!("amst-term-wait {rid}"), move || {
            let code = match child.wait() {
                Ok(status) => {
                    if status.signal().is_some() {
                        None
                    } else {
                        Some(status.exit_code() as i32)
                    }
                }
                Err(_) => None,
            };
            std::thread::sleep(std::time::Duration::from_millis(120));
            let _ = out_tx.blocking_send(OutMsg::Exit(code));
        });

        self.sessions
            .lock()
            .insert(route_id.to_string(), PtySession { ctl_tx, killer });
        Ok(out_rx)
    }

    /// Feed viewer keystrokes to the hosted PTY. `false` = no such
    /// session, or its input queue is full (a flow-stopped shell — the
    /// bytes are dropped rather than stalling the mesh loop).
    pub fn write(&self, route_id: &str, bytes: Vec<u8>) -> bool {
        self.ctl_send(route_id, CtlMsg::Data(bytes))
    }

    /// Resize the hosted PTY to the viewer's emulator dimensions.
    pub fn resize(&self, route_id: &str, cols: u16, rows: u16) -> bool {
        self.ctl_send(route_id, CtlMsg::Resize { cols, rows })
    }

    fn ctl_send(&self, route_id: &str, msg: CtlMsg) -> bool {
        let sessions = self.sessions.lock();
        let Some(s) = sessions.get(route_id) else {
            return false;
        };
        match s.ctl_tx.try_send(msg) {
            Ok(()) => true,
            Err(_) => {
                tracing::warn!("terminal {route_id}: input dropped (shell not draining)");
                false
            }
        }
    }

    /// Tear down whatever this route had here — the hosted PTY (killed,
    /// promptly) and/or the viewer buffer. Idempotent; safe on either side.
    pub fn stop(&self, route_id: &str) {
        let session = self.sessions.lock().remove(route_id);
        if let Some(mut s) = session {
            // Direct kill first — the control thread may be wedged on a
            // write. Shutdown then unblocks/ends it, and dropping ctl_tx
            // closes the channel for good measure.
            let _ = s.killer.kill();
            let _ = s.ctl_tx.try_send(CtlMsg::Shutdown);
        }
        self.watchers.lock().remove(route_id);
    }

    // ---- viewer side ----------------------------------------------------

    /// Make sure an output buffer exists for `route_id` *before* the
    /// window subscribes — called when the route goes active, so the
    /// host's first prompt bytes (which arrive immediately) are kept, not
    /// dropped. Token 0 marks it adoptable.
    pub fn ensure_queue(&self, route_id: &str) {
        self.watchers
            .lock()
            .entry(route_id.to_string())
            .or_insert(TermWatcher {
                token: 0,
                queue: VecDeque::new(),
                queued_bytes: 0,
            });
    }

    /// The terminal window claims `route_id`'s output. Adopts the eager
    /// queue (keeping anything buffered) or replaces a previous watcher;
    /// returns the token that scopes the matching `unwatch`.
    pub fn watch_output(&self, route_id: &str) -> u64 {
        let token = self.watch_tokens.fetch_add(1, Ordering::Relaxed);
        let mut map = self.watchers.lock();
        let w = map.entry(route_id.to_string()).or_insert(TermWatcher {
            token: 0,
            queue: VecDeque::new(),
            queued_bytes: 0,
        });
        w.token = token;
        token
    }

    /// Release a watch claim. The token scopes it: a late unwatch from a
    /// closed tab can't remove the queue a newer watcher owns. Idempotent.
    pub fn unwatch(&self, route_id: &str, token: u64) {
        let mut map = self.watchers.lock();
        if map.get(route_id).is_some_and(|w| w.token == token) {
            map.remove(route_id);
        }
    }

    /// Drain everything queued for `route_id` into one length-prefixed
    /// buffer (`[u32 le len][bytes]…`) for a single IPC hop. Empty when
    /// there's nothing (or no such watcher).
    pub fn poll(&self, route_id: &str) -> Vec<u8> {
        let mut map = self.watchers.lock();
        let Some(w) = map.get_mut(route_id) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(w.queued_bytes + 4 * w.queue.len());
        for chunk in w.queue.drain(..) {
            out.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
            out.extend_from_slice(&chunk);
        }
        w.queued_bytes = 0;
        out
    }

    /// Buffer one inbound output chunk for the watching window. Returns
    /// `true` when the queue went empty → non-empty — the caller's cue to
    /// poke the front-end (mirroring `allmystuff://video-ready`).
    pub fn enqueue(&self, route_id: &str, bytes: Vec<u8>) -> bool {
        let mut map = self.watchers.lock();
        let Some(w) = map.get_mut(route_id) else {
            tracing::debug!("no terminal watcher for {route_id} — bytes dropped");
            return false;
        };
        let was_empty = w.queue.is_empty();
        w.queued_bytes += bytes.len();
        w.queue.push_back(bytes);
        while w.queued_bytes > MAX_QUEUED_BYTES {
            let Some(old) = w.queue.pop_front() else {
                break;
            };
            w.queued_bytes -= old.len();
            tracing::warn!("terminal queue for {route_id} unread — oldest chunk dropped");
        }
        was_empty
    }
}

/// The per-OS shell candidates, best first. Each is a full
/// [`CommandBuilder`] (program + args + env + cwd) ready to spawn.
fn default_shell_commands() -> Vec<CommandBuilder> {
    let mut out = Vec::new();
    #[cfg(windows)]
    {
        // PowerShell when available (pwsh = PowerShell 7+), classic
        // Windows PowerShell next, cmd.exe as the floor.
        for prog in ["pwsh.exe", "powershell.exe"] {
            let mut cmd = CommandBuilder::new(prog);
            cmd.arg("-NoLogo");
            out.push(dressed(cmd));
        }
        out.push(dressed(CommandBuilder::new(
            CommandBuilder::new_default_prog().get_shell(),
        )));
    }
    #[cfg(unix)]
    {
        // The user's own shell ($SHELL, else the password db) as a login
        // shell; the same shell plain for the rare one that rejects `-l`;
        // /bin/sh as the floor.
        let shell = CommandBuilder::new_default_prog().get_shell();
        let mut login = CommandBuilder::new(&shell);
        login.arg("-l");
        out.push(dressed(login));
        out.push(dressed(CommandBuilder::new(&shell)));
        if shell != "/bin/sh" {
            out.push(dressed(CommandBuilder::new("/bin/sh")));
        }
    }
    out
}

/// Home cwd + the terminal identity every spawn gets.
fn dressed(mut cmd: CommandBuilder) -> CommandBuilder {
    if let Some(home) = dirs::home_dir() {
        cmd.cwd(home);
    }
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd
}

fn spawn_named(name: &str, f: impl FnOnce() + Send + 'static) {
    let _ = std::thread::Builder::new().name(name.to_string()).spawn(f);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Collect Data until `pred` matches the transcript (or panic at the
    /// deadline). Returns the transcript so far.
    #[cfg(unix)]
    fn read_until(
        rx: &mut tokio::sync::mpsc::Receiver<OutMsg>,
        pred: impl Fn(&[u8]) -> bool,
        what: &str,
    ) -> Vec<u8> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            match rx.try_recv() {
                Ok(OutMsg::Data(b)) => {
                    seen.extend_from_slice(&b);
                    if pred(&seen) {
                        return seen;
                    }
                }
                Ok(OutMsg::Exit(_)) => break,
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        panic!(
            "didn't see {what} in time; transcript: {:?}",
            String::from_utf8_lossy(&seen)
        );
    }

    #[cfg(unix)]
    fn wait_exit(rx: &mut tokio::sync::mpsc::Receiver<OutMsg>) -> Option<i32> {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            match rx.try_recv() {
                Ok(OutMsg::Exit(code)) => return code,
                Ok(OutMsg::Data(_)) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        panic!("no exit in time");
    }

    #[cfg(unix)]
    fn sh(script: &str) -> Vec<CommandBuilder> {
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg(script);
        vec![cmd]
    }

    #[cfg(unix)]
    #[test]
    fn spawn_echo_roundtrip() {
        let host = TerminalHost::new();
        let mut rx = host.spawn_with("r1", sh("cat")).unwrap();
        assert!(host.write("r1", b"hello\n".to_vec()));
        // `cat` echoes back (and the PTY echoes the typing) — either way
        // the bytes round-tripped through a real PTY.
        read_until(
            &mut rx,
            |b| String::from_utf8_lossy(b).contains("hello"),
            "echo",
        );
        host.stop("r1");
        assert!(
            !host.write("r1", b"x".to_vec()),
            "stopped session takes no input"
        );
    }

    #[cfg(unix)]
    #[test]
    fn resize_reaches_the_pty() {
        let host = TerminalHost::new();
        // The shell waits for a newline, so the resize lands first.
        let mut rx = host.spawn_with("r2", sh("read line; stty size")).unwrap();
        assert!(host.resize("r2", 100, 40));
        std::thread::sleep(Duration::from_millis(150));
        assert!(host.write("r2", b"\n".to_vec()));
        read_until(
            &mut rx,
            |b| String::from_utf8_lossy(b).contains("40 100"),
            "stty size 40 100",
        );
        host.stop("r2");
    }

    #[cfg(unix)]
    #[test]
    fn exit_code_surfaces() {
        let host = TerminalHost::new();
        let mut rx = host.spawn_with("r3", sh("exit 3")).unwrap();
        assert_eq!(wait_exit(&mut rx), Some(3));
    }

    #[cfg(unix)]
    #[test]
    fn stop_kills_promptly() {
        let host = TerminalHost::new();
        let mut rx = host.spawn_with("r4", sh("sleep 30")).unwrap();
        std::thread::sleep(Duration::from_millis(150));
        host.stop("r4");
        // Killed → wait-thread reports a signal death (no code) quickly,
        // not after 30s. (rx still drains: stop doesn't close the pump.)
        assert_eq!(wait_exit(&mut rx), None);
    }

    /// How many CSI 6 n (cursor-position) probes the transcript holds —
    /// what a ConPTY-backed shell sends its terminal before painting.
    fn dsr_probes(hay: &[u8]) -> usize {
        const QUERY: &[u8] = b"\x1b[6n";
        hay.windows(QUERY.len()).filter(|w| *w == QUERY).count()
    }

    #[test]
    fn dsr_probe_counting_finds_each_query() {
        assert_eq!(dsr_probes(b""), 0);
        assert_eq!(dsr_probes(b"plain output"), 0);
        assert_eq!(dsr_probes(b"\x1b[6n"), 1);
        assert_eq!(dsr_probes(b"a\x1b[6nb\x1b[6nc"), 2);
        // A truncated probe (chunk boundary) doesn't count until the rest
        // arrives — the caller re-counts over the whole transcript.
        assert_eq!(dsr_probes(b"\x1b["), 0);
    }

    #[cfg(windows)]
    #[test]
    fn spawn_cmd_echo_and_exit() {
        let mut cmd = CommandBuilder::new("cmd.exe");
        cmd.arg("/C");
        cmd.arg("echo hello-from-conpty");
        let host = TerminalHost::new();
        let mut rx = host.spawn_with("rw", vec![cmd]).unwrap();

        // ConPTY probes its "terminal" with CSI 6 n (report cursor
        // position) and holds output until something answers — in the
        // real app xterm.js answers and the reply rides the route back.
        // The test plays the emulator: answer every probe it sees.
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut seen: Vec<u8> = Vec::new();
        let mut answered = 0usize;
        let mut exit: Option<Option<i32>> = None;
        while Instant::now() < deadline {
            match rx.try_recv() {
                Ok(OutMsg::Data(b)) => {
                    seen.extend_from_slice(&b);
                    while answered < dsr_probes(&seen) {
                        assert!(host.write("rw", b"\x1b[1;1R".to_vec()));
                        answered += 1;
                    }
                }
                Ok(OutMsg::Exit(code)) => {
                    exit = Some(code);
                    break;
                }
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        // The exit linger orders Data before Exit, but drain any tail
        // that was already queued before asserting on the transcript.
        while let Ok(OutMsg::Data(b)) = rx.try_recv() {
            seen.extend_from_slice(&b);
        }
        let transcript = String::from_utf8_lossy(&seen);
        assert!(
            transcript.contains("hello-from-conpty"),
            "transcript: {transcript:?}"
        );
        assert_eq!(exit, Some(Some(0)));
    }

    #[test]
    fn double_spawn_is_refused() {
        let host = TerminalHost::new();
        host.sessions.lock().insert(
            "dup".into(),
            // A fake session is enough — spawn must bail before any PTY.
            {
                let (tx, _rx) = std::sync::mpsc::sync_channel(1);
                PtySession {
                    ctl_tx: tx,
                    killer: Box::new(NoopKiller),
                }
            },
        );
        assert!(host.spawn("dup").is_err());
    }

    #[derive(Debug)]
    struct NoopKiller;
    impl ChildKiller for NoopKiller {
        fn kill(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(NoopKiller)
        }
    }

    #[test]
    fn watcher_framing_is_byte_exact() {
        let host = TerminalHost::new();
        host.ensure_queue("w1");
        assert!(host.enqueue("w1", vec![1, 2, 3]), "empty → non-empty pokes");
        assert!(!host.enqueue("w1", vec![4]), "already non-empty: no poke");
        let buf = host.poll("w1");
        assert_eq!(
            buf,
            vec![3, 0, 0, 0, 1, 2, 3, 1, 0, 0, 0, 4],
            "[u32 le len][bytes] per chunk"
        );
        assert!(host.poll("w1").is_empty(), "drained");
        assert!(host.enqueue("w1", vec![9]), "poke again after a drain");
    }

    #[test]
    fn watch_adopts_the_eager_queue_and_scopes_unwatch() {
        let host = TerminalHost::new();
        host.ensure_queue("w2");
        host.enqueue("w2", b"early prompt".to_vec());
        let token = host.watch_output("w2");
        assert_eq!(host.poll("w2")[4..], b"early prompt"[..], "buffer kept");

        // A stale token can't remove the live watcher…
        host.unwatch("w2", token + 999);
        assert!(host.enqueue("w2", vec![1]));
        // …the right one can.
        host.unwatch("w2", token);
        assert!(!host.enqueue("w2", vec![2]), "no watcher — dropped");
    }

    #[test]
    fn overflow_drops_oldest_not_newest() {
        let host = TerminalHost::new();
        host.ensure_queue("w3");
        let chunk = vec![0u8; 1024 * 1024];
        for _ in 0..4 {
            host.enqueue("w3", chunk.clone());
        }
        host.enqueue("w3", b"newest".to_vec());
        let buf = host.poll("w3");
        let tail = &buf[buf.len() - 6..];
        assert_eq!(tail, b"newest");
        assert!(buf.len() <= MAX_QUEUED_BYTES + 6 + 5 * 4);
    }
}
