//! "Always On" window behaviour — whether closing or minimizing the main
//! window keeps AllMyStuff alive in the system tray / menu bar instead of
//! quitting.
//!
//! The backend owns this preference, not the front-end: the close / minimize
//! decision is made in a native window-event handler that runs whether or not
//! the webview has finished loading, so localStorage would be both too late
//! and unreachable. It's persisted next to the rest of AllMyStuff's state
//! (under `~/.myownmesh`, `MYOWNMESH_HOME`-overridable, exactly like
//! `networks_store`) and surfaced to the Svelte "Always On" tab through the
//! `window_behavior` / `set_window_behavior` commands.

use std::path::PathBuf;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

/// The persisted preference. `#[serde(default)]` so an older file (or none)
/// still loads, and additive fields stay backward-compatible.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Behavior {
    /// Closing (the window's X) hides to the tray and keeps running, rather
    /// than quitting. Default on — the requested "close button minimizes".
    #[serde(default = "default_true")]
    pub close_to_tray: bool,
    /// Minimizing hides to the tray (gone from the taskbar), rather than the
    /// usual minimize. Default off — offered as a toggle.
    #[serde(default)]
    pub minimize_to_tray: bool,
}

fn default_true() -> bool {
    true
}

impl Default for Behavior {
    fn default() -> Self {
        Behavior {
            close_to_tray: true,
            minimize_to_tray: false,
        }
    }
}

/// The live store, cheap to share behind Tauri's managed state.
pub struct WindowBehavior {
    path: Option<PathBuf>,
    inner: Mutex<Behavior>,
}

impl WindowBehavior {
    /// Load the saved preference from disk (or start at the defaults).
    pub fn load() -> Self {
        let path = store_path();
        let inner = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str::<Behavior>(&s).ok())
            .unwrap_or_default();
        WindowBehavior {
            path,
            inner: Mutex::new(inner),
        }
    }

    /// The current preference (a cheap `Copy`).
    pub fn get(&self) -> Behavior {
        *self.inner.lock()
    }

    pub fn close_to_tray(&self) -> bool {
        self.inner.lock().close_to_tray
    }

    pub fn minimize_to_tray(&self) -> bool {
        self.inner.lock().minimize_to_tray
    }

    /// Update the preference and persist it. Returns the stored value (which
    /// equals the input when the write succeeded).
    pub fn set(&self, next: Behavior) -> Behavior {
        let mut inner = self.inner.lock();
        *inner = next;
        persist(&self.path, &inner);
        *inner
    }
}

fn persist(path: &Option<PathBuf>, value: &Behavior) -> bool {
    let Some(path) = path else {
        return false;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string_pretty(value) {
        Ok(s) => std::fs::write(path, s).is_ok(),
        Err(_) => false,
    }
}

/// `~/.myownmesh/allmystuff-window.json` (MYOWNMESH_HOME-overridable), beside
/// the rest of AllMyStuff's per-user state.
fn store_path() -> Option<PathBuf> {
    let home = std::env::var_os("MYOWNMESH_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)?;
    Some(home.join(".myownmesh").join("allmystuff-window.json"))
}
