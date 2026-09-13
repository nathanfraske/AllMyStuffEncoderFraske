# Headless startup and platform review

Review date: 2026-09-13. Source baseline: `b15aa1a277894999a8f01134843c06e892de084b` (0.2.121). Owner: C1, session `9b1bd3f4-fe32-4dfd-b444-1d663b49d165`, branch `agent/9b1bd3f4`. Report only; implementation is not authorized by this report. Independent reviewer C2 (`87ba9bd9-4098-46c6-b97b-b7da68eedab2`) accepted the matrix, startup and mismatch findings; direction, audio fallback and version-alias qualifications are incorporated below. Manager's distinction between internal media separation and mesh API migration is also incorporated.

The existing `serve` entry point already avoids a webview. It does **not** provide a small, media-free foundation: the default node enables capture, and disabling defaults leaves codecs, viewers, storage, service supervision, and other platform machinery. The first functional preparation should make the existing reduced build's advertised capabilities agree with its implementations, after resolving the baseline lockfile prerequisite.

## Startup paths and boundaries

| Entry | Verified source behavior | Consequence |
|---|---|---|
| `allmystuff` | `crates/allmystuff-cli/src/main.rs:25` applies staged updates before dispatch; no arguments launch `gui_launch::launch`. On Linux, `gui_launch.rs:20` rejects missing `DISPLAY` and `WAYLAND_DISPLAY`. | Root CLI is webview-free, but bare invocation expects a GUI. Root workspace success cannot qualify the node. |
| `allmystuff serve [args]` | `crates/allmystuff-cli/src/serve.rs:23` finds `allmystuff-serve` through the shared service crate and forwards arguments. Unix replaces the process with `exec`; other targets spawn/wait. | No GUI launch is needed. Node binary and mesh runtime still need provisioning. |
| `allmystuff-serve` | `node/src/bin/serve.rs:160` handles version/update verbs before startup. `main:218` configures state, applies pending updates, initializes logging, and chooses foreground/supervised/Windows SCM mode. | `--version` / `-V` / `version` are inert node probes. `--help` and unknown first arguments fall through to full startup; do not use them as smoke tests. |
| Foreground node | `serve.rs:406` first binds the local node socket, avoids duplicate ownership (including existing takeover/standby rules), resolves the mesh control client, ensures/supervises a daemon, constructs `Mesh` with `SocketSink`/`LogSink`, starts it, serves local control/events, runs the unattended updater, then waits for shutdown. | Preserve this lifecycle and its authorization boundaries when extracting modules. A live invocation is not a compile-only check. |
| Desktop GUI | `gui/src-tauri/src/main.rs:5206,5280` creates `NodeClient` and ensures the node process. Its manifest still directly depends on the default node (`gui/src-tauri/Cargo.toml:50`). | Thin process ownership does not mean a thin compile dependency closure. |
| Mobile GUI | `gui/mobile/Cargo.toml:44,84` defaults to embedded `mesh`, linking node with defaults off plus `audio-io`; `gui/mobile/src/engine.rs:200` constructs/starts `Mesh` in process. | The phone path is an embedded library path; standalone desktop `serve` is not established for iOS/Android. |

Root `Cargo.toml:22` excludes node and GUI workspaces. `node/src/lib.rs:198` supplies the existing `UiSink` seam. `node/build.rs:20` only stamps target/pin facts; GUI build machinery stages sidecars. `.myownmesh-rev` and mobile patches currently name **v0.3.21**. This review does not inspect another project's source or assume a finalized MyOwnMesh v1.0.0 contract.

`Mesh::start` remains substantial even without capture: `node/src/mesh.rs:3210` starts native-drive recovery, FleetFiles watching, media forwarding, inventory watching, offer/room/consent maintenance, clipboard synchronization, and network handling. On Windows the existing SCM path runs a privileged console-session agent (`serve.rs:984`); it assumes an interactive session for desktop control. A server with no logged-in console needs separate runtime qualification, not an inference from the foreground path.

