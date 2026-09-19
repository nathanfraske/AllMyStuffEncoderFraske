# Experimental RISC-V Serve and OpenH264

**Experimental checkpoint.** The full `allmystuff-serve` binary now links for
`riscv64gc-unknown-linux-musl` with `--no-default-features`. Under QEMU,
684 root/node tests and a degraded Serve lifecycle fixture passed. The
standalone unoptimized codec proof still aborts on a 32-bit shift check;
a diagnostic relink failed before execution. That investigation is deferred,
with its inputs and failures preserved, while work returns to modularization.

This is generic Linux/musl emulation, not qualification of a board, firmware,
hardware codec, performance or production deployment. Doctests, compatible
real Mesh sessions and target child/self-execution remain unverified. The
earlier Windows checks and desktop/mobile limitations are retained below.

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
produced a linked executable. These build commands did **not run** its
assertions; the later execution failure is recorded in the checkpoint below.

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
regenerable build/cache output. That build command and build runner did not
execute it.

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

No full RISC-V Serve build or emulator execution was performed in that
Windows integration slice. The later results follow.

## RISC-V build and emulation checkpoint

The three cross-builds used source
`d15b0693542c74cfc6c938fda1cc1119bbfc3668`. The reviewed namespace/lifecycle
recipes were integrated at `6602922dfa7c4eba937e1c8fa4d8eda200da010b` before
execution; they selected the earlier artifacts by exact build identity and
hash. Windows Rust/Cargo 1.97.1 and Zig 0.16.0 produced generic RV64GC/musl
artifacts. Ubuntu 24.04 under WSL2 supplied QEMU 8.2.2 (`rv64`), package
`1:8.2.2+ds-0ubuntu1.18`. These environment values describe this experiment.

| Gate | Durable run | Terminal result |
| --- | --- | --- |
| Full no-default Serve build/link | `e160fc0d-dddd-49e0-b9b2-8c4308b96a00` | Exit 0, 224.797 s. |
| Root workspace test compilation | `e90ba069-d142-4f19-b111-656ad34d98ae` | Exit 0, 71.064 s; 25 test harnesses plus one ordinary executable. |
| No-default node workspace test compilation | `a8a700e6-0a5b-43b5-954d-7d14f5436bf4` | Exit 0, 26.769 s; Serve, pixels and node test harnesses. |
| Root tests under QEMU | `73967166-881c-4f16-8a33-9955ec5929e1` | Exit 0, 14.041 s; 370 passed across 25 harnesses. |
| No-default node tests under QEMU | `4c2fd5fc-bc5c-43ed-a84e-8de7bc9b7f5b` | Exit 0, 15.844 s; Serve 5 + pixels 11 + node 298 = 314 passed. |
| Degraded Serve lifecycle under QEMU | `afa73dc4-ad47-4380-931b-0beb62b2e0b4` | Exit 0, 25.664 s; real local IPC, shutdown and restart checks passed. |
| Original standalone codec proof under QEMU | `33c42e08-3bd0-45b6-bfec-4c9d851c55cc` | Failed, harness exit 1, 3.597 s; target aborted with signal 6. |
| Scratch location-diagnostic relink | `716d28f1-50d5-4649-8253-c1d785740c63` | Failed, wrapper exit 1, 44.330 s; Cargo exit 101 at unsupported `--wrap` linker argument; no target executed. |

The linked Serve is a 46,212,632-byte little-endian RISC-V ELF, static PIE
with no `PT_INTERP` or `DT_NEEDED`, SHA256
`358bf7399e0f1c7d91243a2dfbc7c78f12be36ca32e71b01a9cae0b73822553d`.
The [build recipe](../../../scripts/riscv/README.md) records exact commands,
locked metadata, tool/environment identities and artifact hashes. Compile
success is separate from test execution; build logs retain Zig/libc++ and
linker warnings. The ordinary `dump_kvm_fixtures` artifact was not a test
harness and was excluded from libtest execution.

