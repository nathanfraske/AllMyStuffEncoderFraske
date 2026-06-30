//! AllMyStuff's headless mesh **node engine**.
//!
//! This crate is the machinery that used to live inside the desktop GUI's
//! Tauri backend (`gui/src-tauri/src`). It is a *client* of a `myownmesh
//! serve` daemon over the control socket and, on top of that link, runs:
//!
//!  * **presence** — broadcasts this machine's [`NodeProfile`] and tracks
//!    peers (the graph fills with real devices);
//!  * **the route handshake** — the [`allmystuff_session::Session`] state
//!    machine, auto-accepting authorized offers (owner/fleet gated);
//!  * **every media plane** — screen + camera ([`video`], the capture
//!    sessions in [`win_capture`] / [`wayland_capture`] / [`camera_capture`]),
//!    audio ([`audio`]), input injection ([`input_inject`]), the mesh-native
//!    terminal ([`terminal`]) and file manager ([`files`]), clipboard
//!    ([`clipboard`]), and exposed-service tunnels ([`sites`]);
//!  * **ownership + the owned fleet** ([`ownership`]).
//!
//! The one thing it does *not* own is where node events surface. That seam is
//! the [`UiSink`] trait: [`mesh::Mesh::new`] takes one and the engine emits
//! through it, knowing nothing about who (if anyone) listens.
//!
//! There is **one node per machine**, and it runs in the `allmystuff-serve`
//! binary. That binary wires in a [`node_control::SocketSink`], which logs each
//! event *and* fans it out to every client connected to the node's own control
//! socket ([`node_control`]). The desktop GUI is a thin client of that socket
//! ([`node_control::NodeClient`]): it drives the node with one request per
//! command and re-emits the streamed events onto Tauri's bus, rather than
//! linking the engine and running a second `Mesh` itself. (It used to do the
//! latter, with a Tauri-backed sink — but that put two nodes under one identity
//! on an Always-On machine, and then nothing could connect.)

pub mod audio;
pub mod byte_queues;
pub mod camera_capture;
pub mod clipboard;
pub mod control_client;
pub mod daemon_spawn;
pub mod files;
/// Hardware H.264 encode via FFmpeg vendor encoders. Only built with the
/// `hwenc` feature (which pulls FFmpeg); the encoder ladder in [`video`] skips
/// it otherwise and runs software openh264.
#[cfg(feature = "hwenc")]
pub mod hwenc;
pub mod input_inject;
/// Hardware H.264 encode via Media Foundation — the GPU's own H.264 MFT
/// (NVENC/QuickSync/AMD) on Windows, with no FFmpeg toolchain. Windows-only;
/// the encoder ladder in [`video`] enumerates and frame-send-tests it, falling
/// to software openh264 when no hardware MFT produces frames.
#[cfg(windows)]
pub mod mediafoundation;
pub mod mesh;
pub mod networks_store;
pub mod node_control;
pub mod ownership;
pub mod shares;
pub mod sites;
pub mod terminal;
pub mod video;
pub mod video_decode;
pub mod wake;
// Windows screen capture (in-house DXGI). Declared on every target — the
// module is internally `cfg`-gated to a stub off Windows, exactly as it was
// when it lived in the GUI binary.
pub mod win_capture;
// Wayland screen capture via the ScreenCast portal — Linux only.
#[cfg(target_os = "linux")]
pub mod wayland_capture;

/// Where the node surfaces events. Every variant the engine emits
/// (`allmystuff://session`, `…/video-ready`, `…/term-exit`, …) is a
/// front-end concern, so a headless node can drop them all; only the GUI's
/// webview actually listens.
///
/// The GUI implements this over Tauri's [`Emitter`](https://docs.rs/tauri)
/// (`app.emit`); the `allmystuff-serve` binary implements it with a logging
/// sink. Keeping it a trait object (`Arc<dyn UiSink>`) means the engine
/// links no webview and the same [`mesh::Mesh`] runs in both worlds.
pub trait UiSink: Send + Sync + 'static {
    /// Deliver one event + JSON payload to whatever front-end is attached
    /// (or nowhere). Must never block the caller — the GUI's `app.emit` is
    /// fire-and-forget, and headless sinks should be too.
    fn emit(&self, event: &str, payload: serde_json::Value);

    /// Relaunch the host process onto a freshly-applied self-update (the
    /// fleet "upgrade this machine" path). The GUI restarts its webview app;
    /// the headless node re-execs itself. Never returns.
    fn restart(&self) -> !;
}

// ---------------------------------------------------------------------------
// Runtime registry
// ---------------------------------------------------------------------------

/// The Tokio runtime the engine spawns onto, registered once at startup.
static RUNTIME: std::sync::OnceLock<tokio::runtime::Handle> = std::sync::OnceLock::new();

/// Register the runtime the engine should spawn tasks onto. [`mesh::Mesh::start`]
/// calls this with the handle of whatever runtime it's running on — Tauri's in
/// the GUI, the `allmystuff-serve` binary's own headless.
///
/// This exists because the engine fires async tasks from threads that are **not**
/// Tokio workers: screen capture (DXGI / PipeWire / AVFoundation) and audio
/// capture (cpal) each run on their own OS thread, and their callbacks need to
/// hand work back to the async world. A bare `tokio::spawn` there panics with
/// "there is no reactor running" — so the engine routes every spawn through a
/// stored [`Handle`](tokio::runtime::Handle), which is valid from any thread.
/// (This is the role `tauri::async_runtime` played while the engine still lived
/// inside the GUI.) Idempotent: the first registration wins.
pub fn set_runtime(handle: tokio::runtime::Handle) {
    let _ = RUNTIME.set(handle);
}

/// Spawn a task onto the engine's registered runtime. Unlike [`tokio::spawn`],
/// this is safe to call from any thread (a capture/audio callback included),
/// because it spawns through a stored handle rather than the ambient runtime.
///
/// Must be called after [`set_runtime`], which [`mesh::Mesh::start`] does before
/// anything captures — every spawn in the engine happens once a session is live.
pub fn spawn<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    RUNTIME
        .get()
        .expect("allmystuff-node runtime not registered — Mesh::start calls set_runtime()")
        .spawn(future)
}
