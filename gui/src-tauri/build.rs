//! Build-time bundling of the `myownmesh` daemon as a Tauri sidecar.
//!
//! AllMyStuff *is* a mesh app — end users shouldn't have to install
//! MyOwnMesh separately. So this build script obtains the daemon binary
//! pinned in `.myownmesh-rev` and drops it at
//! `binaries/myownmesh-<target-triple>{.exe}`; `tauri.conf.json`'s
//! `externalBin` then ships it *inside* the app bundle. Resolution order:
//!
//!   1. **`MYOWNMESH_BIN`** override — a release pipeline can hand us a
//!      pre-signed binary and skip the fetch.
//!   2. **Sibling checkout** — a side-by-side `../MyOwnMesh` with a built
//!      `target/{release,debug}/myownmesh` (the both-repos dev setup).
//!   3. **Prebuilt release asset** — download
//!      `myownmesh-<platform>.tar.gz` from MyOwnMesh's GitHub Releases for
//!      the pinned tag and verify its checked-in SHA-256 before extraction.
//!
//! Development builds are best-effort: on failure we stamp a zero-byte stub
//! at the sidecar slot and the runtime falls back to PATH / sibling discovery
//! in `daemon_spawn.rs`. Release builds set
//! `ALLMYSTUFF_REQUIRE_SIDECAR=1`, which turns that failure into a hard error.
//! Set `ALLMYSTUFF_SKIP_SIDECAR=1` to skip the fetch entirely for offline CI
//! builds that only verify compilation.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

mod sidecar_platform;
use sidecar_platform::{invalidate_sidecar, release_platform_name, stage_file_atomic};

fn main() {
    if let Err(e) = bundle_myownmesh_sidecar() {
        println!(
            "cargo:warning=myownmesh sidecar bundle skipped: {e} — the app still \
             builds; at runtime it falls back to a sibling MyOwnMesh build, \
             `myownmesh` on PATH, or MYOWNMESH_BIN"
        );
        write_sidecar_stub().unwrap_or_else(|stub_err| {
            panic!("could not invalidate failed myownmesh sidecar bundle: {stub_err}")
        });
        if env::var_os("ALLMYSTUFF_REQUIRE_SIDECAR").is_some() {
            panic!("required myownmesh sidecar could not be bundled: {e}");
        }
    }
    // The node binary the service runs (`allmystuff-serve`). Bundling it inside
    // the app means the desktop "Install as a service" works on *every* install
    // path — including a `.dmg`/`.msi`-only install that never ran the curl
    // installer — not just where the CLI dropped it on PATH.
    if let Err(e) = bundle_serve_sidecar() {
        println!(
            "cargo:warning=allmystuff-serve sidecar bundle skipped: {e} — the app \
             still builds; at runtime it falls back to a sibling node build, \
             `allmystuff-serve` on PATH, or the install dirs"
        );
        write_serve_stub().unwrap_or_else(|stub_err| {
            panic!("could not invalidate failed allmystuff-serve sidecar bundle: {stub_err}")
        });
        if env::var_os("ALLMYSTUFF_REQUIRE_SIDECAR").is_some() {
            panic!("required allmystuff-serve sidecar could not be bundled: {e}");
        }
    }
    tauri_build::build();
}

fn target_triple() -> String {
    env::var("TARGET").unwrap_or_else(|_| "unknown".into())
}

fn exe_suffix() -> &'static str {
    if target_triple().contains("windows") {
        ".exe"
    } else {
        ""
    }
}

fn binaries_dir() -> PathBuf {
    PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("binaries")
}

fn sidecar_path() -> PathBuf {
    binaries_dir().join(format!("myownmesh-{}{}", target_triple(), exe_suffix()))
}

/// `.myownmesh-rev` lives at the repo root — two parents up from
/// `gui/src-tauri`.
fn rev_file() -> PathBuf {
    PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .parent()
        .and_then(|p| p.parent())
        .map(|root| root.join(".myownmesh-rev"))
        .unwrap_or_else(|| PathBuf::from(".myownmesh-rev"))
}

/// Release tag, immutable tag commit, and official per-platform archive hashes.
fn release_metadata_file() -> PathBuf {
    PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .parent()
        .and_then(|p| p.parent())
        .map(|root| root.join(".myownmesh-release-sha256"))
        .unwrap_or_else(|| PathBuf::from(".myownmesh-release-sha256"))
}

#[derive(Debug)]
struct ReleaseMetadata {
    tag: String,
    commit: String,
    hashes: BTreeMap<String, String>,
}

