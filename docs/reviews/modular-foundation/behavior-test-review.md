# Baseline behavior and test review

Author: Rivest / C2, session `87ba9bd9-4098-46c6-b97b-b7da68eedab2`.
Baseline: `b15aa1a277894999a8f01134843c06e892de084b` (0.2.121).
Worktree: `C:\Users\Admin\AppData\Roaming\AllMyAgents\data\worktrees\87ba9bd9`, branch `agent/87ba9bd9`.
Scope: source review and run proposals only; no daemon launch, credential inspection, or test execution by C2. C1 independently accepted the report, source findings and proposed help scope; its hardware-test clarification is incorporated below. Manager authorized help implementation separately after that review; it will be committed separately from this report.

## Findings that affect the first slice

1. Headless currently means no webview, not minimal dependencies or no media. The root workspace excludes node and GUI; node defaults to `host`. Turning defaults off substitutes capture/input/terminal/clipboard stubs but retains video decode, OpenH264, Opus, SQLite, networking, files, and the mesh/control engine (`Cargo.toml`; `node/Cargo.toml:48`; `node/src/lib.rs:28`). CI compile-checks this capture-less configuration on Linux; that is not a behavior or dependency-closure test.
2. There are no dedicated Cargo `tests/` integration-test directories or CLI test attributes in the reviewed tree. Numerous inline unit tests include real PTY, temporary-file, and socket round trips. Default invocation, CLI-to-node argument forwarding, and isolated `serve` lifecycle remain source-only findings here.
3. `gui/package.json:12` explicitly lists nine frontend test files and omits the tenth, `gui/src/stream-tune.test.mjs`. Its existing four tests cover settings held until accept, reconnect, clearing overrides, and source switch. Thus ordinary `pnpm test`/CI does not run those tests.
4. Authentication is distributed across graph rules, node ingress/dispatch, ownership, and consent. Extracting a feature must preserve the caller's gates as well as its implementation. A successful test of the pure `Session` state machine alone does not prove secure node dispatch.

## Existing test and CI inventory

Counts below are anchored source occurrences of `#[test]` and `#[tokio::test...]`, not discovered or executed test totals. They include target/feature-gated and ignored tests and exclude doctests. Count command: `rg -c --glob '*.rs' '^\s*#\[(test|tokio::test)(\]|\()' crates node gui/src-tauri/src gui/mobile/src`.

| Area | Source inventory | What it covers / limitation |
|---|---|---|
| Root workspace | 340 attributes: inventory 50, graph 19, protocol 64, bridge 9, session 43, updater 27, service 35, term 17, mobile-core 46, CEC protocol 16, CEC consent 14; CLI 0 | Platform inventory parsers, grant/route rules, serialization, state transitions, service command/rendering policy, updater policy, terminal framing, pure mobile control/media, consent persistence/expiry. Service tests do not establish a functioning installed service. |
| Node tree | 469 attributes, including node/pixels; mesh 96, video 55, ownership 20, node-control 17, CEC 14, files 14, terminal 14 | Mix of pure policy and actual local I/O. PTY/ConPTY shell round trips are in `node/src/terminal.rs`; file operations use temporary paths; Unix owner-only socket test binds a temporary socket. Windows `mediafoundation::tests::hardware_pump_is_lossless_and_decodable` is not ignored, but passes by early return if no MFT exists or opening fails (`mediafoundation.rs:1071-1082`); `hardware_paced_slices_are_real_cut_points` is ignored (`:1145`). A green suite or source count alone does not prove hardware execution. |
| Desktop shell | 30 attributes | Recovery decisions, local file operations, Wi-Fi parsers, window/icon behavior and command helpers. CI tests the shell, but the old PTY wording in the workflow does not move node's tests into this workspace. |
| Mobile shell | 2 attributes | `gui/mobile/src/engine.rs:426,480` contains a real embedded engine boot/dispatch/migration test with temporary identity/socket state and an adapter-state migration test. They are inline integration-style tests, not mere parser helpers. Android CI builds with `--no-default-features`, omitting this mesh engine; this does not demonstrate on-device mesh behavior. |
| Frontend | 10 `.test.mjs` files; 9 in package test script | Mouse/key/relative-motion forwarding, support provenance, file canvas, event cleanup, decode progress, screen shares and video polling/statistics. These are Node tests of helpers, not browser/device end-to-end coverage. |
| Wire fixtures | `contract-fixtures/README.md`, session `dump_kvm_fixtures` example | Existing Rust/Go contract JSON and a documented regeneration command. No local CI step verifies fixture freshness or runs the other repository's Go tests. That repository was not accessed. |

