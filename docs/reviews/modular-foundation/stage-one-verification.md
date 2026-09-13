# Stage-one verification ledger

Evidence date: 2026-09-13 (UTC). Author: C2, session `87ba9bd9-4098-46c6-b97b-b7da68eedab2`. Manager task: `872a63e7-c134-4fcf-be6d-6830680154ac`. Independent ledger review: **accepted by A1**, session `82e1188e-ff92-4c4e-bf95-d2afae251655`, task `796f45c7-5957-4f1a-a020-0098d741cc1e`. A1 verified substantive draft blob `91c340dc791a6cbde8ae3cbd3a450961df88f5f9`; this final record adds that acceptance and A1-confirmed total/review identity.

The bounded stage-one source passes the focused local Windows x64 checks below. This establishes the first host-feature, capability-advertisement, and direct-node-help corrections; it does not establish a complete minimal node or qualify live mesh/media operation. Read the [roadmap](../../MODULAR-FOUNDATION.md) with the initial [responsibility](responsibility-mesh-review.md), [dependency](modularity-dependency-review.md), [platform](headless-platform-review.md), and [behavior/test](behavior-test-review.md) reviews. Their baseline findings are historical source evidence; this ledger records subsequent execution.

## Source and independent review identities

Starting upstream: `b15aa1a277894999a8f01134843c06e892de084b`. Tested integration: `152c933920d788b80e5b8c80a580d600ed07e01c`, branch `agent/1503a621`. The initial review pairs were A1/A2 and C1/C2; implementation source review used A1 for A2's feature changes, C2 for C1's capability changes, and C1 for C2's help changes. A2 independently accepted A1's architecture/roadmap with the lock-status/mobile qualifications incorporated. A1 independently accepted this ledger.

| Artifact | Worker commit | Integrated commit |
| --- | --- | --- |
| A1 responsibility review | `d9a627f67edeea6ee4e47dd01b30d13b004af003` | `3597ebe13717ea465dc5143dad5f898b72db90db` |
| A1 review isolation correction | `13d7d630095c32691d543e4af552fe6cb6b95481` | `3fd1e5ffad5b62ca0c252e1851afff79e105f558` |
| A2 dependency review | `b78b6f5b72fb3e58634a607f19bba9b9cae82df0` | `69d899033e801e26efd583877fbf8794ea1e0cab` |
| C1 platform review | `faf518d2a680adddeaa4dfedf341fc49f279c615` | `e14d5184d2c5658985be8c7025a3cc557ca92682` |
| C2 behavior/test review | `c39391c7ca86010c294005e998248937d5b97a74` | `51b2b8955953f8dc0a863d255472eac33f53aecc` |
| A2 narrow node lock prerequisite | `6682494e1ea7889561503dd56a4b28c51b119611` | `113a2b3cd378fb115deab7ee06a6c0843073021b` |
| A2 host dependency/capture gates | `cc22ea2a85a3bca6ced82dc8146b77b3cf987c86` | `f576f864e753f9a3672b1bdde33fca7e1637a6c3` |
| C1 compiled capability filtering | `9e1f9839501943747b509fb066098e8080367a20` | `0e47ce60ed51be93b84ed5da3b71448dee564024` |
| C2 first-argument direct-node help | `b69e66d9dc2c270d6e5f2f9529e9a677ca7c2ed2` | `fbf9877c04643e0c56176321b0962af3d760bd6c` |
| A1 architecture/roadmap | `0944ef3865429801a21c3770d8626d64e9e46a1b` | `152c933920d788b80e5b8c80a580d600ed07e01c` |
| A2 user guide, independently reviewed by C1 | `91f33d772b1a758c9c8d28115d7411d6aa871fcc` | `29e746203a547ec45f3493b7dd7706fb6f4384b7` |

C2 compared the lock, three implementation slices, and roadmap files against their integrated copies: identical. The four stage-one commits integrated in durable run `6978bd4b-c995-4e21-ac36-295f59516c62`, succeeded/exit 0, 22:58:57.933-22:58:58.234 UTC (0.301 s). Manager and C2 serve blobs are both `0a8d3d2e7da1c5aecfd3ec66b931c34181eaec9d` in separate worktrees. Main, origin/main, and upstream/main remained at the starting upstream commit when checked. No archive or old dirty Documents checkout was used.