fn read_release_metadata(path: &Path) -> Result<ReleaseMetadata, String> {
    let text = fs::read_to_string(path)
        .map_err(|e| format!("read release metadata {}: {e}", path.display()))?;
    let mut entries = BTreeMap::new();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split_whitespace();
        let key = fields
            .next()
            .ok_or_else(|| format!("{}:{}: missing key", path.display(), index + 1))?;
        let value = fields
            .next()
            .ok_or_else(|| format!("{}:{}: missing value", path.display(), index + 1))?;
        if fields.next().is_some() {
            return Err(format!(
                "{}:{}: expected exactly two fields",
                path.display(),
                index + 1
            ));
        }
        if entries.insert(key.to_string(), value.to_string()).is_some() {
            return Err(format!(
                "{}:{}: duplicate key {key}",
                path.display(),
                index + 1
            ));
        }
    }

    let tag = entries
        .remove("tag")
        .ok_or_else(|| format!("{}: missing tag", path.display()))?;
    let commit = entries
        .remove("commit")
        .ok_or_else(|| format!("{}: missing commit", path.display()))?;
    if !is_lower_hex(&commit, 40) {
        return Err(format!(
            "{}: commit must be a 40-character lowercase hexadecimal SHA",
            path.display()
        ));
    }
    for (platform, hash) in &entries {
        if !is_lower_hex(hash, 64) {
            return Err(format!(
                "{}: hash for {platform} must be 64 lowercase hexadecimal characters",
                path.display()
            ));
        }
    }
    Ok(ReleaseMetadata {
        tag,
        commit,
        hashes: entries,
    })
}

