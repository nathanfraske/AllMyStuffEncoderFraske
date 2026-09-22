# Cumulative macOS validation

`modular-macos.yml` runs on the task branch `agent/1503a621-audio-macos` and by
manual dispatch. It selects standard `macos-15-intel` (native x86_64) and
`macos-15` (native arm64) jobs, independently, without fail-fast cancellation.
The labels and preinstalled tools were checked against GitHub's
[runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)
and the [Intel](https://github.com/actions/runner-images/blob/main/images/macos/macos-15-Readme.md)
and [Apple Silicon](https://github.com/actions/runner-images/blob/main/images/macos/macos-15-arm64-Readme.md)
image inventories. The job records actual versions; image updates do not imply
a tested or fixed toolchain version.

The initial audited source is `e6340b31daa6d9c2058c4ee345564d3e0c0ecebf`.
`modular-macos-suites.json` lists every selected Rust test name, target source,
feature configuration and timeout. Cargo only builds the named packages/targets;
each resulting test binary is enumerated before its matching filter runs. A
missing, added, ignored or failing case prevents a green result. The initial
inventory contains **239 executions per architecture**, including the deliberate
17-case viewer-contract repetition with the host feature enabled.

| Selection | Executions |
| --- | ---: |
| Frame timing | 5 |
| Byte queues | 4 |
| Video pacing | 10 |
| Update policy | 6 |
| Inventory model | 7 |
| Video metadata | 5 |
| IPC client (private sockets and in-memory framing) | 16 |
| Storage plan core | 28 |
| Video core, handoff, ingress and receive | 42 |
| Terminal viewer contract | 17 |
| Terminal host public/channel contracts | 18 + 30 |
| Retained Unix terminal module (11 PTY, 2 pure) | 13 |
| Unix terminal lifecycle (9 PTY) | 9 |
| Software video decode | 7 |
| Node storage (7 memory, 15 private persistence fixtures) | 22 |

The final two gates use `cargo check --locked --lib` for `allmystuff-video` with
`host`, and the node with its default host features. They compile Mac capture,
input and audio support without opening devices. No GUI, mobile cross-build,
Mesh construction, scanner invocation, live daemon endpoint, updater test,
microphone, speaker, camera, screen recording or hardware encoder is executed.
In particular, video encoder-ladder tests and the retained audio test
`capture_and_playback_for_one_route_coexist` are excluded. The optional adapter
cases `node_control::tests::` (17 on Mac), `persist::tests::` (2), inventory
`model_identity` (1), and control-client cases (13) are outside this initial
inventory. Audio extraction tests require a separately reviewed inventory
addition after that package is integrated; this script does not discover and
run future test targets automatically.

## Environment, bounds and evidence

No setup installer runs. Python 3.11+, native Rust/Cargo/rustup, Clang, CMake and
Xcode are preflighted from the hosted image. Rustup's existing toolchain home is
retained explicitly while `RUSTUP_AUTO_INSTALL=0`; Cargo uses a new private home
and `--locked` for every build/check. Network dependency downloads are allowed,
without Cargo retrying them. No shared cache is restored. The existing
`CMAKE_POLICY_VERSION_MINIMUM=3.5` workaround supports the pinned Opus build's
bundled-source fallback on CMake 4. Its build script may instead find an installed
static Opus library through pkg-config; the lockfile does not fix that native
library's origin. Retained Cargo JSON build-script messages record actual link
libraries and search paths for review. The vendored OpenH264 and pinned mozjpeg build scripts
already fall back when NASM is absent; its presence is recorded, and no package
is installed to alter that existing fallback.

Each job creates an exclusive mode-0700 `/private/tmp/ams-mac-*` root with its own
HOME, configuration/data/state/cache/runtime directories, Cargo home, target,
cwd, `MYOWNMESH_HOME` and temporary files. The canonical path and a conservative IPC fixture
suffix must fit Darwin's 104-byte Unix socket pathname. The environment is an
allowlist; no Actions token or ordinary application endpoint/configuration is
passed to tests. `SHELL=/bin/sh` and private HOME exist before xpty's first cached
environment snapshot. Retained tests call private `open_with` and their fixed
shell commands; lifecycle fixtures additionally set their own cwd. Decode uses
`ALLMYSTUFF_H264_DECODER=software`. Test processes are serial; build concurrency
is two, with debug information and incremental outputs disabled to limit disk
and memory use on both standard runner sizes.

Each native command has a timeout, a 128-MiB combined output limit and an outer
70-minute run deadline. The workflow allows 80 minutes including cleanup and
artifact upload. Original fixture assertions, fixed waits and internal timeouts
are preserved; failures are never retried. Every command has separate complete
stdout/stderr files, arguments, exit status, elapsed time and cleanup evidence.
Artifacts also record checkout SHA, native architecture, image provenance,
toolchain/SDK versions, exact test lists and counts, test-binary SHA-256 values,
and all four Cargo.lock identities before/after. A final tracked-tree check
rejects source drift. Artifacts are retained for 14 days, including failures.

## macOS descendant cleanup

PTY children call `setsid`, and xpty's close signaller targets only its child PID.
Consequently, process-group cleanup alone does not contain the retained tests.
`macos_processes.py` uses Darwin's process inventory, a fresh inherited run
marker, observed parentage and PID/UID/start-second/start-microsecond identity.
Known children remain owned when their environment becomes unreadable. It
checks that exact identity before signalling a reparented child, with bounded
TERM/KILL scans. An unreadable new same-UID process remains unclassified and
prevents a clean result. It never signals an unclassified process. No raw
process environment or arbitrary command line is written to the artifact.

The process ABI and raw environment parser are checked at startup. Before any
tests, the runner launches `/bin/sh`, `/bin/cat`, `/bin/sleep` and `/bin/stty`
with Darwin's `POSIX_SPAWN_START_SUSPENDED`. Each image must expose the inherited
marker and private HOME/SHELL/PATH. They are never resumed: each direct unreaped
child is killed and reaped even if visibility/registration fails. This qualifies
the actual image's visibility rules without racing stty's normal short lifetime.
Apple can omit a restricted process's environment even when the sysctl succeeds;
empty/incomplete environments therefore never count as a negative marker match.

The workflow's `always()` cleanup step reloads the private process journal and
scans again. It removes the private directory only after verified empty scans
and checking its canonical location, UID, mode, device and inode. Unknown
survivors retain the directory and fail the step. A forced runner/VM termination
can interrupt this machinery; the standard hosted job's fresh VM is the final
boundary. This is a reviewed cleanup mechanism for the selected inherited-env
fixtures, not a security sandbox or a promise about arbitrary child programs.
The first Intel and arm64 runs must qualify the native ABI and cleanup behavior;
source review alone is not runtime evidence.

Relevant primary sources, frozen to Apple's Darwin 24 family where available:

- [Process inventory structures and flavors](https://github.com/apple-oss-distributions/xnu/blob/xnu-11417.140.69/bsd/sys/proc_info.h)
- [libproc API](https://github.com/apple-oss-distributions/xnu/blob/xnu-11417.140.69/libsyscall/wrappers/libproc/libproc.h)
- [Argument/environment visibility and omission](https://github.com/apple-oss-distributions/xnu/blob/xnu-11417.140.69/bsd/kern/kern_sysctl.c)
- [Spawn flags](https://github.com/apple-oss-distributions/xnu/blob/xnu-11417.140.69/bsd/sys/spawn.h)
- [Opaque spawn types and prototypes](https://github.com/apple-oss-distributions/xnu/blob/xnu-11417.140.69/libsyscall/wrappers/spawn/spawn.h)