`.github/workflows/ci.yml` represents these gates, not evidence that they passed at this SHA:

- Root: fmt on Linux; Clippy and workspace tests on Ubuntu/macOS/Windows; `cargo run -p allmystuff-cli -- capabilities` inventory smoke on each.
- Node: default `cargo test` on the three desktop OSes; Linux fmt/Clippy, `--features hwenc` Clippy/tests, and `cargo check --no-default-features`. No capture-less runtime test or systematic feature powerset/dependency exclusion assertion.
- Frontend: Node 22, pnpm 10, frozen install, tests, typecheck, production build.
- Desktop Tauri: build frontend, then check/test on three OSes, with `ALLMYSTUFF_SKIP_SIDECAR=1` and `CMAKE_POLICY_VERSION_MINIMUM=3.5`. Stub sidecar builds are compile evidence only.
- Mobile: `cargo check --no-default-features --target aarch64-linux-android`; no iOS or engine-enabled mobile runtime validation in this CI file.

## Observable behavior to preserve or deliberately revise

- Bare `allmystuff` applies pending updates, then launches the discovered desktop GUI. Linux with neither `DISPLAY` nor `WAYLAND_DISPLAY` fails with actionable `serve`, service, scan, capabilities and update guidance. Missing GUI also fails with recovery guidance (`crates/allmystuff-cli/src/main.rs:25`; `gui_launch.rs:17`). Do not silently redefine the default desktop experience while introducing a minimal build.
- CLI `serve` locates the separate `allmystuff-serve` binary, forwards arguments, and uses `exec` on Unix; other targets wait and map the child's result to success/failure. Missing node emits a build/install/override hint. It does not automatically build a node (`crates/allmystuff-cli/src/serve.rs:23`; `allmystuff-service::find_serve_binary`).
- The node recognizes first-argument `--version`/`-V`/`version` and `update` before normal startup. **`allmystuff-serve --help` is not a safe help probe:** unrecognized arguments fall through to normal startup. Ordinary CLI help also reaches `apply_pending_if_any` before dispatch. Do not run bare CLI, `serve`, `serve --help`, or `update` against operator state to gather review evidence (`node/src/bin/serve.rs:160,218`; CLI `main.rs:28`).
- Normal serve applies pending updates, binds the node-control socket before starting the mesh, discovers/reuses or spawns and supervises a MyOwnMesh daemon, starts the mesh/control server and unattended updater, and waits for shutdown. Duplicate healthy-node, supervised standby, Windows re-exec handoff, and installed-versus-bundled ownership handling are distinct paths. A missing daemon can leave the node up and retrying; startup alone is not mesh readiness (`node/src/bin/serve.rs:406`).
- Current no-default-feature behavior is capture-less: video capture reports `GrabFailed`; audio capture/playback is inert without `audio-io`; viewer decoding remains. That is not yet a contract for a tiny media-free `serve` (`node/src/stubs/video.rs`; `node/src/stubs/audio.rs`).

## Authorization boundaries and evidence limits

