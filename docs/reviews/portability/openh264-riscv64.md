# Experimental OpenH264 support for RISC-V

The pinned OpenH264 source compiled and linked for generic RISC-V Linux/musl
after a two-line architecture-recognition correction in its Rust wrapper.
This is an isolated codec build result. Validation of the vendored dependency
in the application workspaces and a full `allmystuff-serve` cross-build remains
**pending**. No RISC-V executable was run, and no firmware, hardware, video
performance or production deployment is qualified by this result.

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
workspace does not consume OpenH264. Workspace resolution and supported-host
regressions must still be verified centrally after integration.

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

- Execute the existing software H.264, AU metadata and control-client
  regressions with the integrated dependency on supported hosts.
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