## Current feature and platform assumptions

- `node/Cargo.toml:52` defaults to `host`. `host` includes audio I/O, PTY, screen/camera capture, input injection, clipboard, and target-specific Linux/macOS capture dependencies. `hwenc` enables `host` and FFmpeg. `audio-io` can be selected independently.
- `--no-default-features` substitutes capture/injection stubs (`node/src/lib.rs:49-187`), but OpenH264, Opus, bundled SQLite, file/drive/site handling and decode remain unconditional. Windows API/service dependencies and `win_capture`, `win_privilege`, `windows_fleetfiles`, and telemetry declarations are not wholly removed by `!host`. This is manifest/module evidence; a resolved dependency tree is currently blocked below.
- **Profile mismatch:** `mesh.rs:13576` removes only `screen` and `camera` origins for `!host`. The bridge still supplies input-control sink, duplex clipboard, duplex system audio, and scanned audio endpoints (`crates/allmystuff-bridge/src/lib.rs:96-180`). Input/audio stubs drop operations and clipboard stubs report unavailable (`node/src/stubs/`). Terminal/camera/clipboard-receipt feature tags are already host-gated (`mesh.rs:4029`). A capture-less profile is therefore not yet a reliable list of usable capabilities.
- Linux default node CI installs ALSA, PipeWire, XKB, GBM, udev, GTK/X11/Wayland and FFmpeg development packages (`.github/workflows/ci.yml:92-103`). Webview-free does not imply display-library-free. Actual hardware availability and permissions remain runtime questions.
- Windows NVDEC has guarded x86_64 AVX2 code with a scalar fallback (`node/src/nvdec.rs:1001`); this is not an x86-only proof for the entire node or evidence that ARM Windows works. Linux aarch64 GUI has a WebKit rendering workaround (`gui/src-tauri/src/main.rs:4058`).
- `allmystuff service` explicitly supports systemd, launchd, and Windows SCM; unsupported init systems are told to use foreground `serve` (`crates/allmystuff-service/src/lib.rs:162-218`). Updater assets are named for only the five release targets below, with `unknown` elsewhere (`crates/allmystuff-updater/src/lib.rs:1827`). Linux asset selection does not distinguish GNU from musl.
- Manifests declare Rust 1.88.0, while `rust-toolchain.toml` selects floating `stable`. Neither CI nor this review proves the declared minimum Rust version. This host reports Rust/Cargo 1.97.1, `x86_64-pc-windows-msvc`.

## Evidence-qualified support matrix

“Configured” means checked-in workflow instructions, not a successful historical run. No GitHub workflow execution or released artifact was audited. General CI uses OS aliases without explicit architecture, so it does not qualify every architecture listed in release configuration.

