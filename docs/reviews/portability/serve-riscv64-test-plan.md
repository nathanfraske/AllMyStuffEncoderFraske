# Serve RISC-V test inventory and execution acceptance

Source audit: `723bf8f5664ab56ff842f4b7c67f1bea3bd7dfd1`, 2026-09-18.
This report records source inspection and a proposed execution contract. The
reviewer ran no compiler, test executable, installer, or emulator. The existing
[OpenH264 experiment](openh264-riscv64.md) linked a codec test ELF; that result
does not establish execution or a passing Serve build.

The immediate configuration is `riscv64gc-unknown-linux-musl` with node default
features disabled. It is a validation step, not a new product mode or a claim
that the default host build works. C1 owns the cross-build recipe; C2 owns the
emulator recipe. Following the staffing handoff, C2 owns final review of C1's
recipe and C1 owns review of C2's recipe.

## Required suites

[Root Cargo.toml](../../../Cargo.toml) has 18 members and excludes `node`.
Running only the root workspace cannot test the node or Serve. Running only
node tests does not run the tests of its library dependencies.

| Root members | Relevant existing coverage and limits |
| --- | --- |
| `allmystuff-inventory-model` | Model JSON fixtures and public API compatibility. |
| `allmystuff-video-metadata` | Production H.264/HEVC AU identity marker insertion, parsing, removal, malformed/truncated data and byte vectors. These tests remain available independently of host video. |
| `allmystuff-video-pacing`, `allmystuff-frame-timing`, `allmystuff-byte-queues` | Production pacing policy, timing and bounded queue logic, including public API tests where present. |
| `allmystuff-update-policy` | Production update policy and public API vectors; no live update required. |
| `allmystuff-inventory` | Linux parsers, shared classification/report logic, model identity and real environment scan smoke tests. Scans inspect the emulator environment's kernel/filesystem view. |
| `allmystuff-graph`, `allmystuff-protocol`, `allmystuff-bridge`, `allmystuff-session` | Application graph/policy, payload/control wire formats, capability mapping, session effects, media/audio framing and reassembly. These are not live Mesh transport tests. |
| `allmystuff-updater` | Policy, asset naming, temporary state, version stamps, signatures, archive extraction and pending-artifact handling. No successful release download/install is established. |
| `allmystuff-service` | Service text/argv construction and status parsing, with temporary files and environment handling. No live service-manager registration is established. |
| `allmystuff-term` | CLI/target/attachment parsing and terminal protocol logic; separate from node PTY hosting. |
| `allmystuff-mobile-core` | Platform-neutral connection/control/media logic; not a mobile OS runtime qualification. |
| `allmystuff-cec-protocol`, `allmystuff-cec-consent` | Wire/media/identity and consent-store behavior, including temporary filesystem state. |
| `allmystuff-cli` | Include Cargo's binary test target; no dedicated test functions were found in this audit. A zero-test harness is not CLI process coverage. |

The separate [node workspace](../../../node/Cargo.toml) needs its library and
`allmystuff-serve` binary test harnesses. With defaults disabled:

| Area | What is actually exercised or omitted |
| --- | --- |
| Software video | `video_decode.rs:1329` encodes actual OpenH264 frames, feeds `DecodeBridge`, verifies RGBA dimensions/length/alpha and stops the route. `:1388` encodes two resolutions and verifies both decoded geometries. Both remain enabled; neither substitutes a fake codec. |
| Receive-side media | `control_client.rs:951-1371` tests buffered media, paced AU assembly, backpressure, gaps and recovery ordering. Mesh, session and protocol tests cover additional application media semantics. |
| Application state | Files/Fleetfiles, folders, namespace, operations, ownership, shares, consent, canvas, sites, service profiles and storage policy retain their real implementations. Several tests use actual temporary files or SQLite, while others use in-memory stores. |
| Local IPC | Node frame codec tests and the Unix owner-only socket test (`node_control.rs:3075`) execute real framing/socket code. Existing unit tests do not start the Serve process and drive its command dispatcher. |
| Serve harness | Five Linux-applicable tests cover help/verb dispatch, state-home path derivation and supervisor parent policy (`bin/serve.rs:1233-1319`). The additional Windows helper test is target-excluded. None establishes startup/shutdown behavior. |
| Host features | `lib.rs:47-186` excludes real capture, host video, clipboard, input injection, terminal hosting, wake, and audio-device implementations. Same-API stubs are used where declared. There are no dedicated tests in those stub files. Windows/macOS backends are also target-excluded. |

