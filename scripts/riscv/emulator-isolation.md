# Root/node tests and degraded Serve lifecycle under QEMU

This is the second execution gate after the
[minimal QEMU setup and retained codec proof](emulator-README.md). The manager
runs it only after reviewing the exact scripts and successful compile records.
Source review and syntax checks do not establish namespace availability or
passing target tests. Preserve central failures and terminal run IDs.

`emulator-isolate.py` runs on the reviewed Ubuntu 24.04 Linux environment with
Python 3.11 or newer, `qemu-riscv64`, `unshare`, `mount` and `ip`. The lifecycle
fixture also needs existing native `/usr/bin/false` and `/usr/bin/readelf`.
Only QEMU provisioning is part of the separate setup script; this launcher does
not install anything or register binary handlers.

## Inputs and artifact selection

Use the directory produced by `build.py` containing `identity.json`,
`metadata.json` and `artifacts.json`. Supply its exact reviewed build commit
with `--expected-head`. `--artifact-root` is the Linux path corresponding to
the recorded Windows `CARGO_TARGET_DIR`, usually `target` when launched from
the same checkout under WSL. The launcher converts only paths beneath that
recorded directory, rejects traversal/symlink escape, and verifies each selected
ELF's SHA-256, size, RISC-V header, and absence of `PT_INTERP`/`DT_NEEDED`.

For `--mode tests`, records must come from successful `root-tests` or
`node-tests` compilation with `--workspace`. Only `profile.test == true`
artifacts execute. All other reported executables remain listed as excluded
from this gate. Root and node workspaces need separate invocations; node
workspace compilation includes `pixels`. These transferred libtest artifacts
do not include Rustdoc execution.

For `--mode lifecycle`, the record must be the successful ordinary `serve`
build. Exactly one `allmystuff-serve` artifact with `profile.test == false`
is selected. The caller supplies the independently reviewed SHA-256 of the
adjacent `serve-lifecycle.py`; no arbitrary inner command is accepted. The
expected IPC version comes from that artifact's package in `metadata.json`.

The result directory must be new, its parent must already exist, and it must
be outside `/tmp`, `/var/tmp` and `/run`. The checkout and build records must
also remain outside those mount points. Copies of selected ELFs, complete
build-record hashes, source identity, lock hashes, emulator identity, exact
commands and all execution logs are retained under the result directory.
The original artifacts and build records are read-only inputs.

## Central command templates

Replace the commit and lifecycle hash placeholders with independently reviewed
values; preserve the resulting exact command in the durable run record.

```sh
python3 scripts/riscv/emulator-isolate.py --mode tests \
  --build-record target/riscv-runs/root-tests-01 --artifact-root target \
  --expected-head BUILD_COMMIT --run-root target/qemu-root-tests-01

python3 scripts/riscv/emulator-isolate.py --mode tests \
  --build-record target/riscv-runs/node-tests-01 --artifact-root target \
  --expected-head BUILD_COMMIT --run-root target/qemu-node-tests-01

python3 scripts/riscv/emulator-isolate.py --mode lifecycle \
  --build-record target/riscv-runs/serve-01 --artifact-root target \
  --expected-head BUILD_COMMIT --run-root target/qemu-serve-lifecycle-01 \
  --lifecycle-sha256 REVIEWED_HARNESS_SHA256
```

The external deadline defaults to 1200 seconds per executable, including test
discovery or the lifecycle harness. `--timeout` accepts 1..7200 seconds.
Internal test deadlines remain unchanged and can fail under emulation. The
runner does not retry, filter, ignore or edit failing tests.

## Namespace and lifecycle contract

Each executable gets a fresh native user, mount, network, PID and IPC namespace.
The inner launcher verifies that all five namespace identifiers differ from
the outer process and that it is namespace PID 1. Mount propagation is private;
the launcher verifies the mount table and requires private tmpfs mounts at
`/tmp`, `/var/tmp` and `/run`, plus the new namespace's `/proc`. The network
must contain only an enabled loopback interface. A failed isolation preflight
stops the batch before trying subsequent executables.

State lives entirely in Linux tmpfs below `/tmp/ams`, including HOME,
ALLMYSTUFF_USER_HOME, MYOWNMESH_HOME, ALLMYSTUFF_HOME, XDG locations and temporary
directories. The helper PATH names an empty directory and automatic updates
are disabled. This preserves a disposable HOME fallback when tests remove
their state overrides. Tests execute serially within each executable with
`--test-threads=1`; each executable receives new state. No display, D-Bus,
SSH-agent or service-socket environment is inherited.

Before target execution the inner launcher writes `isolation.json`:

```text
schema: 1
namespaces: {user,mnt,net,pid,ipc}: {outer: <before>, inner: <current>}
work_root: /tmp/ams
result_root: <fresh retained artifact attempt directory>
environment: <exact clean child environment>
temporary_mounts: <verified temporary/proc filesystem types>
links: <verified loopback-only link list>
host_kernel: <kernel release>
```

The separate Serve harness verifies that record against its current namespaces
and paths before spawning. It creates its own state below the work root and
its own new result child. The launcher passes `--serve`, `--artifact-record`,
`--qemu`, `--expected-version`, `--result-dir`, and `--isolation-record`.
The harness owns the protocol assertions and all Serve process handles.

Each libtest executable first runs `--list --format terse`, then runs all its
tests. Success requires exactly one complete result summary, an exit code of
zero, and passes equal to the discovered count with zero failed, ignored,
measured or filtered tests. A zero-test binary is recorded as such. Complete
stdout/stderr, discovery, counts, failures and timeout status remain available.
Lifecycle success additionally requires the harness's retained result to name
the same build commit and Serve hash and report success.

External timeout or cancellation kills and reaps the launcher process group;
`unshare --kill-child=KILL` and namespace PID 1 exit terminate namespace
descendants. Normal lifecycle cleanup is also checked by the harness. Temporary
mounts disappear with the namespace; retained attempts are never removed.
See the [Ubuntu unshare manual](https://manpages.ubuntu.com/manpages/noble/man1/unshare.1.html)
for the native namespace and child-lifetime options.

## Evidence limits

This is isolation for the reviewed application fixtures, not a general sandbox
for untrusted binaries. The host filesystem and sysfs remain visible outside
the private mounts. The empty helper PATH prevents accidental `nvidia-smi`
execution from inventory scans, whose results still describe an emulated
process observing a host-kernel view. No device, firmware, hardware codec or
performance qualification follows from these tests.

The Serve fixture uses native `/usr/bin/false` as an explicitly failing daemon.
It can establish real Serve local IPC, duplicate-owner handling, graceful
shutdown and rebinding, but not successful Mesh sessions or a RISC-V daemon
child. User-mode QEMU, native child helpers and a full guest OS are distinct
execution environments. See the detailed
[coverage and isolation plan](../../docs/reviews/portability/serve-riscv64-test-plan.md)
for remaining host-feature, doctest, child-exec and hardware gates.
