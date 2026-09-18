# Experimental OpenH264 support for RISC-V

The pinned OpenH264 source compiled and linked for generic RISC-V Linux/musl
after a two-line architecture-recognition correction in its Rust wrapper.
This is an isolated codec build result. The integrated dependency now passes
Windows node compilation and 33 focused regression executions. Desktop/mobile
metadata checks remain blocked by offline inputs, and the full
`allmystuff-serve` RISC-V cross-build remains **pending**. No RISC-V executable
was run in the checks recorded here; firmware, hardware, video performance
and production deployment remain unqualified.

## Patch and integration boundary

The application keeps `openh264` **0.9.3** and `openh264-sys2` **0.9.6**. The
[vendored wrapper](../../../vendor/openh264-sys2-0.9.6/build.rs) adds only:

```diff
 enum TargetArch {
     ...
+    Riscv64,
     ...
 }
 ...
+    "riscv64" => TargetArch::Riscv64,
```

All 343 files from the published sys2 package are retained; only `build.rs`
changes. The codec implementation, existing architecture branches, assembly
selection, encoder settings and application callers remain unchanged. RISC-V
uses the existing generic C++ path, with no x86 NASM or ARM assembly selected.
`OPENH264_NO_ASM` alone cannot repair the original wrapper: target parsing
rejects the architecture before that option is consulted.

The cumulative integration uses a local Cargo patch in the separate node,
desktop and mobile workspaces. It preserves their existing codec features,
mobile receive/audio roles and unrelated Mesh overrides. It does not change
the global Cargo cache or upgrade registry dependencies. The root library
workspace does not consume OpenH264. The central integration results below
record the validated Windows scope and the remaining workspace limitations.

## Retained isolated experiment

The experiment used application baseline
`e6d7c15a38245463457ca168ac552f6fd1e3aaf3`, with separate scratch consumers of
the original and patched packages. Each attempt used the same synthetic
64-by-64 RGB-to-H.264 encode/decode test source. Only the corrected recipe
produced a linked executable. Its assertions were **not run**.

| Durable run | Terminal result | Duration | Evidence |
| --- | --- | --- | --- |
| `e4a6239a-4201-4760-800d-6637d9995476` | Failed, exit 101 | 9.573 s | Original wrapper rejects `riscv64` during target selection. |
| `0c705acd-2496-48cc-8edc-3dec312716a3` | Failed, exit 101 | 6.710 s | Patched wrapper reaches the scalar path; Zig rejects the initial `-march=rv64gc` CPU argument. |
| `22422c83-9b5c-4fe0-b562-47a8b4d65245` | Succeeded, exit 0 | 65.113 s | Corrected toolchain recipe compiles and links the encoder/decoder consumer. |

The second failure concerns the experimental compiler invocation. It is not
evidence of a C++ source failure. Both failures remain retained alongside the
successful attempt; the original negative control was not replayed.

The successful command, from the isolated consumer, was:

```text
cargo test --locked --offline --target riscv64gc-unknown-linux-musl --test codec --no-run -j 2 -vv
```

The manager used Windows x64, Rust/Cargo 1.97.1 and Zig 0.16.0. Target-specific
C/C++ wrappers selected `riscv64-linux-musl`,
`-mcpu=generic_rv64+m+a+f+d+c+zicsr+zifencei` and `-mabi=lp64d`. The linker
used the Zig C++ driver, `CXXSTDLIB=c++` and Rust `link-self-contained=no`.
Cargo dev/test debug information was disabled, with two build jobs. These
settings describe the tested generic toolchain; they are not a vendor SDK
or a globally installed repository configuration.

The runner checked all 343 packaged file identities, its 18 reviewed inputs,
the selected source feature and all 23 resolved package records. Registry
names, versions, sources and checksums matched the frozen node lock; only
the local consumer/sys2 substitution and pruning were permitted. The
resolved consumer lock, frozen node lock and project root lock remained
unchanged during the successful locked build.

