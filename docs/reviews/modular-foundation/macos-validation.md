# Cumulative macOS validation

Validation record, 2026-09-22. GitHub Actions run
[35763211936](https://github.com/nathanfraske/AllMyStuffEncoderFraske/actions/runs/35763211936)
passed on native Intel and Apple Silicon at
`b86c9fb31baed34a2feeb55243a90120ec669088`. Each job passed **338 executions in
38 selected suites**, both Mac host compile gates, lockfile checks and normal
process/directory cleanup. The combined 676 executions include deliberate repeats
across architectures and feature configurations; they are not 676 distinct tests.

| Native job | Runner | UTC interval | Job elapsed |
| --- | --- | --- | --- |
| [106866157491](https://github.com/nathanfraske/AllMyStuffEncoderFraske/actions/runs/35763211936/job/106866157491), x86_64 | `macos-15-intel` | 17:50:27–18:04:50 | 14m 23s |
| [106866157880](https://github.com/nathanfraske/AllMyStuffEncoderFraske/actions/runs/35763211936/job/106866157880), arm64 | `macos-15` | 17:50:33–17:57:33 | 7m 00s |

These are complete job times, including dependency downloads and compilation,
not a controlled performance comparison. The later source
`ad5962d3db096e1ee42d3a1ebeae184c606cab7d` adds only two comments and
`#[allow(clippy::duplicate_mod)]` to the audio test harness include. Its diff from
the tested commit changes no production code or test assertion; it was not the
commit run by this Mac workflow.

## Selected execution and build evidence

The [frozen inventory](../../../scripts/ci/modular-macos-suites.json) records every
test name, source target, feature configuration and timeout. Its retained bytes
match the tested commit exactly (SHA-256
`14b92d1579ff242cc6ca3a6a8cebe93ebccc6b724c3b4799e8c0e8fe8a933e8b`).
The [runner README](../../../scripts/ci/README.md) describes the selection and
isolation boundaries.

| Build group | Suites | Passing executions per architecture |
| --- | ---: | ---: |
| Core libraries | 16 | 123 |
| Terminal viewer | 1 | 17 |
| Terminal host | 4 | 70 |
| Software video decode | 1 | 7 |
| Node storage | 1 | 22 |
| Audio default | 3 | 14 |
| Audio codec | 5 | 33 |
| Audio I/O compiled, device-free selection | 7 | 52 |
| **Total** | **38** | **338** |

Core coverage includes timing, byte queues, pacing, update policy, inventory,
video metadata, private-socket IPC, storage models and video contracts. Terminal
host coverage is 18 public contracts, 30 channel comparisons, the retained Unix
module's 13 cases (11 PTY and two pure), and nine PTY lifecycle cases. Node storage
includes seven memory and 15 private persistence cases. Audio's 99 executions
across three feature configurations cover 52 distinct safe definitions.

The audit reconciled all **97 commands per job**: ten provenance commands, eight
locked test builds, 38 discovery commands, 38 test commands, two locked host
checks and one tracked-tree check. Every command exited zero. All 388 retained
stdout/stderr files across both jobs match their recorded byte lengths and were
read completely. Each discovered test-name list and each individual runtime
pass name matches the frozen inventory, with zero failed or ignored cases.
Cargo JSON maps each selected source target to its private test binary; the
recorded binary SHA-256 identities agree wherever a binary is reused. The runner
also checked each binary's hash after execution.

Builds used `cargo test --locked --no-run --message-format=json-render-diagnostics`
with the inventory's package/target/feature arguments. Discovery and execution
used the same filter, including `--exact` for the sole retained audio LevelStats
case. Execution was serial with `--test-threads=1 --nocapture`. Both jobs also
passed these compile-only commands:

```text
cargo check --locked -p allmystuff-video --no-default-features --features host --lib
cargo check --locked --manifest-path node/Cargo.toml --lib
```

No retry was needed or performed. The logs retain a future-incompatibility warning
for dependency `block 0.1.6`; these results do not claim a warning-free build or
Mac Clippy coverage.

## Actual tools and native linkage

| Recorded property | Intel | Apple Silicon |
| --- | --- | --- |
| Runner image | `20260824.0482.1` | `20260907.0337.1` |
| Rust / Cargo | `1.98.0` / `1.98.0` | `1.98.1` / `1.98.1` |
| Rust host | `x86_64-apple-darwin` | `aarch64-apple-darwin` |
| CMake | `4.4.2` | `4.4.3` |
| macOS / Darwin | `15.7.9` (`24G830`) / `24.6.0` | Same |
| Python | `3.14.7` | Same |
| Apple Clang / Xcode | `17.0.0` / `16.4` (`16F6`) | Same |
| NASM in selected PATH | Absent | Absent |

Both jobs used the SDK under `/Applications/Xcode_16.4.app` and preinstalled
native toolchains; no setup installer ran. Rustup reported the repository's
`stable` override. These are the observed versions for this run, not fixed future
properties of the runner labels.

In both architectures, `58-build-node-storage.stdout.log`,
`68-build-audio-codec.stdout.log` and `79-build-audio-io.stdout.log` record
`audiopus_sys 0.2.2` with `linked_libs: ["static=opus"]`. Their native search paths
are under the private `target/debug/build/audiopus_sys-*/out/lib`, matching the
pinned build script's bundled CMake output, rather than an installed Opus search
path. `CMAKE_POLICY_VERSION_MINIMUM=3.5` was set. Paired codec tests compare
independent instances using the same linked library; they do not establish
universal Opus packet bytes across library versions. The artifacts retain link
directives, not a separate native-library hash or version query.

## Source and cleanup checks

Both jobs' before/after lock hashes equal the tested commit's Git contents:

| Lockfile | SHA-256 |
| --- | --- |
| `Cargo.lock` | `b9a80698920534aee9f426c6c2b929af1caee84df7718f9c9f2d60d77c47ad09` |
| `node/Cargo.lock` | `6c3374337440755c491318de2ff7548ede25af0ec06f7bf0f3759394d5ac0263` |
| `gui/src-tauri/Cargo.lock` | `e9a024fc3dd429f2f7140dc341159fce741778931ba027d1f02a3cbf4910eb5b` |
| `gui/mobile/Cargo.lock` | `d9da000cab3d2387323ee16da7f31fc3b42c28cba7a6ae10b8dc95f4dfb3c86f` |

`git diff --exit-code HEAD --` returned zero with empty output. Each job used a
private HOME, cwd, temporary directory, Cargo home/target and application state
root. The conservative Unix socket pathname budget was 77 bytes. The Darwin
process ABI/environment preflight passed for the suspended `/bin/sh`, `/bin/cat`,
`/bin/sleep` and `/bin/stty` images; each was killed and reaped by that preflight.

All 97 per-command cleanup receipts and final process scans in each job report
clean, with no signals or scan errors. Both `always-cleanup.json` receipts record
the exclusive supervisor lease acquired immediately, no handoff signals, clean
process scans and `private_root_removed: true`. This qualifies the selected
native ABI/visibility, PTY fixtures and **normal cleanup path** on both images.
It does not exercise forced cancellation, supervisor TERM/KILL escalation or
cleanup during an interrupted launch. Test binaries and native outputs were
removed, so this artifact audit cannot independently rehash those files afterward.

## Earlier cancellation and correction

The first run,
[35760215630](https://github.com/nathanfraske/AllMyStuffEncoderFraske/actions/runs/35760215630)
at `0c169987bf59f1b4f2f14451eb36aa80f17d15ab`, was cancelled. Intel completed
22 suites/217 executions before interruption during node-storage compilation;
ARM completed 23 suites/239 executions before interruption during the host
checks. Neither was a complete workflow pass. These were the pre-audio selections.

The retained Intel process identities prove that two host Cargo commands started
after its `always()` cleanup began. On ARM, the original supervisor recorded a
later node-host command, its log contained `Downloading crates ...`, and directory
removal failed with errno 66 (`Directory not empty`) inside the private Cargo
registry. That establishes failed ARM removal and supervisor/cleanup overlap;
it does not identify the exact ARM writer. Intel cleanup reported five TERM
signals and successful removal. The retained evidence does not establish why
GitHub cancelled the run.

Reviewed correction `178ea7e34a23e7d81e73a0ce5ef34ca6125ed48d` made Python the
step entry process with `exec`, added a permanent stop request and an exclusive
supervisor lifetime lease, and stopped later launches after a signalled child.
Cleanup now obtains the lease before restoring the journal or removing state,
with bounded escalation only against the recorded supervisor's exact UID/start
identity. Removal errors remain failures and are not suppressed or retried.
The successful follow-up validates ordinary operation of this correction;
forced-cancellation recovery remains untested.

## Evidence retention and limits

The manager retained complete run/job/artifact metadata, workflow logs and
downloaded artifacts under
`target/audio-validation-20260922-01/macos-35763211936` and the corresponding
`macos-35760215630` directory. The successful archives are
`modular-macos-x86_64-35763211936-1` (artifact `10711960643`) and
`modular-macos-arm64-35763211936-1` (artifact `10711337737`). Each contains
`summary.json`, `selected-suites.json`, `always-cleanup.json` and all 194 command
output files. GitHub records archive digests
`7795918e124d4e7ed3f69b331c9e03a4dbdc7cd23b81864fdf058415afa551ef` (Intel) and
`e67cb0e7db3e28608b644e7c96e4029c2bfd43d0cea59518dffb50475801019a` (ARM), with
14-day retention. Local evidence copies are retained separately.

This selection executes no real audio device test, video encoder ladder, GUI,
mobile build, scanner invocation, Mesh construction or live daemon endpoint.
Host compilation does not establish microphone/speaker/camera/screen capture,
hardware encoder quality, TCC permission behavior or application usability.
The optional adapter tests omitted by the inventory remain outside this result.
No additional native run was performed while preparing this report.
