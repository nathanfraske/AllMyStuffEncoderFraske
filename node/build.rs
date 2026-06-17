//! Build-time facts the node engine bakes in for daemon discovery.
//!
//! `daemon_spawn.rs` (which used to live in the GUI crate) needs two things
//! that a `cargo:rustc-env` only ever delivers to the crate owning the build
//! script — so now that the code is here, the values have to be set here too:
//!
//!  * **`DAEMON_SIDECAR_TRIPLE`** — the target triple, used to recognise the
//!    dev-staged sidecar name (`myownmesh-<triple>`).
//!  * **`MYOWNMESH_PIN`** — the daemon version the app is built against,
//!    read from the repo-root `.myownmesh-rev`. The runtime compares the
//!    daemon it finds against this and asks it to self-update when it's
//!    behind. (The GUI's own `build.rs` reads the same file to *bundle* the
//!    sidecar; this is the *runtime* half.)
//!
//! Both are best-effort: a missing pin just means no version gate, exactly
//! as `option_env!` already handles.

use std::path::PathBuf;

fn main() {
    println!(
        "cargo:rustc-env=DAEMON_SIDECAR_TRIPLE={}",
        std::env::var("TARGET").unwrap_or_default()
    );

    // `.myownmesh-rev` lives at the repo root — one parent up from `node/`.
    let rev_file = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
        .parent()
        .map(|root| root.join(".myownmesh-rev"))
        .unwrap_or_else(|| PathBuf::from(".myownmesh-rev"));
    if let Ok(rev) = std::fs::read_to_string(&rev_file) {
        let rev = rev.trim();
        if !rev.is_empty() {
            println!("cargo:rustc-env=MYOWNMESH_PIN={rev}");
        }
    }
    println!("cargo:rerun-if-changed={}", rev_file.display());
}
