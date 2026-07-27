/// MyOwnMesh release platform name for a Rust target triple.
///
/// Linux libc is part of the asset identity. In particular, the aarch64
/// glibc and static-musl archives are distinct, and using the glibc archive
/// for a musl package produces a sidecar that cannot start on the target.
pub(crate) fn release_platform_name(triple: &str) -> Result<&'static str, String> {
    Ok(match triple {
        t if t.contains("x86_64") && t.contains("linux") && t.contains("gnu") => "linux-x86_64",
        t if t.contains("aarch64") && t.contains("linux") && t.contains("musl") => {
            "linux-aarch64-musl"
        }
        t if t.contains("aarch64") && t.contains("linux") && t.contains("gnu") => "linux-aarch64",
        t if t.contains("riscv64") && t.contains("linux") && t.contains("musl") => "linux-riscv64",
        t if t.contains("x86_64") && t.contains("apple") => "macos-x86_64",
        t if t.contains("aarch64") && t.contains("apple") => "macos-aarch64",
        t if t.contains("x86_64") && t.contains("windows") => "windows-x86_64",
        other => return Err(format!("no release platform mapping for target {other}")),
    })
}

/// Copy a sidecar through a sibling temporary file, then replace the live
/// slot. A failed copy never truncates the currently staged executable.
pub(crate) fn stage_file_atomic(
    src: &std::path::Path,
    dst: &std::path::Path,
) -> Result<(), String> {
    use std::fs;

    let parent = dst
        .parent()
        .ok_or_else(|| format!("sidecar path {} has no parent", dst.display()))?;
    fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    let name = dst
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("sidecar path {} has no file name", dst.display()))?;
    let nonce = std::process::id();
    let staged = parent.join(format!(".{name}.{nonce}.stage"));
    let backup = parent.join(format!(".{name}.{nonce}.backup"));
    let _ = fs::remove_file(&staged);
    let _ = fs::remove_file(&backup);

    let copied = fs::copy(src, &staged)
        .map_err(|e| format!("copy {} to {}: {e}", src.display(), staged.display()))?;
    if copied == 0 {
        let _ = fs::remove_file(&staged);
        return Err(format!("copy {} produced an empty sidecar", src.display()));
    }
    fs::OpenOptions::new()
        .write(true)
        .open(&staged)
        .and_then(|file| file.sync_all())
        .map_err(|e| {
            let _ = fs::remove_file(&staged);
            format!("flush staged sidecar {}: {e}", staged.display())
        })?;

    #[cfg(windows)]
    {
        let had_previous = dst.exists();
        if had_previous {
            fs::rename(dst, &backup).map_err(|e| {
                let _ = fs::remove_file(&staged);
                format!(
                    "move existing sidecar {} to {}: {e}",
                    dst.display(),
                    backup.display()
                )
            })?;
        }
        if let Err(error) = fs::rename(&staged, dst) {
            if had_previous {
                let _ = fs::rename(&backup, dst);
            }
            let _ = fs::remove_file(&staged);
            return Err(format!(
                "replace sidecar {} with {}: {error}",
                dst.display(),
                staged.display()
            ));
        }
        if had_previous {
            // The live slot is already valid. A cleanup failure must not turn
            // that success into a bundle failure, since the caller responds to
            // bundle failure by invalidating the live slot.
            let _ = fs::remove_file(&backup);
        }
    }
    #[cfg(not(windows))]
    {
        fs::rename(&staged, dst).map_err(|e| {
            let _ = fs::remove_file(&staged);
            format!(
                "replace sidecar {} with {}: {e}",
                dst.display(),
                staged.display()
            )
        })?;
    }
    Ok(())
}

/// Fail closed after staging failure. The external-bin slot remains present
/// for development builds, but it cannot contain a stale executable, and its
/// cache sentinel cannot claim that it is current.
pub(crate) fn invalidate_sidecar(
    slot: &std::path::Path,
    sentinel: &std::path::Path,
) -> Result<(), String> {
    use std::fs;

    if let Some(parent) = slot.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    fs::write(slot, b"").map_err(|e| format!("invalidate {}: {e}", slot.display()))?;
    match fs::remove_file(sentinel) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "remove stale sidecar sentinel {}: {error}",
            sentinel.display()
        )),
    }
}