| Boundary to retain | Existing evidence and qualifications |
|---|---|
| Trust an authenticated sender, not arbitrary presence/body fields | `node/src/mesh.rs:10230` requires claim-body owner to match transport sender; fleet-key handoff at 10351 accepts only the recorded owner. `sender_may_control` at 17335 trusts self, recorded owner, local admit records, or daemon signed-roster cache, not presence ownership gossip. `presence_owner_is_topology_not_fleet_membership` covers classification. |
| Claim only through deliberate local policy and durable state | `Ownership::claimable`, `set_claim_mode`, and `try_accept_claim` require claim mode/unowned state; acceptance rolls back when persistence fails. Arrival-network policy is checked before `Effect::Ownership` loses network provenance (`mesh.rs:4695,11627`). Public claims default off; independent LAN proof is a bounded fallback, with helper tests. Ownership tests cover public-default policy, fleet exclusion, key handoff and release. They do not demonstrate the full failed-persist/duplicate-claim ingress transaction. |
| Authorize the exact resource before auto-accept or I/O | Graph tests reject ungranted shared routes and wrong grant direction/capability. Node screens offers before Session auto-accepts (`mesh.rs:4721`), with pure tests for privileged refusal, room membership limited to call routes, mapped-drive pull tokens, and grants permitting only their own planes. Configuration/upgrade retain owner/fleet-only paths. Token-based shared fetch and mapped-root routes are narrower exceptions, not general Files permission (`mesh.rs:14709`; `files.rs:1832,1899`). |
| Bind input/file/terminal operations and destructive route controls to the live route's peer | Ingress handlers require a live route, correct route kind and canonical sender, plus current applicable authorization before privileged operations (`mesh.rs:14449,14709`). Reject/Refresh/Tune peer checks are tested in `allmystuff-session/src/lib.rs`; node applies an explicit peer gate before Teardown (`mesh.rs:4943`). Preserve gates when relocating either side of this boundary. |
| Preserve consent scope and revocation behavior | Consent tests cover Once not persisted, three-hour expiry, Forever reload, view-only rejection of Control, and immediate store revoke. Node admission evaluates live CEC consent; the input hot path can trust a previously admitted known technician until the approximately two-second consent sweep tears down the route (`mesh.rs:3660,17373`). Do not claim universal per-frame consent reevaluation or instantaneous end-to-end revocation. |
| Restrict local privileged IPC | Unix socket mode 0600 has a real temporary-socket test; service-backed Windows pipe code constructs a DACL for SYSTEM, administrators and the installing account (`node_control.rs:655,3075`). **Limitation:** Unix chmod failure logs a warning and still returns the listener; ordinary Windows path can use default security options. Negative ACL/error-path access is not covered here. |

Two source risks need explicit peer verification before choosing security-related implementation: `refresh_fleet_authorization` retains the prior authorization cache on RPC failure or empty roster (`mesh.rs:12241`), contrary to an adjacent broad fail-closed comment; and raw `Session::handle_route` Accept updates a route without comparing `from` to its peer (`session/lib.rs:593`). Node checks the Accept sender for pacing metadata, then still dispatches the message to Session (`mesh.rs:5143,5198`). This is a missing negative test and apparent sender-validation gap, not a claim that an end-to-end exploit was tested. Neither behavior should become a presumed security guarantee during extraction.

## Reviewed recipes and bounded durable run proposals

Use manager `start_run` / `inspect_runs`, exact baseline provenance, and retained run IDs. No execution below was performed by C2. C2 independently inspected these completed manager runs and retained logs:

- Root run `55c240f9-9dd5-4765-a284-9cc5312ad1c5`: `cargo test --workspace --locked`, HEAD `b15aa1a277894999a8f01134843c06e892de084b`, manager branch/worktree, local win32/x64. **Succeeded, exit 0; 316 unit tests and 3 doctests passed**, no failures/ignored tests. Started `2026-09-13T22:35:23.593Z`, ended `22:36:49.062Z` (85.469 s). Logs untruncated; consumed cursors stdout 22961 / stderr 8621. Run provenance records dirty=true with one non-baseline source-manifest entry; the manager reports all source remains at the baseline. The retained run should not be described as an entirely clean checkout. This run does not test node, GUI, service installation, or live mesh behavior.
- Node dependency graph run `9eb3cb23-533c-4622-b1bb-73db0f9ed70c`: `cargo tree --manifest-path node/Cargo.toml --locked --no-default-features --target x86_64-pc-windows-msvc --edges normal --prefix none --format '{p}'`, same HEAD on A2 worktree, local win32/x64. **Failed, exit 101**: node lockfile needs updating and `--locked` forbids it. Started `22:35:58.463Z`, ended `22:36:21.119Z` (22.656 s); cursors stdout 0 / stderr 306. No compilation or tests ran. A2 owns diagnosis; defer node runs until the manager resolves this prerequisite.