| OS / architecture | Source assessment and likely constraints | Configured workflow coverage | Actual evidence in this review |
|---|---|---|---|
| Linux x86_64 GNU | CLI/foreground node/desktop paths exist; default node needs native media libraries; GUI needs WebKitGTK and a display. | Release explicitly builds CLI, terminal, node and GUI for `x86_64-unknown-linux-gnu` on Ubuntu 22.04; Linux CI tests root/default node, checks capture-less node, checks/tests GUI, and lints/tests `hwenc`. | No Linux build or runtime run inspected. Headless operation remains unqualified. |
| Linux aarch64 GNU | Same intended paths; GUI has architecture-specific workaround. | Release target `aarch64-unknown-linux-gnu` on `ubuntu-22.04-arm`; no dedicated aarch64 ordinary CI entry. | No build, Pi/ARM device, or no-display run inspected. |
| macOS aarch64 and x86_64 | Foreground node, launchd and desktop paths exist; native capture permissions and frameworks still apply. | Both explicit Darwin release targets on `macos-14`; macOS alias CI tests default node/root and GUI backend. No macOS capture-less check. | Neither architecture run inspected; cross-target compilation would not establish hardware/runtime behavior. |
| Windows x86_64 MSVC | CLI, foreground node, GUI and SCM paths exist; service agent/desktop-session assumption above. | `x86_64-pc-windows-msvc` release plus Windows alias CI for root/default node/GUI. No Windows capture-less CI check. | Root workspace tests passed, run E1 below. Node dependency resolution failed, E2. No node/GUI/media/service runtime evidence. |
| Android aarch64 | Embedded mobile node + audio is intended; full mesh needs Android native toolchain. | `gui-mobile` checks **shell only**, `--no-default-features --target aarch64-linux-android`; it disables the embedded mesh. | No Android build/device run inspected. Linux `node --no-default-features` is not equivalent to the mobile `audio-io` configuration. |
| iOS aarch64/device or simulator | Embedded node/mesh path is documented; Xcode/signing/native permissions remain prerequisites. | No iOS job found. `docs/MOBILE.md` documents device and simulator toolchains. | No build, signed app or device evidence. Standalone `serve` is not a supported claim. |
| Windows ARM64; Linux musl, ARMv7, RISC-V; BSD/other desktop Unix | Candidates for future source evaluation only. Dependencies, updater assets and service integration need qualification; unsupported service backend does not prove foreground node compilation. | No matching explicit release or ordinary CI target found. | Unverified; do not publish support claims. |
| Browser/Wasm | Current node requires native processes, files, local IPC and native dependencies. | No native-node Wasm check found. | No `serve` path demonstrated; requires a separately designed adapter. |

Workflow evidence: `.github/workflows/ci.yml:15-42,70-142,147-198,207-240`; release target matrix and binary build commands: `.github/workflows/release.yml:22-37,196-244`. Desktop CI uses `ALLMYSTUFF_SKIP_SIDECAR=1`, so it does not test a real bundled mesh. CI triggers are pull requests and manual dispatch, not every push; older prose in mobile docs is broader than the current workflow.

## Actual checks and limitations

- **E1:** manager durable run `55c240f9-9dd5-4765-a284-9cc5312ad1c5`, `cargo test --workspace --locked`, source HEAD above, Windows x64, succeeded/exit 0 in **85.469 s** (316 unit tests and 3 doc tests). Inspected terminal state and logs. Root excludes node and both GUI workspaces; this does not verify their builds or live serving.
- **E2:** manager durable run `9eb3cb23-533c-4622-b1bb-73db0f9ed70c`, `cargo tree --manifest-path node/Cargo.toml --locked --no-default-features --target x86_64-pc-windows-msvc --edges normal --prefix none --format '{p}'`, same baseline on A2, failed/exit 101 in **22.656 s** because `node/Cargo.lock` needs updating. It did not compile anything. A2 owns diagnosis; the manager has deferred node checks behind that prerequisite.
- C1 independently observed the same lock refusal with `cargo tree --locked --offline --manifest-path node/Cargo.toml --no-default-features --target x86_64-pc-windows-msvc --edges normal,build --prefix none --format '{p}'` (shell exit 1). No lockfile was regenerated. No complete resolved feature/dependency closure is claimed.

E1/E2 were local hub runs; no remote transport or payload transfer applies. No application was launched and no service, archive branch, main branch, authentication policy, or old Documents checkout was changed.

## Minimal path and smallest preparation slice

Use the existing **foreground `allmystuff-serve` binary with node defaults disabled** as the candidate starting path. It already removes the GUI and capture backends, but it is a capture-less engine, not the final minimal foundation, and it loses PTY hosting with `host`. Preserve the existing process lifecycle, state ownership, daemon version checks, local socket permissions, and route/consent gates. Windows uses a fixed node pipe name (`node_control.rs:289`); setting a temporary state home alone does not isolate a Windows live test.

Recommended first functional slice, after A2's bounded lock diagnosis: make `Mesh::advertised_capabilities` consistently filter endpoints whose backing feature is absent. Test the emitted profile against this independently checked matrix; do not change the bridge's general hardware inventory contract merely to fit one consumer.

