//! Crash-safe, tolerant JSON persistence for the consent store.
//!
//! Mirrors the discipline of AllMyStuff's `node/src/persist.rs`: writes go
//! through a temp file + fsync + atomic rename (mode 0600 on Unix) so a crash
//! mid-write never leaves a half-written store; loads treat a missing file as
//! "empty" and quarantine a corrupt file aside rather than failing the whole
//! app. A bad or absent consent file must never brick the customer's machine —
//! at worst it re-prompts.

use std::fs;
use std::io;
use std::path::Path;

use serde::de::DeserializeOwned;

/// Atomically write `bytes` to `path`: write a sibling temp file, fsync it,
/// then rename over the target. On Unix the file is created 0600.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = tmp_path(path);
    {
        let mut f = fs::File::create(&tmp)?;
        set_owner_only(&f)?;
        use io::Write as _;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    // Rename is atomic on the same filesystem; the sibling temp guarantees it.
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Load and deserialize `path`. Missing file → `T::default()`. Corrupt file →
/// move it aside to `<name>.corrupt` and return `T::default()`, so the next
/// save starts clean instead of appending to garbage.
pub(crate) fn load_json<T: DeserializeOwned + Default>(path: &Path) -> T {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(_) => return T::default(),
    };
    match serde_json::from_slice::<T>(&bytes) {
        Ok(v) => v,
        Err(_) => {
            // Quarantine the unreadable file; ignore failure (best-effort).
            let corrupt = corrupt_path(path);
            let _ = fs::rename(path, corrupt);
            T::default()
        }
    }
}

fn tmp_path(path: &Path) -> std::path::PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".tmp");
    std::path::PathBuf::from(s)
}

fn corrupt_path(path: &Path) -> std::path::PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".corrupt");
    std::path::PathBuf::from(s)
}

#[cfg(unix)]
fn set_owner_only(file: &fs::File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    file.set_permissions(fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_owner_only(_file: &fs::File) -> io::Result<()> {
    Ok(())
}
