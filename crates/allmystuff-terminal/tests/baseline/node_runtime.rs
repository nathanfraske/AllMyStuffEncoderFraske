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