In particular, `video.rs:5696` (slice partitioning), `:5832` (incremental
OpenH264 decoding of paced slice chunks), send-side encoder/rate/recovery
integration, and `mesh.rs:23134` (real PTY multi-attachment) are absent from
this node configuration. AU marker implementation coverage is **not** wholly
absent: it moved into `allmystuff-video-metadata` and is in the root suite.
Audio framing tests and linked Opus do not prove real audio I/O or every codec
path has executed. Host-enabled testing needs its own build/runtime evidence.

There are executable Rust doc examples in byte-queues, graph, video-metadata,
frame-timing, update-policy and mobile-core. Inventory's example is explicitly
`no_run`. Track doctest compilation/execution separately; transferred libtest
ELFs from `cargo test --no-run` do not account for it. Exact counts must come
from the target executables' `--list` and Rustdoc output, not source attribute
counts across mutually exclusive platform/feature branches.

## State and execution isolation

| Source anchor | Required isolation |
| --- | --- |
| `node/src/daemon_spawn.rs:940,954` | Tests set and then remove `MYOWNMESH_HOME`, without restoring an inherited value. Their private mutex does not guard unrelated tests. |
| `crates/allmystuff-updater/src/lib.rs:2194-2390` | Tests change `ALLMYSTUFF_HOME`, `PATH`, GUI/Serve overrides and temporary update state. Several remove the home override afterward. |
| `crates/allmystuff-service/src/lib.rs:2351-2366` | A test temporarily removes both state-home overrides. |
| `node/src/mesh.rs:2117-2173,22070,23072,23087,23219` | `Mesh::new` loads default application stores even in tests that never connect to a daemon. A disposable fallback HOME remains necessary after overrides are removed. |
| `node/src/persist.rs`, `operations.rs`, `namespace.rs`, `ownership.rs`, `shares.rs`, `canvas.rs`, `networks_store.rs`, and CEC consent tests | Actual temporary file/directory creation, persistence, reopening and cleanup. Use fresh TMPDIR/TMP/TEMP per executable. |
| `crates/allmystuff-inventory/src/lib.rs:302,344` | Real scans read `/proc`, `/sys`, storage/network information and may invoke `nvidia-smi` if NVIDIA hardware is exposed (`:260`). Limit exposed devices/helpers and qualify the resulting inventory evidence. |
| `node/src/bin/serve.rs:287-291,552,587,618` | Normal startup applies pending updates, may spawn/update a daemon, starts application state/watchers and an unattended update loop. Start with empty stores and explicit update/daemon controls. |

Run each test executable in a new disposable environment with
`--test-threads=1`. Serialize within each executable because environment
mutation and process-global state are shared. Preserve failures and internal
timeouts: decoder tests have 2/10-second receives, and the runtime-spawn test
has a 5-second receive. A slow emulator failure remains a failure until
diagnosed; a larger external deadline does not change those assertions.

QEMU user mode is not a guest OS or a filesystem/network sandbox. C2's outer
runner must establish and verify native mount, PID, IPC and network isolation
before target execution, with private propagation, a private `/proc` for the
PID namespace, private `/tmp`, `/var/tmp` and `/run`, and loopback only. Keep
logs/results outside those temporary mounts. Use a clean environment, fresh
state and no inherited display, D-Bus, SSH-agent, service or device sockets.
Do not grant real host-root privileges to compensate for missing namespace
support. Report a failed isolation preflight instead of silently reducing it.