| Node configuration | Proposed emitted capability behavior |
|---|---|
| Default (`host`, `audio-io`) | Preserve the existing bridge list. `host` without `audio-io` is unreachable with the current manifest. |
| No defaults | Remove screen/camera sources, synthetic `control` **Input Sink**, `clipboard` **Clipboard Duplex**, and all Audio endpoints. |
| No defaults plus `audio-io` | Apply the same host-disabled removals, but retain microphone Audio Source, speaker Audio Sink and system Audio Duplex. |

In both reduced configurations, retain synthetic controller and physical keyboard/mouse/touchpad Input Sources, remote-desktop Display Sink, viewer Video Sink, connected display sinks, and storage endpoints. Preserve feature tags, identity and authorization decisions. Audio-only system capture falls back to the default microphone on macOS and other platforms including iOS (`node/src/audio.rs:518-543`); retaining that existing behavior does not qualify OS-loopback capture support. Decoder inclusion without `audio-io` does not make the dropping audio stub a usable playback sink.

Prospective implementation ownership: one manager-assigned worker owns `node/src/mesh.rs` and focused capability-profile tests; C1 supplies platform checks and C2 independently reviews. Coordinate before changing this shared file. Later dependency splitting would touch `node/Cargo.toml`, `node/src/lib.rs`, stubs and relevant call sites; separating PTY from screen/input capture is a useful subsequent headless step, not part of this initial slice.

An independent cross-check of C2's help finding supports an even smaller startup preparation: add a first-token `--help`/`-h`/`help` branch to the existing `run_cli_verb` in `node/src/bin/serve.rs`. Print static usage and return success before state/updater/logging/socket work. Preserve no-argument startup and all existing version/update/log/supervised/service/session-agent behavior; changing unknown arguments to errors should be considered separately. Pure argument-classification tests and a later direct-node help process check can verify this boundary. The CLI wrapper applies pending updates before dispatch, so the inert-help claim would apply to the direct node binary. This is preparation, not a media-free node implementation.

Local IPC restrictions (`node_control.rs:642-702`), remote offer/consent gates, public/claim-network defaults and existing service privilege behavior must remain unchanged. Internal media feature separation can proceed against this repository's existing seams independently of MyOwnMesh v1.0.0 finalization. Only changes replacing the mesh/signaling contract must wait for the relevant reviewed upstream API.

## Exact prospective checks

Manager requests have been sent for these three durable checks after the lock prerequisite. Run from the checkout root so `.cargo/config.toml` supplies its existing CMake policy compatibility setting:

```text
cargo check --locked --manifest-path node/Cargo.toml --no-default-features --bin allmystuff-serve
cargo check --locked --manifest-path node/Cargo.toml --no-default-features --features audio-io --lib
cargo check --locked --manifest-path node/Cargo.toml --bin allmystuff-serve
```

For a later capability change, exact focused suite commands are `cargo test --locked --manifest-path node/Cargo.toml --no-default-features --lib advertised_capabilities`, the same with `--features audio-io`, and `cargo test --locked --manifest-path node/Cargo.toml --lib advertised_capabilities`; these assume the new regression tests use that name and are **not present or run yet**. Run the appropriate existing authorization/node suites through the manager as part of that implementation's validation.

Repeat the requested compile configurations on granted Linux/macOS targets before broadening the matrix, with exact target triples and environment evidence. Capture dependency closure using `cargo tree --locked --manifest-path node/Cargo.toml --no-default-features --target <verified-triple> --edges normal,build --prefix none --format '{p}'` after lock resolution. The future minimal profile must exclude native capture, audio and codec packages from the resolved graph, not just omit their Cargo feature names.

No live startup command is requested now. A future `node/target/debug/allmystuff-serve --version` check is an inert binary probe; a real foreground test requires an isolated granted testbed, explicit test-owned mesh binary/state and verified socket isolation, plus exact stop/cleanup behavior. Never use `just restart`, service installation/restarts, or `--help` for this evidence. Do not replay an `outcome_unknown` run.
