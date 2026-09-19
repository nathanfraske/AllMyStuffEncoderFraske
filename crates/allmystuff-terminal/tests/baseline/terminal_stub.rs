//! Host-half stub of [`crate::terminal`] for capture-less builds
//! (`--no-default-features`, i.e. iOS — see the `host` feature in
//! `Cargo.toml`).
//!
//! A PTY needs fork/exec, which the iOS sandbox forbids — so *hosting* a
//! terminal fails cleanly ([`TerminalHost::open`] returns an error the mesh
//! already forwards to the requesting viewer). The **viewer half is real**:
//! the per-route output queues ([`crate::byte_queues`], pure std) buffer
//! remote shells' output for this node's own terminal windows exactly as on
//! desktop — attaching to another machine's terminal works; offering one
//! doesn't.

use crate::byte_queues::ByteQueues;

/// What a hosted PTY produces for the mesh pump — kept identical so the
/// viewer-side pump code and tests compile unchanged.
#[derive(Debug, Clone)]
pub enum OutMsg {
    /// A chunk of PTY output.
    Data(Vec<u8>),
    /// The shared PTY's reconciled size changed.
    Resize { cols: u16, rows: u16 },
    /// The shell ended (`None` = killed by signal / no status).
    Exit(Option<i32>),
}

/// The handle [`TerminalHost::open`] would hand back. Constructed by no one
/// on this build (open always fails), but destructured by the mesh.
pub struct TermAttach {
    pub session_id: String,
    /// Broadcast before this attach subscribed — replayed first for a
    /// gapless screen.
    pub scrollback: Vec<u8>,
    pub rx: tokio::sync::broadcast::Receiver<OutMsg>,
    /// `true` when this call created the session.
    pub created: bool,
}

/// A row in [`TerminalHost::list_sessions`] — always empty here.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub title: String,
    pub created_unix: u64,
    pub attachers: usize,
}

/// Same cap as the real host's viewer buffer.
const MAX_QUEUED_BYTES: usize = 4 * 1024 * 1024;

/// The viewer half of the desktop `TerminalHost`, with the PTY half
/// answering "not on this device".
pub struct TerminalHost {
    /// Viewer-side buffers of *remote* PTY output per route, drained by the
    /// terminal window (the shared poke-then-pull queue plumbing).
    output: ByteQueues,
}

impl Default for TerminalHost {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalHost {
    pub fn new() -> Self {
        TerminalHost {
            output: ByteQueues::new(MAX_QUEUED_BYTES),
        }
    }

    // ---- host side: no PTY to give ------------------------------------

    /// Hosting always fails: there is no shell to spawn inside this app's
    /// sandbox. The error text rides the existing refusal path back to the
    /// viewer that asked.
    pub fn open(
        &self,
        _session_id: Option<&str>,
        _route_id: &str,
        _cols: u16,
        _rows: u16,
    ) -> Result<TermAttach, String> {
        Err("this device cannot host a terminal".into())
    }

    pub fn detach(&self, _route_id: &str) {}

    pub fn is_attached(&self, _route_id: &str) -> bool {
        false
    }

    pub fn close(&self, _session_id: &str) {}

    pub fn list_sessions(&self) -> Vec<SessionInfo> {
        Vec::new()
    }

    pub fn write(&self, _route_id: &str, _bytes: Vec<u8>) -> bool {
        false
    }

    pub fn resize(&self, _route_id: &str, _cols: u16, _rows: u16) -> bool {
        false
    }

    /// The legacy one-route-one-session bridge — same refusal as
    /// [`open`](Self::open).
    #[allow(dead_code)]
    pub fn spawn(&self, _route_id: &str) -> Result<tokio::sync::mpsc::Receiver<OutMsg>, String> {
        Err("this device cannot host a terminal".into())
    }

    /// Tear down whatever this route had here — on this build, only the
    /// viewer buffer.
    pub fn stop(&self, route_id: &str) {
        self.output.remove(route_id);
    }

    // ---- viewer side (real) --------------------------------------------

    pub fn ensure_queue(&self, route_id: &str) {
        self.output.ensure(route_id);
    }

    pub fn watch_output(&self, route_id: &str) -> u64 {
        self.output.watch(route_id)
    }

    pub fn unwatch(&self, route_id: &str, token: u64) {
        self.output.unwatch(route_id, token);
    }

    pub fn poll(&self, route_id: &str) -> Vec<u8> {
        self.output.poll(route_id)
    }

    /// Buffer one inbound output chunk for the watching window. Returns
    /// `true` when the queue went empty → non-empty — the caller's cue to
    /// poke the front-end.
    pub fn enqueue(&self, route_id: &str, bytes: Vec<u8>) -> bool {
        self.output.enqueue(route_id, bytes)
    }
}