fn is_lower_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn bundle_myownmesh_sidecar() -> Result<(), String> {
    // The runtime needs the triple to find the dev-staged sidecar name.
    println!("cargo:rustc-env=DAEMON_SIDECAR_TRIPLE={}", target_triple());
    let rev = fs::read_to_string(rev_file())
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty());
    if let Some(r) = &rev {
        println!("cargo:rustc-env=MYOWNMESH_PIN={r}");
    }
    println!("cargo:rerun-if-changed={}", rev_file().display());
    println!(
        "cargo:rerun-if-changed={}",
        release_metadata_file().display()
    );
    println!("cargo:rerun-if-env-changed=MYOWNMESH_BIN");
    println!("cargo:rerun-if-env-changed=ALLMYSTUFF_SKIP_SIDECAR");
    println!("cargo:rerun-if-env-changed=ALLMYSTUFF_REQUIRE_SIDECAR");

    let bin_dir = binaries_dir();
    fs::create_dir_all(&bin_dir).map_err(|e| e.to_string())?;
    let sidecar = sidecar_path();
    let sentinel = bin_dir.join(".bundled-rev");

    if env::var_os("ALLMYSTUFF_SKIP_SIDECAR").is_some() {
        return Err("ALLMYSTUFF_SKIP_SIDECAR set".into());
    }

    // Resolve what we *would* stage before copying anything, as a cache
    // signature — so the idempotency check can't be fooled by a source
    // that changed meaning (the old sentinel recorded the pin even when
    // the bytes came from a sibling build, which made a stale sibling
    // stick around forever once stamped).
    //
    // 1. Explicit override.
    if let Ok(p) = env::var("MYOWNMESH_BIN") {
        let p = PathBuf::from(p);
        if p.exists() {
            println!("cargo:rerun-if-changed={}", p.display());
            let sig = format!("bin:{}:{}", p.display(), sha256_file(&p)?);
            if !staged_matches(&sidecar, &sentinel, &sig) {
                stage(&p, &sidecar)?;
                let _ = fs::write(&sentinel, &sig);
                println!("cargo:warning=[sidecar] bundled daemon from MYOWNMESH_BIN");
            }
            return Ok(());
        }
    }

    // 2. Sibling checkout (release first, then debug) — but only when its
    // build can actually satisfy the pin. A sibling *ahead* of the pin is
    // the active-development loop and wins; a sibling *behind* it is a
    // stale artifact that would silently mask the very features the pin
    // bump was for (a v0.2.0 leftover shadowing the v0.2.1 video lane,
    // say), so it's skipped with a note and the pinned release is fetched.
    if let Some(p) = sibling_daemon() {
        // Watch the sibling binary itself so a plain `cargo build --bin
        // myownmesh` in the sibling checkout re-triggers this script and
        // re-stages the fresh daemon. Without this, the only watched inputs
        // are `.myownmesh-rev` + the override env vars, so a rebuilt sibling
        // was silently ignored and the app kept spawning the previously
        // staged daemon — the "I rebuilt it but nothing changed" trap that
        // makes the sibling dev loop untrustworthy for exactly the media
        // fixes it exists to test.
        println!("cargo:rerun-if-changed={}", p.display());
        let sibling_ver = binary_version(&p);
        let pin_ver = rev
            .as_deref()
            .and_then(|r| r.strip_prefix('v'))
            .and_then(parse_semver);
        let acceptable = match (sibling_ver, pin_ver) {
            (Some(s), Some(want)) => s >= want,
            // Unknown version (no --version? exec failed): trust the dev
            // setup rather than break its loop, but say so.
            (None, _) => {
                println!(
                    "cargo:warning=[sidecar] couldn't read the sibling daemon's version; using it anyway"
                );
                true
            }
            // No (parseable) pin → the sibling is the best truth we have.
            (Some(_), None) => true,
        };
        if acceptable {
            let sig = format!("sib:{}:{}", p.display(), sha256_file(&p)?);
            if !staged_matches(&sidecar, &sentinel, &sig) {
                stage(&p, &sidecar)?;
                let _ = fs::write(&sentinel, &sig);
                println!("cargo:warning=[sidecar] bundled daemon from sibling MyOwnMesh checkout");
            }
            return Ok(());
        }
        println!(
            "cargo:warning=[sidecar] sibling MyOwnMesh build is v{} but the pin wants {} — ignoring it (rebuild the sibling to use it); fetching the pinned release",
            sibling_ver
                .map(|(a, b, c)| format!("{a}.{b}.{c}"))
                .unwrap_or_default(),
            rev.as_deref().unwrap_or("?"),
        );
    }

    // 3. Prebuilt release asset for a tag, or a source build for a raw rev.
    let rev = rev.ok_or("no .myownmesh-rev pin and no override/sibling daemon")?;
    let (sig, expected_hash) = if rev.starts_with('v') {
        let metadata = read_release_metadata(&release_metadata_file())?;
        if metadata.tag != rev {
            return Err(format!(
                "release metadata tag {} does not match pin {rev}",
                metadata.tag
            ));
        }
        let platform = release_platform_name(&target_triple())?;
        let hash = metadata
            .hashes
            .get(platform)
            .cloned()
            .ok_or_else(|| format!("release metadata has no hash for {platform}"))?;
        (
            format!(
                "release:{}:{}:{}:{}",
                metadata.tag, metadata.commit, platform, hash
            ),
            Some(hash),
        )
    } else {
        (format!("rev:{rev}"), None)
    };
    if staged_matches(&sidecar, &sentinel, &sig) {
        return Ok(());
    }
    let out_dir = PathBuf::from(env::var("OUT_DIR").map_err(|e| e.to_string())?);
    let staging = out_dir.join("myownmesh-staging");
    fs::create_dir_all(&staging).map_err(|e| e.to_string())?;

    let staged_bin = if let Some(expected_hash) = expected_hash {
        download_release_asset(&rev, &expected_hash, &staging)?
    } else {
        cargo_install(&rev, &staging)?
    };

    stage(&staged_bin, &sidecar)?;
    let _ = fs::write(&sentinel, &sig);
    println!(
        "cargo:warning=[sidecar] daemon ready ({} bytes)",
        fs::metadata(&sidecar).map(|m| m.len()).unwrap_or(0)
    );
    Ok(())
}

/// True when the sidecar slot is non-empty and the sentinel records
/// exactly this staging signature — the skip condition.
fn staged_matches(sidecar: &Path, sentinel: &Path, sig: &str) -> bool {
    let present = sidecar.metadata().map(|m| m.len() > 0).unwrap_or(false);
    present
        && fs::read_to_string(sentinel)
            .map(|s| s.trim() == sig)
            .unwrap_or(false)
}

/// Ask a daemon binary its version (`myownmesh 0.2.1` → `(0, 2, 1)`).
fn binary_version(p: &Path) -> Option<(u64, u64, u64)> {
    let out = Command::new(p).arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    parse_semver(text.split_whitespace().last()?)
}