No remote transport/payload transfer applies to these local runs. Do not duplicate the root success, or replay an `outcome_unknown` result.

Reviewed sources: `Justfile` (`test`, `node-check`, `gui-check`, `setup`), `CONTRIBUTING.md:67`, `.github/workflows/ci.yml`, `.cargo/config.toml`. Root MSRV is 1.88; toolchain channel is moving `stable`, not a fixed compiler release. Record actual compiler version. Windows recipe is `powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/bootstrap.ps1`; Unix recipe is `bash ./scripts/bootstrap.sh`. Both provision a full development environment, including GUI packages; neither script names CMake. A missing PATH entry alone does not establish a missing installation, and this review supplies no invented dependency installer.

Local target for the following node/frontend proposals: `target_session="87ba9bd9-4098-46c6-b97b-b7da68eedab2"`, cwd owned worktree root. Manager may reuse its matching baseline checkout instead. Rust executable confirmed by manager: `C:\Users\Admin\.cargo\bin\cargo.exe`. Proposed timeouts are 1,800,000 ms for Rust and 120,000 ms for a single Node helper test.

| Priority / purpose | Exact executable and argument vector | Environment |
|---|---|---|
| Completed by manager: root baseline | Cargo executable above; `["test","--workspace","--locked"]` | Root `.cargo/config.toml` supplies `CMAKE_POLICY_VERSION_MINIMUM=3.5`; run provenance captures execution-environment keys/hash. Success evidence above. |
| First node gate: CI capture-less compilation, no node launch | Cargo; `["check","--manifest-path","node/Cargo.toml","--locked","--no-default-features"]` | `{"CMAKE_POLICY_VERSION_MINIMUM":"3.5"}` |
| Bounded policy tests: memory/temp-backed ownership suite | Cargo; `["test","--manifest-path","node/Cargo.toml","--locked","--lib","ownership::tests::"]` | `{"CMAKE_POLICY_VERSION_MINIMUM":"3.5"}`; default host dependencies still build. |
| Exact privileged-offer policy test | Cargo; `["test","--manifest-path","node/Cargo.toml","--locked","--lib","mesh::tests::privileged_offers_are_refused_exactly_when_unauthorized","--","--exact"]` | `{"CMAKE_POLICY_VERSION_MINIMUM":"3.5"}` |
| Exercise omitted existing frontend test without package installation | Manager-resolved Node 22+ executable; `["--test","--experimental-strip-types","gui/src/stream-tune.test.mjs"]` | `{}`; test imports only Node builtins and the local TypeScript helper. Resolve the executable before launch; do not guess its path. |

For full frontend CI reproduction, the existing sequence is `pnpm --dir gui install --frozen-lockfile`, `pnpm --dir gui test`, `pnpm --dir gui check`, `pnpm --dir gui build`, each retained separately. Full `cargo test --manifest-path node/Cargo.toml --locked` is deferred here: several `mesh::tests` call `Mesh::new`, which loads ownership, shares, consent and other default stores (`mesh.rs:2117,2161,2207; tests:21921,22923`). Use a verified disposable profile/testbed before broad node runtime tests; two state-dir overrides alone should not be assumed to isolate every home/data path. Targeted tests above avoid those constructors. Native prerequisite failures must be recorded as setup/build failures, not failing test assertions.

## Candidate slice and missing behavior tests

The manager selected **direct-node early help** as the smallest preparatory candidate, independently confirmed by C1. Recognize first-token `--help`, `-h`, or `help` in the node's early CLI-verb dispatch, print static usage and return success before service-environment configuration, pending update application, logging, sockets or daemon work. Preserve existing version/update dispatch, no-argument startup, `--log`, `--supervised`, Windows `--service`/`--session-agent`, and permissive unknown-argument behavior. Do not claim the CLI wrapper itself is inert. Pure dispatch tests and a manager-run direct-node help process check after the lock prerequisite should establish this boundary. No implementation is authorized by this report.

