//! Cross-platform OS-clipboard access for cross-machine copy/paste.
//!
//! The bundled clipboard-manager plugin only reaches text, HTML and images;
//! copying *files* puts file references on the OS clipboard in a platform
//! format (CF_HDROP on Windows, file URLs on macOS, `text/uri-list` on
//! Linux) it can't read or write. `clipboard-rs` can, so this module owns
//! the clipboard for the copy/paste feature.
//!
//! All access runs on one dedicated thread that holds the single
//! [`ClipboardContext`] for the app's life. That matters on X11, where the
//! process that *set* the clipboard must stay alive to hand the data to
//! whoever pastes — a transient context would lose the selection the moment
//! it dropped. Keeping the context on its own thread also sidesteps the
//! `Send`/`Sync` question: it never leaves that thread.

use std::path::{Path, PathBuf};
use std::sync::mpsc::SyncSender;

use clipboard_rs::common::RustImage;
use clipboard_rs::{Clipboard, ClipboardContext, ContentFormat, RustImageData};
use tokio::sync::mpsc::UnboundedSender;

/// One file referenced on the clipboard — its base name, real path on this
/// machine, and byte size. The bytes stream from `path` at paste time, never
/// held in memory.
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

enum Cmd {
    Read(SyncSender<Option<LocalClip>>),
    SetText(String),
    SetImage(Vec<u8>), // PNG bytes
    SetFiles(Vec<String>),
}

/// Handle to the clipboard thread. Cheap to clone (just the command sender).
/// A tokio sender so it's `Send + Sync` — `Mesh` holds it inside an `Arc`.
#[derive(Clone)]
pub struct ClipboardService {
    tx: UnboundedSender<Cmd>,
}

impl ClipboardService {
    /// Spawn the clipboard thread. Always returns a handle; if the OS
    /// clipboard can't be opened (a headless box, no display), reads return
    /// `None` and writes are dropped — never a panic.
    pub fn spawn() -> ClipboardService {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Cmd>();
        std::thread::Builder::new()
            .name("clipboard".into())
            .spawn(move || {
                let ctx = match ClipboardContext::new() {
                    Ok(c) => Some(c),
                    Err(e) => {
                        tracing::warn!("OS clipboard unavailable: {e}");
                        None
                    }
                };
                while let Some(cmd) = rx.blocking_recv() {
                    let Some(ctx) = ctx.as_ref() else {
                        if let Cmd::Read(resp) = cmd {
                            let _ = resp.send(None);
                        }
                        continue;
                    };
                    match cmd {
                        Cmd::Read(resp) => {
                            let _ = resp.send(read_clipboard(ctx));
                        }
                        Cmd::SetText(t) => {
                            if let Err(e) = ctx.set_text(t) {
                                tracing::warn!("clipboard set_text failed: {e}");
                            }
                        }
                        Cmd::SetImage(png) => match RustImageData::from_bytes(&png) {
                            Ok(img) => {
                                if let Err(e) = ctx.set_image(img) {
                                    tracing::warn!("clipboard set_image failed: {e}");
                                }
                            }
                            Err(e) => tracing::warn!("clipboard image decode failed: {e}"),
                        },
                        Cmd::SetFiles(paths) => {
                            if let Err(e) = ctx.set_files(paths) {
                                tracing::warn!("clipboard set_files failed: {e}");
                            }
                        }
                    }
                }
            })
            .expect("spawn clipboard thread");
        ClipboardService { tx }
    }

    /// Read this machine's clipboard. Blocking — call from a blocking
    /// context (the mesh wraps it in `spawn_blocking`). `None` when the
    /// clipboard is empty, unreadable, or unavailable.
    pub fn read(&self) -> Option<LocalClip> {
        let (resp_tx, resp_rx) = std::sync::mpsc::sync_channel(1);
        self.tx.send(Cmd::Read(resp_tx)).ok()?;
        resp_rx.recv().ok().flatten()
    }

    pub fn set_text(&self, text: String) {
        let _ = self.tx.send(Cmd::SetText(text));
    }

    /// Set the clipboard to a PNG image (decoded on the clipboard thread).
    pub fn set_image(&self, png: Vec<u8>) {
        let _ = self.tx.send(Cmd::SetImage(png));
    }

    /// Point the clipboard at real files on this machine, so a paste in a
    /// file manager materializes them.
    pub fn set_files(&self, paths: Vec<String>) {
        let _ = self.tx.send(Cmd::SetFiles(paths));
    }
}

/// Query the clipboard, preferring files, then an image, then text — the
/// order that keeps a file copy from degrading to a text-path label.
fn read_clipboard(ctx: &ClipboardContext) -> Option<LocalClip> {
    if ctx.has(ContentFormat::Files) {
        if let Ok(raw) = ctx.get_files() {
            let files: Vec<LocalFile> = raw
                .iter()
                .filter_map(|entry| {
                    let path = normalize_clip_path(entry);
                    let meta = std::fs::metadata(&path).ok()?;
                    if !meta.is_file() {
                        return None; // directories are a follow-up
                    }
                    Some(LocalFile {
                        name: base_name(&path),
                        path,
                        size: meta.len(),
                    })
                })
                .collect();
            if !files.is_empty() {
                return Some(LocalClip::Files(files));
            }
        }
    }
    if ctx.has(ContentFormat::Image) {
        if let Ok(img) = ctx.get_image() {
            if let Ok(png) = img.to_png() {
                return Some(LocalClip::Image(png.get_bytes().to_vec()));
            }
        }
    }
    if ctx.has(ContentFormat::Text) {
        if let Ok(text) = ctx.get_text() {
            if !text.is_empty() {
                return Some(LocalClip::Text(text));
            }
        }
    }
    None
}

/// Turn a clipboard file entry into a real path: a `file://` URL (Linux /
/// macOS `text/uri-list`) is decoded; a bare path (Windows CF_HDROP) stands.
fn normalize_clip_path(entry: &str) -> PathBuf {
    let s = entry.trim();
    if let Some(rest) = s.strip_prefix("file://") {
        // Drop an optional host ("file://host/path" → "/path"), keeping the
        // path's own leading slash.
        let path = match rest.find('/') {
            Some(i) => &rest[i..],
            None => rest,
        };
        PathBuf::from(percent_decode(path))
    } else {
        PathBuf::from(s)
    }
}

/// Minimal percent-decoding for file URLs (`%20` → space, …). A malformed
/// escape is left untouched.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn base_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into())
}

/// The staging directory a received clipboard transfer lands in, so the OS
/// clipboard can point a paste at real files. Per-transfer so concurrent
/// pastes never collide; under the system temp dir.
pub fn staging_dir(transfer: u64) -> PathBuf {
    std::env::temp_dir()
        .join("allmystuff-clipboard")
        .join(transfer.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_urls_decode_to_paths() {
        assert_eq!(
            normalize_clip_path("file:///home/u/my%20file.txt"),
            PathBuf::from("/home/u/my file.txt")
        );
        // A bare path (Windows-style entry) is left alone.
        assert_eq!(
            normalize_clip_path("C:\\Users\\u\\a.txt"),
            PathBuf::from("C:\\Users\\u\\a.txt")
        );
        // A host component is dropped.
        assert_eq!(
            normalize_clip_path("file://host/srv/data.bin"),
            PathBuf::from("/srv/data.bin")
        );
    }

    #[test]
    fn base_name_is_the_final_component() {
        assert_eq!(base_name(Path::new("/a/b/c.png")), "c.png");
    }
}
