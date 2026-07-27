#[path = "../sidecar_platform.rs"]
mod sidecar_platform;

use sidecar_platform::{invalidate_sidecar, release_platform_name, stage_file_atomic};

#[test]
fn release_assets_match_supported_target_abi() {
    let cases = [
        ("x86_64-unknown-linux-gnu", "linux-x86_64"),
        ("aarch64-unknown-linux-gnu", "linux-aarch64"),
        ("aarch64-unknown-linux-musl", "linux-aarch64-musl"),
        ("riscv64gc-unknown-linux-musl", "linux-riscv64"),
        ("x86_64-apple-darwin", "macos-x86_64"),
        ("aarch64-apple-darwin", "macos-aarch64"),
        ("x86_64-pc-windows-msvc", "windows-x86_64"),
    ];

    for (triple, expected) in cases {
        assert_eq!(release_platform_name(triple).unwrap(), expected);
    }
}

#[test]
fn unsupported_linux_abis_fail_closed() {
    for triple in [
        "x86_64-unknown-linux-musl",
        "aarch64-unknown-linux-uclibc",
        "riscv64gc-unknown-linux-gnu",
    ] {
        assert!(
            release_platform_name(triple).is_err(),
            "{triple} must not receive an ABI-incompatible sidecar"
        );
    }
}

fn temporary_directory(label: &str) -> std::path::PathBuf {
    let unique = format!(
        "allmystuff-sidecar-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    );
    let path = std::env::temp_dir().join(unique);
    std::fs::create_dir_all(&path).expect("create temporary directory");
    path
}

#[test]
fn atomic_stage_replaces_a_stale_slot_without_leaving_work_files() {
    let directory = temporary_directory("replace");
    let source = directory.join("source.bin");
    let slot = directory.join("sidecar.bin");
    std::fs::write(&source, b"new-sidecar").expect("write source");
    std::fs::write(&slot, b"stale-sidecar").expect("write stale slot");

    stage_file_atomic(&source, &slot).expect("stage sidecar");

    assert_eq!(std::fs::read(&slot).expect("read slot"), b"new-sidecar");
    let work_files = std::fs::read_dir(&directory)
        .expect("read temporary directory")
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.ends_with(".stage") || name.ends_with(".backup")
        })
        .count();
    assert_eq!(work_files, 0);
    std::fs::remove_dir_all(directory).expect("remove temporary directory");
}

#[test]
fn failed_bundle_invalidation_truncates_stale_slot_and_removes_sentinel() {
    let directory = temporary_directory("invalidate");
    let slot = directory.join("sidecar.bin");
    let sentinel = directory.join(".bundled-serve");
    std::fs::write(&slot, b"stale-sidecar").expect("write stale slot");
    std::fs::write(&sentinel, b"stale-signature").expect("write stale sentinel");

    invalidate_sidecar(&slot, &sentinel).expect("invalidate sidecar");

    assert_eq!(std::fs::metadata(&slot).expect("slot metadata").len(), 0);
    assert!(!sentinel.exists());
    std::fs::remove_dir_all(directory).expect("remove temporary directory");
}
