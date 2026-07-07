//! No-op twin of [`crate::clipboard`] for capture-less builds
//! (`--no-default-features`, i.e. iOS — see the `host` feature in
//! `Cargo.toml`).
//!
//! `clipboard-rs` has no iOS backend (UIPasteboard is a webview/UIKit
//! concern the mobile GUI can grow later), so reads answer `None` and
//! writes drop — the exact posture the desktop service already takes on a
//! headless box whose OS clipboard won't open. [`staging_dir`] stays real:
//! inbound file *transfers* still need somewhere to land.

use std::path::PathBuf;

/// One file referenced on the clipboard.
#[derive(Debug, Clone)]
pub struct LocalFile {
    pub name: String,
    pub path: PathBuf,
    pub size: u64,
}

/// What was on this machine's clipboard when a paste fired.
#[derive(Debug, Clone)]
pub enum LocalClip {
    Text(String),
    /// A bitmap, already PNG-encoded.
    Image(Vec<u8>),
    /// Files by reference (bytes stream from disk).
    Files(Vec<LocalFile>),
}

/// Handle the mesh holds either way. Cheap to clone; owns nothing.
#[derive(Clone, Default)]
pub struct ClipboardService {}

impl ClipboardService {
    /// No thread to spawn — there is no OS clipboard here to watch.
    pub fn spawn() -> ClipboardService {
        ClipboardService::default()
    }

    #[allow(dead_code)]
    pub fn read(&self) -> Option<LocalClip> {
        None
    }

    pub fn set_text(&self, _text: String) {}

    pub fn set_image(&self, _png: Vec<u8>) {}

    pub fn set_files(&self, _paths: Vec<String>) {}
}

/// The staging directory a received clipboard transfer lands in. Per-transfer
/// so concurrent pastes never collide; under the system temp dir (iOS gives
/// every app its own).
pub fn staging_dir(transfer: u64) -> PathBuf {
    std::env::temp_dir()
        .join("allmystuff-clipboard")
        .join(transfer.to_string())
}
