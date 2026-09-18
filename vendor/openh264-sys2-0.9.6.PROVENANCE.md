# Experimental OpenH264 RISC-V build patch

This directory preserves the published `openh264-sys2` 0.9.6 package, with exactly
two lines added to `build.rs`: the `Riscv64` architecture variant and the
`"riscv64"` parser arm. This selects the existing generic C++ path. No codec
source, flags, Rust API, default feature, or existing architecture branch changed.

This is an **experimental compiler unblock**, not a claim of AllMyStuff Serve,
firmware, runtime, correctness, or performance qualification on RISC-V. The
isolated generic Linux/musl encode/decode test executable cross-linked successfully
without being run. Its evidence and remaining gates are recorded in the
[portability note](../docs/reviews/portability/openh264-riscv64.md).

## Source identity

| Item | Exact value |
| --- | --- |
| Registry package | `openh264-sys2` 0.9.6 |
| Archive | [crates.io 0.9.6 download](https://static.crates.io/crates/openh264-sys2/openh264-sys2-0.9.6.crate) |
| Archive SHA256 | `fa9e072e9b270f3b291c80488dc160abc31ecc214ab3bfde937213cfd8c83b32` |
| Packaged VCS revision | `b8c508d223c9cc2c40dc9c27508b7b321c8e44f0`, path `openh264-sys2` |
| Original `build.rs` SHA256 | `cf01f861c8cb2ab1e4aae8d741a0aef2a43baa635deb1bbc81d90cf5fd278838` |
| Patched `build.rs` SHA256 | `88b25e67b70b21a056a56494d532326076c6de4e87e30f4163b953adbd307f7e` |
| Packaged files | 343; every file except `build.rs` is byte-identical |

The normalized manifest declares `BSD-2-Clause`. All packaged notices remain,
including [Cisco's source license](openh264-sys2-0.9.6/upstream/LICENSE) and the
[reference binary license](openh264-sys2-0.9.6/tests/reference/BINARY_LICENSE.txt).
The vendored package does not inherit the AllMyStuff root package's MIT declaration.

The adjacent [provenance JSON](openh264-sys2-0.9.6.provenance.json) records each
original file's SHA256. The [patch](openh264-sys2-0.9.6-riscv64.patch) contains the
complete source change. Local Git attributes preserve the package's original
line endings and whitespace; do not reformat the vendored sources.

## Reproduce the source check

From the repository root, using Python 3:

```sh
python vendor/verify_openh264.py
python vendor/verify_openh264.py --archive /path/to/openh264-sys2-0.9.6.crate
```

The second form additionally verifies a supplied original archive. Neither command
downloads dependencies, invokes Cargo, or executes codec code. The script checks
all file identities and proves that removing only the two added lines reconstructs
the original `build.rs` bytes.

The three consuming workspaces (`node`, `gui/src-tauri`, and `gui/mobile`) each
declare the same local `[patch.crates-io]`; Cargo does not inherit a dependency's
workspace patch. Their locks retain version 0.9.6 and its dependency edges, removing
only the registry source/checksum for this now-local package. Existing desktop and
mobile local-version lock drift is outside this change. The root library workspace
does not consume OpenH264 and explicitly excludes this package from its membership.

The packaged `Cargo.lock` is retained for byte provenance; the consuming workspace
locks control actual application builds. No source-cache mutation or external fork
is required. A future upstream replacement needs a separate reviewed version/source
change and validation; this experimental patch should not silently track a release.