| Retained identity | SHA256 |
| --- | --- |
| Published sys2 0.9.6 archive | `fa9e072e9b270f3b291c80488dc160abc31ecc214ab3bfde937213cfd8c83b32` |
| Original `build.rs` | `cf01f861c8cb2ab1e4aae8d741a0aef2a43baa635deb1bbc81d90cf5fd278838` |
| Patched `build.rs` | `88b25e67b70b21a056a56494d532326076c6de4e87e30f4163b953adbd307f7e` |
| Synthetic consumer test | `2405e4e4c87ef2c65e341682b8084d5cf5a90889983eb0295763030047fdc631` |
| Corrected runner | `e39a4a1754c44b4019eb0a15bcf88f22d7bb41f82a1ed5c760e18aa50ea1bbdc` |
| Corrected input manifest | `5404b25f15468f1a1371acdc6dd29351868f78e0d881cdbfdaadc99bc9497351` |
| Resolved isolated consumer lock | `723e059f7469b2549dcae35d8d5f6c344e65a100a663aa3fc2de6b6feef6790f` |
| Linked test ELF | `0dc6386fa8000a87f38d9ca18380f4fa4aea3fbb34eed73c62d5cce3dee3f2d6` |

The linked artifact is 23,792,376 bytes: little-endian ELF64, machine 243,
type 3, flags 5. Its hash and header were independently checked against the
retained run. The manager retained a verified copy as
`proof/codec-riscv64gc-linux-musl` beside the experiment inputs before removing
regenerable build/cache output. The command and runner never execute it.

Successful logs are complete: stdout 136,667 bytes and stderr 6,080,762 bytes.
Warnings remain: unused-result/import/macro diagnostics from `winapi-util`,
`nasm-rs` and `safe_arch`; 339 `wide` diagnostics about `must_use` on impl
methods; and a Rust `linker_messages` warning containing extensive repeated
Zig libc++ nullability diagnostics. This was not a warning-free build.

## Application integration verification

The vendored dependency and this record were integrated at
`723bf8f5664ab56ff842f4b7c67f1bea3bd7dfd1`: reviewed source commit
`45eebd32551b7364ed4a1eabdf372e099904c24e` became `bfd4db6`, and the
documentation commit `8aa5a0e557b3c55ec761e255580602f1c869d9b1` became
`723bf8f`. The following central records use that exact source on Windows x64.
All listed terminal streams were complete and untruncated.

| Check | Durable run | Result |
| --- | --- | --- |
| Packaged archive and inverse-patch proof | `f995cd8a-4f7f-4319-8e85-bbe4c36ad349` | Passed, exit 0; all 343 packaged files, only the two build-script additions. |
| Manifest, all-lock and raw Git-index proof | `9323afa2-e6a7-45c1-8cf8-c954cb45e248` | Passed, exit 0; all four manifests, 280/604/801/732 root/node/desktop/mobile lock records, and 343 package blobs. |
| Root `cargo fmt --all --check` | `c10006b5-4438-4292-9873-1472461c9bea` | Passed, exit 0. |
| `cargo fmt --manifest-path node/Cargo.toml --check` | `6482b951-f328-4a22-b72e-8ac080c61713` | Passed, exit 0; no `--all`, preserving upstream package bytes. |
| Root metadata | `a0b01a4d-f405-4820-a5cb-5da1eff59568` | Passed, exit 0; OpenH264 absent from the root workspace graph. |
| Node metadata | `f9959a00-a251-4050-96bb-9a21aa2ba0d0` | Passed, exit 0; local sys2 0.9.6 with `source`, excluded from workspace membership. |
| Desktop metadata | `ae274318-3e5e-47ff-bf77-ac2bb5e76f5a` | Failed, exit 101; `trash` was unavailable in the offline registry cache. |
| Mobile metadata | `2c16fe0b-09f4-48a7-8da5-721ce8eddcc7` | Failed, exit 101; the pinned `interceptor` Git checkout was unavailable offline. |