The **684 executed tests** had zero failed, ignored, measured or filtered
tests. Runtime command stderr was empty. Each executable used fresh native
user/mount/network/PID/IPC namespaces, private temporary mounts and proc,
loopback-only networking, disposable state and an empty helper PATH. The
[isolation recipe](../../../scripts/riscv/emulator-isolation.md) records these
checks and per-executable results. The node suite includes real software
encode/decode and resolution-change tests; host-gated capture/encoder tests
and Rustdoc tests are not part of this no-default execution result.

The lifecycle fixture used native `/usr/bin/false` as an explicitly failing
Mesh daemon. Version `0.2.121`, installed runtime owner, disconnected link
status and socket mode `0600` matched expectations. A duplicate exited 0;
both initial and restarted Serve exited 0 on SIGTERM, removed the socket,
rebound using the same disposable state and left no owned processes. This
does not establish successful Mesh transport, a RISC-V daemon child or
supervisor/self-execution under user-mode QEMU.

The unchanged standalone proof (`0dc6386f...`, full hash above) began its
synthetic encode/decode test, then reported `shift exponent 32 is too large
for 32-bit type 'uint32_t'` and aborted without a passing summary. It used
the standalone default unoptimized profile and a different RGB/conversion
input from the node tests; the node manifest optimizes OpenH264 packages at
level 3. Passing node tests therefore do not resolve that failure. The exact
codec source location and a fix remain unproven. The separate diagnostic
attempt preserved source, test, lock and proof identities but failed to link;
no diagnostic target ran and no codec fix or check suppression was applied.
Further diagnosis and execution are deferred at this checkpoint.

Preparation failures also remain recorded: offline target metadata first
missed cached `inotify` sources, then locked resolution populated the missing
cache without changing pins. QEMU setup `4bf9e59d-8cd1-43f2-9bf5-13774985d2f8`
exited 141 from the early-exit `awk` pipeline; exact-version setup
`9230a1a6-0d91-4da6-9d08-d92d9ca6fdc2` installed QEMU but exited 1 at CPU help.
The installed package/model was verified separately in
`1eb45a4d-4765-4e58-8593-0e4eca6ea4ae`; narrow setup-script corrections retain
these failures rather than counting them as successful setup runs.

The [node manifest](../../../node/Cargo.toml) still includes software H.264,
Opus, bundled SQLite, TLS and the application runtime without default
features. The successful full link includes `opus` 0.3.1 / `audiopus_sys`
0.2.2, `cmake` 0.1.58, `libsqlite3-sys` 0.30.1 and `ring` 0.17.14; selecting
the Serve binary does not select independent capability modules.

The node's build script only records its target and daemon pin. Running
[Serve](../../../node/src/bin/serve.rs) still starts the legacy daemon and
constructs the full application state. Without `host`, capture and input
modules use stubs. This build does not supply an
embedded board's encoded-video source or USB HID adapter. The updater's
current platform selector also returns `unknown` for RISC-V; release asset
selection remains separate work.

## Qualification still required

- Resolve the standalone codec abort separately if this experiment resumes;
  do not treat the optimized node passes as a fix or disable checks to pass.
- Establish cross-target doctests, compatible real Mesh sessions and target
  child/supervisor/self-execution independently of the degraded fixture.
- Extend the recorded Windows regression coverage to other supported hosts
  and resolve the desktop/mobile dependency-cache limitations.
- Verify the actual board's ISA, libc, vendor ABI and firmware before board
  execution. Exercise encode/decode, malformed streams, resize/retune,
  paced slices, keyframe/recovery behavior and lifecycle cleanup.
- Measure target throughput, latency, CPU and memory. Preserving source
  and producing an ELF do not establish correctness or usable performance.
- Keep any later optional-codec/provider work separate: already encoded
  hardware access units can use application metadata/pacing semantics, but
  provider admission, buffer ownership and control/stop contracts are not
  implemented by this patch. Mesh transport and identity authority are
  unchanged; no new Mesh V1 API is assumed.