For the later capture-less/minimal separation, define advertised capabilities and unsupported-operation responses alongside dependency boundaries, retaining default host and CLI launch behavior. MyOwnMesh pin is currently `v0.3.21` in `.myownmesh-rev`; MyOwnMesh v1.0.0 remains evolving and was not independently inspected. This review supplies no new mesh API contract or cross-project compatibility claim.

When implementation touches these paths, add behavior tests with fake transports/resources and disposable state for:

- Minimal serve startup and shutdown with a fake daemon: no GUI/media initialization, honest readiness failure, one-node ownership, and no access to operator state. CLI subprocess coverage should use fixture executables to test missing binary, argument forwarding, unknown/help behavior and exit results.
- Disabled capabilities absent from presence/control exposure, with actual unsupported responses when requested; feature/dependency checks on default, no-default and supported opt-in combinations. Compile success alone cannot establish this.
- Wrong-peer Accept/Teardown, stale/replaced route IDs, forged owner/fleet metadata, failed ownership persistence, expired/revoked consent during active input, and roster failure/revocation outcomes. Assert prevented I/O/effects at the caller boundary, not just helper return values.
- Negative local IPC permission paths and scoped-file traversal attempts before claiming strict fail-closed operation.

Do not broaden this initial slice to service installation, production mesh sessions, migration of archived work, or implementation solely to improve this inventory. C1 independently checked the inventory, source references, risk qualifications, filtered-test safety and direct-node help scope. Its hardware early-return clarification and the completed durable-run evidence are incorporated; no runtime exploit or new platform claim was accepted.

## Independent cross-check of C1 portability/capability draft

Read C1's report at `C:\Users\Admin\AppData\Roaming\AllMyAgents\data\worktrees\9b1bd3f4\docs\reviews\modular-foundation\headless-platform-review.md` and checked source independently. Its central mismatch is confirmed: `mesh.rs:13576` filters only screen/camera origins without host; bridge synthetic input/clipboard/audio and physical audio remain advertised despite inactive stubs. The following is the expected behavior of a future bounded filtering change, **not current tested output**:

| Feature configuration | Remove | Preserve |
|---|---|---|
| Default (`host` includes `audio-io`) | Nothing from the bridge list | Existing capabilities and direction/identity fields unchanged. |
| No defaults (`!host`, `!audio-io`) | Screen/camera sources, synthetic control **Input Sink**, OS **Clipboard Duplex**, every **Audio** endpoint | Synthetic controller and physical keyboard/mouse/touchpad/etc **Input Sources**; remote-desktop/connected-display **Display Sinks**; viewer **Video Sink**; existing Storage endpoints. |
| No defaults + `audio-io` | The same host-disabled screen/camera/control-sink/clipboard endpoints | Above controller/viewer/storage endpoints, plus microphone **Audio Source**, speaker **Audio Sink**, and system **Audio Duplex**. |
| `host` without `audio-io` | Not a reachable feature combination | `host` explicitly includes `audio-io`. |

Directions verified in `crates/allmystuff-bridge/src/lib.rs:69-241`; stubs/gates in `node/src/lib.rs`, `node/src/stubs/input_inject.rs`, `stubs/clipboard.rs`, `stubs/audio.rs`. Audio-only system capture on macOS/iOS/other non-Windows/Linux targets falls back to the default input (`node/src/audio.rs:518-543`); preserving that source behavior is not proof of OS-loopback support. Decode remaining compiled without audio I/O does not establish usable playback.

C1's platform matrix appropriately separates five explicitly configured release targets, OS-alias CI, root Windows test evidence and unverified runtime/device combinations. Independently checked `.github/workflows/release.yml:22-37`, Android CI's mesh-disabled shell target, and `gui/mobile/Cargo.toml`'s embedded node with `audio-io`. No other platform support claim is approved by this cross-check. Findings sent directly to C1 and manager with `wake=false`; neither peer worktree nor source was modified.