The archive proof ran `python vendor/verify_openh264.py --archive <cached
openh264-sys2-0.9.6.crate>`. The read-only integration helper, SHA256
`dd4795396f57ade829e70483eb90a2fa4b6a0f63848f53937ad94db16eae69e9`,
ran with `--repo <manager-worktree> --index`. Exact invocation paths are
retained in the corresponding run records.

Each metadata wrapper ran `cargo metadata --manifest-path <manifest>
--locked --offline --format-version 1 --filter-platform
x86_64-pc-windows-msvc` and verified that its workspace lock stayed unchanged.
The four manifests were `Cargo.toml`, `node/Cargo.toml`,
`gui/src-tauri/Cargo.toml` and `gui/mobile/Cargo.toml`. The two offline
failures occurred before successful graph validation; neither is a GUI or
mobile build pass. They are separate from the previously recorded local
package version drift (`0.2.118`/`0.2.119`) in those locks. This integration
preserves all prior registry/Git resolutions and changes only the sys2 source
selection in the three consuming locks.

The corrected native runs also use `723bf8f`. Every row below succeeded with
exit 0 and complete, untruncated streams. Durations include compilation;
stdout/stderr columns are retained byte counts.

| Native check | Durable run | Duration | Result | stdout / stderr |
| --- | --- | --- | --- | --- |
| Default all-targets Clippy, warnings denied | `527dd321-c5bb-4f0f-aacc-a23fc5a3578c` | 94.280 s | Passed | 0 / 11,549 |
| No-default all-targets check | `38148558-a498-4008-be5e-9c08f73e6fcc` | 47.916 s | Passed | 0 / 1,726 |
| AU identity | `848fd2c1-c84b-4963-8377-740f1941b493` | 121.847 s | 5 passed | 526 / 10,432 |
| Slice splitter | `1c3aff48-40d2-4ae9-813c-c543a7f4285d` | 0.874 s | 1 passed | 192 / 152 |
| Software H.264 paced slices | `0347954a-7f3e-4ed7-8e92-4bc5308eb8f9` | 0.963 s | 1 passed | 190 / 617 |
| Default control client | `1949f2a4-bf2b-41f8-94c8-d03d091e63bb` | 9.801 s | 13 passed | 1,315 / 152 |
| No-default control client | `17a24926-e746-44d1-8105-2aa2c07c555f` | 53.938 s | 13 passed | 1,315 / 1,790 |

The exact Cargo arguments were:

```text
cargo clippy --manifest-path node/Cargo.toml --locked --offline --all-targets -- -D warnings
cargo check --manifest-path node/Cargo.toml --locked --offline --no-default-features --all-targets
cargo test --manifest-path node/Cargo.toml --locked --offline --lib au_identity
cargo test --manifest-path node/Cargo.toml --locked --offline --lib video::tests::splitter_cuts_only_at_slices_and_partitions_exactly -- --exact
cargo test --manifest-path node/Cargo.toml --locked --offline --lib video::tests::openh264_accepts_paced_slice_chunks_incrementally -- --exact
cargo test --manifest-path node/Cargo.toml --locked --offline --lib control_client::tests::
cargo test --manifest-path node/Cargo.toml --locked --offline --no-default-features --lib control_client::tests::
```

These are 33 test executions: 20 with defaults and 13 without, with no failed
or ignored tests. Clippy and `check` compile their targets; they do not execute
tests. The software H.264 test retains three encoder configuration warnings:
the 4,096-byte max-NAL value takes precedence over the slice constraint, and
AdaptiveQuant and BackgroundDetection are disabled for screen content.
There is no warning-free claim for that test. No broad node/root suite, live
Serve, GUI, device or other-OS test was run in this integration slice.