/// `"0.2.1"` → `(0, 2, 1)` (tolerates a trailing pre-release/build tag on
/// the patch segment by ignoring it).
fn parse_semver(s: &str) -> Option<(u64, u64, u64)> {
    let mut parts = s.trim().splitn(3, '.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts
        .next()?
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    Some((major, minor, patch))
}

fn sibling_daemon() -> Option<PathBuf> {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").ok()?)
        .parent()?
        .parent()?
        .parent()?
        .join("MyOwnMesh");
    let name = format!("myownmesh{}", exe_suffix());
    for profile in ["release", "debug"] {
        let p = root.join("target").join(profile).join(&name);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// Download + extract `myownmesh-<platform>.{tar.gz,zip}` for `tag`,
/// returning the path to the extracted binary. Shells out to `curl` +
/// `tar` / `Expand-Archive` so the build needs no extra crates.
fn download_release_asset(
    tag: &str,
    expected_hash: &str,
    staging: &Path,
) -> Result<PathBuf, String> {
    let triple = target_triple();
    let platform = release_platform_name(&triple)?;
    let windows = triple.contains("windows");
    let archive_ext = if windows { "zip" } else { "tar.gz" };
    let asset = format!("myownmesh-{platform}.{archive_ext}");
    let url = format!("https://github.com/mrjeeves/MyOwnMesh/releases/download/{tag}/{asset}");
    let archive = staging.join(&asset);
    let _ = fs::remove_file(&archive);

    let status = Command::new("curl")
        .args(["-fSL", "--retry", "3", "-o"])
        .arg(&archive)
        .arg(&url)
        .status()
        .map_err(|e| format!("curl spawn failed: {e} (install curl, or use a sibling checkout)"))?;
    if !status.success() {
        return Err(format!("curl failed fetching {url}"));
    }
    if fs::metadata(&archive).map(|m| m.len()).unwrap_or(0) == 0 {
        return Err("downloaded archive is empty".into());
    }
    let actual_hash = sha256_file(&archive)?;
    if actual_hash != expected_hash {
        return Err(format!(
            "SHA-256 mismatch for {asset}: expected {expected_hash}, got {actual_hash}"
        ));
    }

    if windows {
        let out = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg(format!(
                "Expand-Archive -Force -Path '{}' -DestinationPath '{}'",
                archive.display(),
                staging.display()
            ))
            .output()
            .map_err(|e| format!("Expand-Archive spawn failed: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "Expand-Archive failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
    } else {
        let out = Command::new("tar")
            .arg("-xzf")
            .arg(&archive)
            .arg("-C")
            .arg(staging)
            .output()
            .map_err(|e| format!("tar spawn failed: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "tar failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
    }

    let bin = staging.join(format!("myownmesh{}", exe_suffix()));
    if !bin.exists() {
        return Err(format!("extracted {asset} but myownmesh binary not found"));
    }
    validate_binary(&bin)?;
    Ok(bin)
}

fn stage(src: &Path, dst: &Path) -> Result<(), String> {
    validate_binary(src)?;
    stage_file_atomic(src, dst)?;
    make_executable(dst);
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file =
        fs::File::open(path).map_err(|e| format!("open {} for SHA-256: {e}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|e| format!("read {} for SHA-256: {e}", path.display()))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

/// Sanity-check the extracted binary's magic bytes — guard against an HTML
/// error page or a truncated download landing in the sidecar slot.
fn validate_binary(p: &Path) -> Result<(), String> {
    let bytes = fs::read(p).map_err(|e| e.to_string())?;
    let ok = bytes.starts_with(b"\x7fELF")        // Linux ELF
        || bytes.starts_with(b"MZ")               // Windows PE
        || bytes.starts_with(&[0xCF, 0xFA, 0xED, 0xFE]) // macOS Mach-O (64-bit LE)
        || bytes.starts_with(&[0xCA, 0xFE, 0xBA, 0xBE]); // macOS universal
    if ok {
        Ok(())
    } else {
        Err(format!("{} is not a recognised executable", p.display()))
    }
}

/// Build the daemon from source at the pinned rev (tag or branch/sha) when
/// a prebuilt asset isn't available.
fn cargo_install(rev: &str, staging: &Path) -> Result<PathBuf, String> {
    println!("cargo:warning=building myownmesh daemon via cargo install (rev: {rev}); first build is slow");
    let root = staging.join("cargo-install-root");
    let mut cmd = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cmd.args(["install", "--git", "https://github.com/mrjeeves/MyOwnMesh"]);
    if rev.starts_with('v') {
        cmd.args(["--tag", rev]);
    } else {
        cmd.args(["--rev", rev]);
    }
    cmd.args(["--bin", "myownmesh", "--locked", "--root"])
        .arg(&root);
    let status = cmd
        .status()
        .map_err(|e| format!("cargo install spawn failed: {e}"))?;
    if !status.success() {
        return Err(format!("cargo install --git failed for rev {rev}"));
    }
    let bin = root.join("bin").join(format!("myownmesh{}", exe_suffix()));
    if !bin.exists() {
        return Err("cargo install succeeded but binary is missing".into());
    }
    Ok(bin)
}

/// Zero-byte placeholder so `tauri_build`'s `externalBin` existence check
/// passes when no real daemon could be staged. The runtime ignores
/// zero-byte stubs.
fn write_sidecar_stub() -> Result<(), String> {
    let bin_dir = binaries_dir();
    fs::create_dir_all(&bin_dir).map_err(|e| e.to_string())?;
    let p = sidecar_path();
    let sentinel = bin_dir.join(".bundled-rev");
    invalidate_sidecar(&p, &sentinel)?;
    make_executable(&p);
    Ok(())
}

// --- allmystuff-serve sidecar (the node binary the OS service runs) ---------

fn serve_sidecar_path() -> PathBuf {
    binaries_dir().join(format!(
        "allmystuff-serve-{}{}",
        target_triple(),
        exe_suffix()
    ))
}

/// Stage `allmystuff-serve` into the sidecar slot so `externalBin` ships it.
/// It's our own binary (built in the same release run, see release.yml's "Build
/// node serve binary" step), so this is just a local lookup + copy — no network
/// fetch like the daemon.
fn bundle_serve_sidecar() -> Result<(), String> {
    println!("cargo:rerun-if-env-changed=ALLMYSTUFF_SERVE_BIN");
    let bin_dir = binaries_dir();
    fs::create_dir_all(&bin_dir).map_err(|e| e.to_string())?;
    let sidecar = serve_sidecar_path();

    let src = locate_serve_binary().ok_or(
        "allmystuff-serve not built (cargo build --release --manifest-path \
         node/Cargo.toml --bin allmystuff-serve) and no ALLMYSTUFF_SERVE_BIN override",
    )?;
    println!("cargo:rerun-if-changed={}", src.display());
    let sig = format!("serve:{}:{}", src.display(), sha256_file(&src)?);
    let sentinel = bin_dir.join(".bundled-serve");
    if !staged_matches(&sidecar, &sentinel, &sig) {
        stage(&src, &sidecar)?;
        let _ = fs::write(&sentinel, &sig);
        println!(
            "cargo:warning=[serve sidecar] bundled allmystuff-serve from {}",
            src.display()
        );
    }
    Ok(())
}

/// Find a built `allmystuff-serve`: an `ALLMYSTUFF_SERVE_BIN` override, else the
/// node workspace's target dir (with or without a `--target <triple>` segment,
/// release before debug).
fn locate_serve_binary() -> Option<PathBuf> {
    let name = format!("allmystuff-serve{}", exe_suffix());
    if let Some(p) = env::var_os("ALLMYSTUFF_SERVE_BIN") {
        let p = PathBuf::from(p);
        if nonempty_file(&p) {
            return Some(p);
        }
    }
    let node_target = PathBuf::from(env::var("CARGO_MANIFEST_DIR").ok()?)
        .parent()? // gui
        .parent()? // repo root
        .join("node")
        .join("target");
    let triple = target_triple();
    let candidates = [
        node_target.join(&triple).join("release").join(&name),
        node_target.join(&triple).join("debug").join(&name),
        node_target.join("release").join(&name),
        node_target.join("debug").join(&name),
    ];
    candidates.into_iter().find(|p| nonempty_file(p))
}

fn nonempty_file(p: &Path) -> bool {
    p.metadata()
        .map(|m| m.is_file() && m.len() > 0)
        .unwrap_or(false)
}

/// Zero-byte placeholder for the serve sidecar slot — same role as
/// [`write_sidecar_stub`], so a build with no staged `allmystuff-serve` still
/// satisfies `externalBin` and the runtime falls back to PATH / install dirs.
fn write_serve_stub() -> Result<(), String> {
    let bin_dir = binaries_dir();
    fs::create_dir_all(&bin_dir).map_err(|e| e.to_string())?;
    let p = serve_sidecar_path();
    let sentinel = bin_dir.join(".bundled-serve");
    invalidate_sidecar(&p, &sentinel)?;
    make_executable(&p);
    Ok(())
}

#[cfg(unix)]
fn make_executable(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o755));
}
#[cfg(not(unix))]
fn make_executable(_p: &Path) {}