The node and legacy Mesh control sockets are filesystem paths on Linux:
`<MYOWNMESH_HOME>/allmystuff-node.sock` and `<MYOWNMESH_HOME>/daemon.sock`
(`node_control.rs:293`, protocol `control.rs:535`). Namespace isolation also
contains network listeners and any abstract sockets used by other components.
Environment variables alone do not isolate those resources.

User-mode execution may still inspect host-kernel CPU/sysfs information. A
passing scan is a portable-process smoke test, not K3 device detection. Real
board drivers, audio/camera/display access, GPU paths, performance and timing
remain hardware qualification work.

## Build and run interfaces

C1's reviewed build environment must wrap these separate compile/link gates:

```text
cargo build --manifest-path node/Cargo.toml --locked --offline --no-default-features --target riscv64gc-unknown-linux-musl --bin allmystuff-serve -j 2 --message-format=json-render-diagnostics
cargo test --manifest-path Cargo.toml --locked --offline --target riscv64gc-unknown-linux-musl --no-run --workspace -j 2 --message-format=json-render-diagnostics
cargo test --manifest-path node/Cargo.toml --locked --offline --no-default-features --target riscv64gc-unknown-linux-musl --no-run -j 2 --message-format=json-render-diagnostics
```

These are not execution results. Select libtest executables from the recorded
Cargo artifact messages with `profile.test == true`; do not run every
executable in `artifacts.json` as a test or glob old `target` outputs. Match
each exact ELF/hash to its package, target, source commit, feature selection,
lock hashes and build run. Inspect ELF machine, interpreter and shared-library
requirements before claiming an artifact is static or supplying a loader.

Inside the already-verified C2 isolation wrapper, with fresh directories and
the exact manifest-selected `test_elf`, the execution interface is:

```sh
/usr/bin/qemu-riscv64 "$test_elf" --list
/usr/bin/qemu-riscv64 "$test_elf" --test-threads=1
```

The wrapper supplies a clean environment containing at least:

```text
HOME=/tmp/ams/home
ALLMYSTUFF_USER_HOME=/tmp/ams/home
MYOWNMESH_HOME=/tmp/ams/home/.myownmesh
ALLMYSTUFF_HOME=/tmp/ams/home/.allmystuff
XDG_CONFIG_HOME=/tmp/ams/config
XDG_DATA_HOME=/tmp/ams/data
XDG_CACHE_HOME=/tmp/ams/cache
XDG_STATE_HOME=/tmp/ams/state
XDG_RUNTIME_DIR=/tmp/ams/run
TMPDIR=/tmp/ams/tmp
TMP=/tmp/ams/tmp
TEMP=/tmp/ams/tmp
ALLMYSTUFF_AUTOUPDATE=0
```

Create private directories before launch, use a controlled helper PATH, and
retain stdout, stderr, exact exit code, signal/timeout status, test list and
pass/fail/ignored counts per executable. Do not silently filter tests or turn
timeout/failure into success. Doctests need a separate reviewed target runner
and Rustdoc invocation; otherwise report them as unexecuted.

## Smallest missing Serve process fixture

No existing root/node integration fixture starts Serve and proves its IPC
lifecycle. A harness around the real built binary is the smallest required
addition; it need not change production code or duplicate the node engine.

First exercise degraded startup with a deliberately unavailable daemon, in
the same isolated environment. The inner command is:

```sh
/usr/bin/qemu-riscv64 "$serve_elf" --state-home /tmp/ams/home --mesh-bin /usr/bin/false
```

`/usr/bin/false` must exist and be verified as the intended native fixture.
An absent override falls through to discovery (`daemon_spawn.rs:600`), and
`--mesh-bin` is processed only when `--state-home` is present
(`bin/serve.rs:322-340`). An existing explicit override avoids the installed
daemon update branch. Empty application state is still necessary because
startup applies pending updates before normal service operation.

