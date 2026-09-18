# RISC-V emulator setup and retained codec proof

These manager-run recipes prepare Ubuntu 24.04 amd64 and execute the one
retained synthetic codec test. They do not rebuild it or change application
sources, dependency pins, daemon configuration or system binary handlers.
Creation and source review of these recipes are not execution evidence; record
the central setup and test run IDs with their actual terminal results.

## QEMU provisioning

The reviewed local environment is root in WSL `Ubuntu-24.04` (Ubuntu 24.04.4,
amd64), with Python 3 and GNU `readelf` already present. The official Ubuntu
[`qemu-user` package](https://packages.ubuntu.com/noble/amd64/qemu-user) supplies
user-mode emulation. Its `qemu-user-binfmt` integration is a recommendation;
the recipe uses `--no-install-recommends` and does not install that integration
or modify `binfmt_misc`. It requires an existing package candidate from the
distribution's configured signed repositories and never adds repositories.

From the reviewed checkout inside that WSL distribution, the central setup
command is:

```sh
bash scripts/riscv/emulator-setup-ubuntu.sh
```

The script prints package policy, resolves one exact candidate version, installs
`qemu-user=<version>` and required dependencies, then records the installed
package version, emulator version, CPU list and executable SHA-256. Preserve
the complete setup output. A later reproduction can pass that recorded version
as the sole argument; if unavailable, retain the failure instead of silently
choosing another version. No target code runs during setup. The package
manager's normal signature checks remain enabled.

## First codec execution

The fixture is the ELF retained from successful compile/link run
`22422c83-9b5c-4fe0-b562-47a8b4d65245`, described in the
[OpenH264 portability record](../../docs/reviews/portability/openh264-riscv64.md).
Its immutable identity is:

| Property | Value |
| --- | --- |
| SHA-256 | `0dc6386fa8000a87f38d9ca18380f4fa4aea3fbb34eed73c62d5cce3dee3f2d6` |
| Bytes | `23792376` |
| ELF | 64-bit little-endian, machine 243 (RISC-V), type 3 (static PIE), flags 5 |
| Consumer | `openh264` 0.9.3 with patched `openh264-sys2` 0.9.6 |
| Test | `software_encode_decode_roundtrip` |

The reviewed consumer creates a synthetic 64x64 RGB image, converts it to YUV,
encodes an IDR frame, decodes it, and checks dimensions. It has no application
state, network, daemon, child-exec or device behavior. Its source is retained at
`target/openh264-riscv64-0.9.6-candidate/patched/tests/codec.rs`, SHA-256
`2405e4e4c87ef2c65e341682b8084d5cf5a90889983eb0295763030047fdc631`.

From the manager checkout containing that retained proof, after successful
setup, run centrally:

```sh
python3 scripts/riscv/emulator-codec-proof.py \
  --elf target/openh264-riscv64-0.9.6-candidate/proof/codec-riscv64gc-linux-musl \
  --run-root target/qemu-codec-01
```

The output directory must be new and its parent must already exist. The runner
accepts only that fixed ELF hash and size, copies it into the attempt directory,
checks its header and retains `readelf -l -d` output. The proof has no
`PT_INTERP` or `DT_NEEDED` entries; no RISC-V dynamic loader or guest sysroot is
required for this fixture. An unexpected loader/library requirement stops the
attempt before target execution.

The runner records the installed QEMU path, version and hash and explicitly
selects `-cpu rv64`, a
[QEMU RISC-V CPU model](https://github.com/qemu/qemu/blob/v8.2.2/target/riscv/cpu-qom.h#L30).
The artifact was built for `riscv64gc-unknown-linux-musl`, with the native codec
using RV64 integer/multiply/atomic/floating-point/compressed instructions,
Zicsr/Zifencei, and LP64D. The emulator CPU selection is a functional execution
environment, not an exact vendor CPU or ISA-minimum qualification.

Execution uses a clean environment, private home/state/temp directories, an
empty helper PATH, one libtest thread, and a 180-second external deadline.
Timeouts kill and reap the owned process group and remain failures. The runner
retains full stdout/stderr and JSON request/result records, requires the named
test to pass with exactly one passed and zero failed/ignored/measured/filtered
tests, and verifies the copied ELF hash afterward. It never removes attempts.

This first fixture uses fresh state and a bounded process; it does not establish
filesystem or network namespace isolation. Its fixed hash deliberately prevents
use as a general application test launcher. A pass proves this codec roundtrip
under the recorded emulator only. Full Serve linkage, root/node suites,
doctests, application lifecycle, Mesh interoperability, firmware, real hardware,
performance and timing require their own evidence.

## Later suite and lifecycle gates

[QEMU user mode](https://www.qemu.org/docs/master/user/main.html) translates
target Linux syscalls and runs guest threads as host threads. It shares the
host kernel. QEMU can run within native host namespaces, but namespace clone
flags are not supported from the emulated process itself. Root/node unit tests
and Serve need a separately reviewed outer namespace launcher and the isolation
rules in the [test plan](../../docs/reviews/portability/serve-riscv64-test-plan.md).

Select test executables from the build artifact records with
`profile.test == true`; ordinary Serve executables need a lifecycle harness.
Retain per-executable discovery, counts, failures and source/hash identities.
Real receive-side codec tests and root AU vectors remain relevant with node
defaults disabled; host send-side integration has separate coverage gaps.

Manual user-mode invocation does not make RISC-V child/self-exec transparent:
the [QEMU 8.2 Linux exec path](https://github.com/qemu/qemu/blob/v8.2.2/linux-user/syscall.c#L7912)
passes execution to the host kernel. A real target daemon child or target
self-restart requires an independently reviewed mechanism, such as a system
guest or explicitly authorized binary-handler setup. Neither is provisioned
here. A native failed-daemon fixture must remain separately labelled.