Initial native Clippy `d6fd828d-0049-49d3-9c1f-343d3bd49855` and no-default
check `f79e2b10-740e-4c60-b0dd-c2fe73d79e62` failed with exit 101 in
`audiopus_sys`'s CMake compiler test. MSBuild reported `MSB6003` and
`DirectoryNotFoundException` while processing `cmTC_*.tlog` tracking output
through `Directory.GetFiles`, `ExpandWildcards`, `DeleteFiles` and
`PostExecuteTool`. These are path-sensitive CMake/MSBuild environment
failures, resolved by restoring the prior output location. The exact
filesystem mechanism remains unproven; the diagnostics establish neither a
missing linker nor a specific path-length limit, and contain no OpenH264
source failure.

The manager reports the only correction was `CARGO_TARGET_DIR`, from the
manager worktree's `target/integration` to `target`. Commands and tracked
source stayed unchanged. Command/source identities, changed environment
hashes and terminal outcomes are retained; the specific environment values
are manager-supplied. The Windows recipe used Rust/Cargo 1.97.1,
`CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`,
`CARGO_INCREMENTAL=0`, `CMAKE_POLICY_VERSION_MINIMUM=3.5`, and
`CMAKE=C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe`.

Five original follow-on runs were canceled after that environment failure:
`835df173-c9a9-46d5-9295-8b53aeabfa1f`,
`c40762e4-a875-41f6-a3f2-3d6146174e98`,
`5411f7f9-c54f-460a-9dbb-0b2b44e4e5ae`,
`04da39a0-a49f-4908-857b-2e9ba1d19ba5` and
`1f1c4d0d-2314-4d85-acc0-76c51cffbac5`. The first reached compilation;
none executed tests. These cancellation records add no test coverage and
are separate from the successful replacements above.

No full RISC-V Serve build or emulator execution was performed in this
integration verification slice. Those remain the next gates.

## Next gate: the actual Serve binary

After workspace resolution and the supported-host regression checks, the
next compile/link gate is the full binary with desktop hosting disabled:

```text
cargo build --manifest-path node/Cargo.toml --locked --offline --no-default-features --bin allmystuff-serve --target riscv64gc-unknown-linux-musl
```

This command requires a reviewed cross-compilation environment, including
target C/C++ compilers, archiver, linker and CMake configuration. It must not
be read as a successful run or as an instruction to launch Serve. The manager
owns durable execution and records any failure at its actual stage.

The [node manifest](../../../node/Cargo.toml) still includes software H.264,
Opus, bundled SQLite, TLS and the application runtime without default
features. The pinned closure includes `opus` 0.3.1 / `audiopus_sys` 0.2.2,
`cmake` 0.1.58, `libsqlite3-sys` 0.30.1 and `ring` 0.17.14. Opus may build
through CMake; SQLite and ring include native C compilation. Their compiler,
headers, libc, archive and link paths remain unqualified by the isolated
OpenH264 result. The inspected paths do not establish another explicit
RISC-V rejection: CMake accepts Linux with the target processor, ring has a
generic LP64/endian path, and SQLite uses its bundled C source. None of that
is a successful application build, and Opus is not a demonstrated failure.

The node's build script only records its target and daemon pin. Running
[Serve](../../../node/src/bin/serve.rs) still starts the legacy daemon and
constructs the full application state. Without `host`, capture and input
modules use stubs. A successful binary therefore would not supply an
embedded board's encoded-video source or USB HID adapter. The updater's
current platform selector also returns `unknown` for RISC-V; release asset
selection needs separate work even if compilation succeeds.

## Qualification still required

- Extend the recorded Windows regression coverage to other supported hosts
  and resolve the desktop/mobile dependency-cache limitations.
- Verify the actual board's ISA, libc, vendor ABI and firmware before target
  execution. Exercise encode/decode, malformed streams, resize/retune,
  paced slices, keyframe/recovery behavior and lifecycle cleanup.
- Measure target throughput, latency, CPU and memory. Preserving source
  and producing an ELF do not establish correctness or usable performance.
- Keep any later optional-codec/provider work separate: already encoded
  hardware access units can use application metadata/pacing semantics, but
  provider admission, buffer ownership and control/stop contracts are not
  implemented by this patch. Mesh transport and identity authority are
  unchanged; no new Mesh V1 API is assumed.