The harness must own the launched process and enforce a bounded deadline:

1. Wait for a successful IPC response, not just a socket file or a log line.
   Connect to `/tmp/ams/home/.myownmesh/allmystuff-node.sock`; check mode 0600.
2. Send separate JSON requests `{"cmd":"node_version","args":{}}`,
   `{"cmd":"runtime_owner","args":{}}` and
   `{"cmd":"link_status","args":{}}`. The wire frame is
   `[u32 big-endian length][tag 0][UTF-8 JSON]`, where length includes the tag.
   Read the complete response, validate tag/length and `{ok,result,error}`,
   verify the built version and default owner `all-my-stuff-installed`, and
   record the expected disconnected Mesh state.
3. Launch a second identical Serve command without `--supervised`; require
   its successful duplicate-owner exit while the first process still answers.
4. Send SIGTERM to the owned first process; require normal exit 0 within the
   deadline, verify the old endpoint no longer answers, and start a fresh
   process on the same state to demonstrate socket recovery/rebinding.
5. Shut down the restarted process and verify the namespace contains no
   remaining owned children before final teardown. Record any retained socket
   file and recovery behavior rather than assuming unlink is immediate.

This demonstrates real local startup, IPC, duplicate-owner handling and
graceful shutdown under a failed-daemon condition. It does not demonstrate a
successful Mesh session or daemon supervision. Add a separate compatible,
isolated Mesh fixture to test identity/config bring-up, subscription,
owned-child termination/respawn and reused-daemon ownership. A fake or native
daemon can test a bounded protocol/supervisor branch, but label that evidence
separately from a RISC-V Mesh runtime and real peer transport.

Serve uses `Command::new(...).arg("serve")`, a Linux parent-death hook, and
daemon respawn (`daemon_spawn.rs:773-838`, `bin/serve.rs:653-713`). User-mode
QEMU does not itself make target ELF child execution or self-reexec
transparent. The existing no-default unit tests do not establish those paths.
A real RISC-V daemon child and unchanged self-restart need a reviewed mechanism
for guest child execution or a system guest; a native helper does not close
that gap. Keep K3 hardware qualification separate.

## Available recipe snapshot review

Read-only review of C1's seven-file `scripts/riscv` candidate found appropriate
compile-only boundaries: exact source/tool/version and vendor checks, offline
locked resolution, no-default feature checks, new attempt directories, Cargo
artifact identities, unchanged-source/lock checks, and `executed: false`.
No recipe was executed by this reviewer. Final changed/integrated recipe
identity and native-dependency results remain with C2 and the manager.

| File | SHA-256 of inspected snapshot |
| --- | --- |
| `build.py` | `53a459c85c29d0df5e9f03472ac35b1566092eb9342272860050e867431263d9` |
| `zig-musl-toolchain.cmake` | `530fbb954d14bdae42f736f8c2e0c52d97518b201a8457ad25aa56ece7e599e1` |
| `zig-cc.cmd` | `18148ae8077805c79622eeb7e13bd8f0b44c992df93ab4ed53aa4765ffb4da17` |
| `zig-cxx.cmd` | `d5bd6deff3b9920c8ee6ef44f5847bc36a4e5385414f5de2fa7c75c895f64da2` |
| `zig-ar.cmd` | `64c8c8dce96116e2b9c95cec740e02163de7a6aa1cfd3e81d506360ba03cfad1` |
| `zig-ranlib.cmd` | `e372c08422c51ec34dded8c59b3387805035d679b620e107a92b080b9b56c75a` |
| `README.md` | `a4deb3d94b36ba9494417c3511408eab0811cc78d226c91960e8d4251225b935` |

C2's emulator scripts were not ready at this audit boundary. The isolation
proposal was reviewed conceptually; exact script acceptance is handed to C1.
No target execution, full-suite pass, default-host support or board support is
claimed by this report.
