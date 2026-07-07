//! Phone-side diagnostics you can actually reach.
//!
//! Tethered to Xcode, stderr is enough — the console pane shows every line
//! live. Launched from the home screen, iOS throws stderr away, and "it
//! largely works" becomes undebuggable. So the same `tracing` stream is also
//! written to a file the user can reach **on the phone**: on iOS it lives in
//! the app's Documents folder, which `UIFileSharingEnabled` +
//! `LSSupportsOpeningDocumentsInPlace` (see `Info.ios.plist`) surface in the
//! Files app — *On My iPhone → AllMyStuff → allmystuff.log* — and in
//! Finder's device pane when cabled. Share-sheet it straight out of Files.
//!
//! One previous run is kept (`allmystuff.log.1`): the run you want to read
//! is usually the one that just crashed or misbehaved, and it must survive
//! the relaunch that reads it.

use std::fs::File;
use std::path::PathBuf;

use tauri::Manager;

/// Where the log file lives. Documents on iOS (reachable in the Files app);
/// the platform log dir elsewhere (Android's files dir, `~/Library/Logs` /
/// XDG state on a desktop smoke build).
fn log_dir(app: &tauri::AppHandle) -> Option<PathBuf> {
    #[cfg(target_os = "ios")]
    {
        app.path().document_dir().ok()
    }
    #[cfg(not(target_os = "ios"))]
    {
        app.path().app_log_dir().ok()
    }
}

/// Install the global `tracing` subscriber: INFO+, to stderr (the Xcode /
/// logcat console) *and* to `allmystuff.log`. Returns the file's path so the
/// caller can log where it landed; `None` means the file sink couldn't be
/// set up and stderr alone is active — never an error, diagnostics must not
/// take the app down.
pub fn init(app: &tauri::AppHandle) -> Option<PathBuf> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let file = log_dir(app).and_then(|dir| {
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join("allmystuff.log");
        // Keep exactly one previous run: the run worth reading is usually
        // the one that just misbehaved, and it must survive this relaunch.
        if path.exists() {
            let _ = std::fs::rename(&path, dir.join("allmystuff.log.1"));
        }
        Some((File::create(&path).ok()?, path))
    });

    let level = tracing_subscriber::filter::LevelFilter::INFO;
    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(false);

    match file {
        Some((file, path)) => {
            let file_layer = tracing_subscriber::fmt::layer()
                .with_writer(std::sync::Mutex::new(file))
                .with_ansi(false);
            let _ = tracing_subscriber::registry()
                .with(level)
                .with(stderr_layer)
                .with(file_layer)
                .try_init();
            Some(path)
        }
        None => {
            let _ = tracing_subscriber::registry()
                .with(level)
                .with(stderr_layer)
                .try_init();
            None
        }
    }
}
