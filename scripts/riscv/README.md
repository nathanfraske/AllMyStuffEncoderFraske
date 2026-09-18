# Experimental RISC-V compile recipe

`build.py` compiles and links from the reviewed Windows host into
`riscv64gc-unknown-linux-musl`. It never executes the resulting programs,
installs tools, changes dependency versions, or configures an emulator.
Generic Linux/musl with the RV64GC instruction set and LP64D ABI is a toolchain
experiment; it is not qualification of a particular board or firmware.

The first gate is the real `allmystuff-serve` binary with the existing
`--no-default-features` configuration. This still includes the node engine,
software OpenH264/Opus, inventory, bundled SQLite and TLS. Selecting this
binary does not select independent application capabilities. Desktop capture,
audio I/O, injection and PTY hosting remain behind the existing `host` feature.

## Reviewed inputs

- Rust/Cargo 1.97.1 on `x86_64-pc-windows-msvc`, with the RISC-V musl target
  already installed; Python 3.11 or later for this driver.
- Zig 0.16.0, and the existing Visual Studio Build Tools CMake and Ninja.
  The defaults refer to the local reviewed installations; explicit `--zig`,
  `--cmake` and `--ninja` paths can select reviewed copies. Their hashes and
  the Rust version are recorded for each attempt.
- An unchanged, locked dependency graph whose target sources are cached.
  The driver is offline. Missing cached sources are a preparation failure,
  not permission to refresh locks or install another dependency version.
- The vendored OpenH264 0.9.6 source and exact two-line experimental patch,
  checked by `vendor/verify_openh264.py` before compiling.

The Zig C/C++ target and CPU arguments preserve the successful isolated
OpenH264 experiment: `--target=riscv64-linux-musl`,
`-mcpu=generic_rv64+m+a+f+d+c+zicsr+zifencei`, and `-mabi=lp64d`.
The linker uses Zig C++/libc++ and Rust `link-self-contained=no`, so this
experiment uses Zig's musl and C++ runtime together with Rust's installed
target libraries. A linked ELF still needs emulator and runtime testing.

Opus uses its unchanged cached CMake source. The toolchain file explicitly
selects Linux/RISC-V and the target compiler, archive tools and installed
Ninja. This is necessary because `cmake` 0.1.58 does not automatically pass
the compiler for a non-MSVC target on a Windows host. Real compiler/linker
probes remain enabled; no configuration result is forced to success and no
Opus codec options are changed. Target-specific `cc` settings also reach the
unchanged bundled SQLite and ring build scripts. Static Opus and bypassing
host pkg-config prevent accidental use of a Windows library.

## Manager commands

Run through the durable run service from the integration checkout after
independent recipe review. Replace `<reviewed-commit>` with the exact current
40-character commit. Each attempt needs a new label, including retries after
a diagnosed terminal failure; never replay an ambiguous run.

```text
python scripts/riscv/build.py --kind serve --attempt serve-01 --expected-head <reviewed-commit>
python scripts/riscv/build.py --kind root-tests --attempt root-tests-01 --expected-head <reviewed-commit>
python scripts/riscv/build.py --kind node-tests --attempt node-tests-01 --expected-head <reviewed-commit>
```

The corresponding Cargo commands are:

```text
cargo build --manifest-path node/Cargo.toml --locked --offline --no-default-features --target riscv64gc-unknown-linux-musl --bin allmystuff-serve -j 2 --message-format=json-render-diagnostics
cargo test --manifest-path Cargo.toml --locked --offline --target riscv64gc-unknown-linux-musl --no-run --workspace -j 2 --message-format=json-render-diagnostics
cargo test --manifest-path node/Cargo.toml --locked --offline --no-default-features --target riscv64gc-unknown-linux-musl --no-run --workspace -j 2 --message-format=json-render-diagnostics
```

Test compilation is a separate gate from executing tests. `--no-run` does
not execute unit, integration, binary or documentation tests; it does not
establish that cross-target doctests are runnable. The emulator/test-isolation
recipe must select and execute the resulting test programs explicitly and
record all exclusions and failures. Starting Serve also needs an independently
reviewed process/IPC environment and compatible Mesh runtime.

Both test commands select every member of their respective workspace,
including the node workspace's `pixels` member. Test runners must select
artifacts whose `profile.test` is `true`: Cargo can also report ordinary
executables, which need a separate lifecycle harness rather than libtest
arguments. Inspect the ELF interpreter and shared dependencies before assuming
an artifact is statically linked.

All generated files stay under the checkout's ignored `target`: Cargo outputs
use that existing root, Zig caches use `riscv-zig-cache`, temporary files use
`riscv-tmp`, and per-attempt evidence uses `riscv-runs/<attempt>`. This permits
host/target artifacts to be reused across serialized gates without overwriting
prior attempt records or the retained isolated codec proof. No cleanup is
performed by this driver.

Each attempt records its exact commit, recipe/tool hashes, child environment
overrides, all four lock hashes, resolved metadata, Cargo JSON output, and
linked executable paths/hashes/ELF identity in `artifacts.json`. Target binaries
must be little-endian ELF64 for RISC-V; this identity check is not an execution
or board-ABI test. Compiler stderr remains in the durable run log. The source
commit and all represented locks must remain unchanged on completion.

This recipe is prepared for central validation. Its presence does not claim
that the full Serve build, tests, emulator setup or hardware support passed.