The later [user guide](../../USING-ALLMYSTUFF.md) adds 18 documentation lines only. C2 verified its source/integrated blob `d6a11153fb9ec3854fe605a28804db73bc02cd2c`; integration `29e746203a547ec45f3493b7dd7706fb6f4384b7` changes no implementation from tested `152c933920d788b80e5b8c80a580d600ed07e01c`. Documentation descendants are not new tested revisions.

## Execution provenance and commands

All builds/tests here were manager-owned durable runs, local `win32`/`x64`, with no remote transport or project payload transfer. C2 inspected terminal state and retained logs; EOF alone was not treated as completion. Final test commands ran from `C:\Users\Admin\AppData\Roaming\AllMyAgents\data\worktrees\1503a621` with executable `C:\Users\Admin\.cargo\bin\cargo.exe`. Tables list its exact arguments, using explicitly defined common prefixes where indicated.

Manager verified toolchain: rustc 1.97.1 (`8bab26f4f`, 2026-07-14), Cargo 1.97.1 (`c980f4866`, 2026-06-30), bundled CMake 3.29.5-msvc4. Node compilation used these explicit environment overrides:

```text
CMAKE=C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe
CMAKE_POLICY_VERSION_MINIMUM=3.5
```

No dependency-tool installation or host/service restart was required. Final run provenance records environment hash `ec8515f1049b1d028f811215e6abc27b9534ff333f37b7a4fa9fb9f08b34c035`; environment values above are manager-supplied because the run API exposes only keys/hash. Provenance marks the checkout dirty due to untracked managed `AGENTS.md`; manager verified no tracked source drift. Its changing fingerprint is not a changed implementation. Hub lock provenance lists root `Cargo.lock`; the separately inspected, LF-normalized node lock SHA-256 is `81124dd2257d314943037495620808240c98683f9deb43e7b00a67942aa2bd24`. Do not confuse that with a raw working-file hash.

## Baseline and lock prerequisite: execution versus acceptance

| Durable run | Command/evidence | Terminal result and interpretation |
| --- | --- | --- |
| `55c240f9-9dd5-4765-a284-9cc5312ad1c5` | `test --workspace --locked`, exact upstream baseline | Succeeded/0, 85.469 s. 316 unit tests and 3 doctests passed; no failures/ignores. Root workspace excludes node and GUI. |
| `9eb3cb23-533c-4622-b1bb-73db0f9ed70c` | `tree --manifest-path node/Cargo.toml --locked --no-default-features --target x86_64-pc-windows-msvc --edges normal --prefix none --format {p}`, A2 baseline | Failed/101, 22.656 s: baseline node lock needed updating. No graph or compilation result. |
| `22b22b73-61c2-4cb0-a691-236cd07959a9` | `update --manifest-path node/Cargo.toml --offline`, followed in order by `-p allmystuff-bridge -p allmystuff-cec-consent -p allmystuff-cec-protocol -p allmystuff-graph -p allmystuff-inventory -p allmystuff-protocol -p allmystuff-session -p allmystuff-updater` | Command succeeded/0, 1.754 s; **result rejected** by review. It also advanced seven registry packages and remapped Windows dependencies. This was not an accepted repair. |
| `26c3475e-b1a0-4d91-8d0c-24462d05db21` | `tree --manifest-path node/Cargo.toml --locked --offline --no-default-features --target x86_64-pc-windows-msvc --edges normal,build --depth 2`, A2 with narrow candidate lock | Failed/101, 3.102 s: `hashlink 0.9.1` absent from local cache; offline HTTP forbidden. No lock-drift error; no compilation. |
| `d618764c-3a72-4607-8c71-3a5a94474828` | Previous exact tree arguments with only `--offline` removed | Succeeded/0, 11.088 s; downloaded seven already-locked packages and resolved the graph without another lock change. |
| `49ab1b24-95bf-42c0-835f-5dcdeb6cbd9a` | `check --locked --manifest-path node/Cargo.toml --no-default-features --bin allmystuff-serve`, integration `3fd1e5ffad5b62ca0c252e1851afff79e105f558` | Succeeded/0, 79.916 s. Baseline implementation plus accepted lock/reports, before stage-one source changes. Two inherited `FrameCadence` dead-code warnings. Compilation only. |

The accepted prerequisite changes only the eight named local path-package versions from 0.2.119 to 0.2.121 (8 additions/8 deletions), preserving registry selections. Resolution failure, rejected successful mutation, cache failure, accepted repair, and compilation are distinct evidence. No ambiguous outcome was replayed.

## Integrated focused verification

Every row below is at `152c933920d788b80e5b8c80a580d600ed07e01c`. Common argument prefix **T** means exactly `test --locked --manifest-path node/Cargo.toml`; append the listed suffix in order. All terminal results are **succeeded, exit 0**: eight test runs total 38 passing test executions, with zero failed or ignored, plus the direct-help process. Counts are executed tests, not source attributes; repeated feature configurations are not unique additional tests. All output streams were complete and untruncated when inspected.

| Configuration/check | Exact suffix after T (or complete run arguments) | Durable run | UTC start-end; elapsed | Passed / filtered; stdout/stderr bytes |
| --- | --- | --- | --- | --- |
| No host/audio, capabilities | `--no-default-features --lib advertised_capabilities` | `1c66d24f-f171-4746-b5ba-15d8738445ce` | 22:59:38.944-23:01:11.043; 92.099 s | 3 / 322; 374/8220 |
| No host/audio, help | `--no-default-features --bin allmystuff-serve cli_tests::` | `5e476958-24a7-477a-9915-1a48333e4a4a` | 23:01:45.996-23:02:17.513; 31.517 s | 3 / 3; 330/1052 |
| Ownership memory fixtures | `--no-default-features --lib ownership::tests` | `7f189a59-85e3-48c4-990b-83e806ab1503` | 23:02:17.535-23:02:27.240; 9.705 s | 19 / 306; 1572/169 |
| Pure privileged-offer helper | `--no-default-features --lib mesh::tests::privileged_offers_are_refused_exactly_when_unauthorized -- --exact` | `e76c1192-eb7a-406f-8a92-2708907aba50` | 23:02:27.262-23:02:27.858; 0.596 s | 1 / 324; 195/169 |
| Audio only, capabilities | `--no-default-features --features audio-io --lib advertised_capabilities` | `548caf1d-0528-4332-95e3-99b1cb531896` | 23:02:27.880-23:03:12.315; 44.435 s | 3 / 329; 374/439 |
| Audio only, help | `--no-default-features --features audio-io --bin allmystuff-serve cli_tests::` | `bfe1783d-4b5b-473d-bfda-5b4375dd017c` | 23:03:57.332-23:04:39.161; 41.829 s | 3 / 3; 330/1052 |
| Default host, capabilities | `--lib advertised_capabilities` | `57a8dd9d-f659-44fd-9848-cae3168f2fd7` | 23:04:39.184-23:06:21.364; 102.180 s | 3 / 437; 371/3143 |
| Default host, help | `--bin allmystuff-serve cli_tests::` | `fdd88265-ab6f-4e49-809a-ecd3d1854850` | 23:06:21.390-23:07:10.986; 49.596 s | 3 / 3; 330/289 |
| Direct no-host node help process | Complete args: `run --locked --manifest-path node/Cargo.toml --no-default-features --bin allmystuff-serve -- --help` | `aae1883d-3852-4a6b-a595-5628f448e8d9` | 23:07:11.011-23:07:22.778; 11.767 s | Printed static usage and exited 0; 892/1009 |

The no-host and audio-only help builds and direct-help process report the same two inherited `FrameCadence` dead-code warnings. Other focused rows contain no warnings. Ownership's twentieth source test is Unix-only and did not run here. Capability tests construct an inventory fixture and bridge profile without `Mesh::new`, device scanners, or IPC; default actually ran `advertised_capabilities_default_host_preserves_the_bridge_profile`. Help tests call inert help/version branches and classify update without network access; they retain first-verb precedence and default/unknown/runtime pass-through. The direct process log contains static help; the claim that it precedes state/update/log/socket startup additionally rests on the independently reviewed call order, not a filesystem or network trace.

Source-reviewed capability expectations: disabling host removes screen Display Sources, camera Video Sources, control Input Sinks, and clipboard Duplex; disabling audio-io removes Audio capabilities. Controller/physical Input Sources, viewer/display Sinks, and storage remain. Audio-only retains microphone Sources, speaker Sinks, and system Duplex. Default host implies audio-io and preserves the bridge profile; host-without-audio is not a supported feature combination. Audio advertisement is not proof of hardware availability or native loopback on every OS.

Source-reviewed help contract: only first-token `--help`, `-h`, or `help` exits early; later help tokens retain existing permissive runtime behavior. Existing version/update precedence and default/service/supervised/session-agent/log paths remain. The outer `allmystuff` wrapper still applies pending updates before dispatch, so the inert-startup guarantee here belongs to the direct node binary.

## Graph and formatting evidence

Manager-owned graph runs against A2's reviewed candidate source (before its source commit) succeeded: `31539d3d-4038-41b7-b01b-3db8199d7aa8` (0.631 s), no-default normal/build tree; `63930d93-2c44-4412-8b6a-87f2de9a9369` (0.494 s), inverse `windows@0.61.3` features; `9a1e1f50-e291-4672-aae1-7ad687f8fa67` (5.237 s), default inverse `allmystuff-pixels` features. These support removal of pixels and eight host-only Windows 0.61 feature groups from the no-host graph while retaining pixels through default -> host. Base Windows dependencies and native codecs/storage remain. Graph evidence does not establish a dependency-free or media-free node. Exact commands and dirty-source provenance are retained under the run IDs and in the dependency review/roadmap.

Durable formatting succeeded/0 for the reviewed source: help `64e8cba8-51f3-4b2f-8fda-241771fec235` (0.149 s), capability `cbe3a1bd-be7c-43ea-a4bf-a90a2abd7397` (0.398 s), host gates `eb333140-ab89-4865-bddf-f056895ca116` (0.100 s). Executable `C:\Users\Admin\.cargo\bin\rustfmt.exe`, arguments `--check --edition 2021` plus respectively `node/src/bin/serve.rs`, `node/src/mesh.rs`, or `--config skip_children=true node/src/lib.rs node/src/win_capture.rs`. These were formatting checks of the reviewed worker diffs, not additional runtime qualification.

## Limits and remaining handoff

- Root tests were executed at the original baseline; the focused node matrix was executed at the integrated source. This was not an exhaustive node, GUI/frontend, CLI-wrapper, mobile, hardware, or cross-platform suite. The declared MSRV 1.88 was not tested by Rust 1.97.1. No remote testbed was used.
- No live serve/service, real mesh credential/data access, daemon handshake, capture/render/input/audio loop, or runtime authentication scenario was exercised. Source gaps in cached fleet revocation, Session Accept peer comparison, consent sweep timing, and socket/ACL assumptions remain recorded in the behavior review; the focused pure auth-helper pass does not close them. No authentication/authorization behavior was changed.
- A green default suite can early-return from `hardware_pump_is_lossless_and_decodable` if hardware/MFT opening is unavailable; `hardware_paced_slices_are_real_cut_points` is ignored. Neither was selected in this focused matrix.
- MyOwnMesh v1.0.0 remains an evolving contract. This stage makes internal feature/advertisement/help corrections and presumes no finalized external API. The broader modular application roadmap and future adapter-contract review remain necessary.
- Manager reported cleanup after the relevant verification and after confirming no queued/running jobs: `C:\Users\Admin\AppData\Roaming\AllMyAgents\data\worktrees\1503a621\target`, 1,976,609,216 bytes; the same root's `node\target`, 11,724,635,229 bytes. Both are absent; total reclaimed 13,701,244,445 bytes (12.760 GiB). Manager verified canonical in-workspace targets with no reparse points or tracked files. C2 found no `target`, `node/target`, `gui/target`, `gui/src-tauri/target`, or `gui/mobile/target` in its own worktree: 0 generated/deleted bytes. Source, reports, managed `AGENTS.md`, and durable run logs remain; no compiled binary is retained or required for handoff.
- A1 independently accepted the ledger after auditing all eight test runs and the direct-help run, source/integration identities, and the evidence limits. The final roadmap link and normal task-branch publication are manager-coordinated handoff work; no additional native tests are requested by this ledger.
