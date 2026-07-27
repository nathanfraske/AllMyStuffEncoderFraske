# AllMyStuff Sandbox Harness

## Status

The sandbox cut is a headless, black-box AllMyStuff instance. It runs a
real `allmystuff-serve`, its pinned MyOwnMesh sidecar, and the production video
probe from a sealed portable bundle.

The dated smoke-test evidence and remaining limits are recorded in
`docs/SANDBOX-HARNESS-VALIDATION-20260726.md`.

The sustained moving-pattern profile and its next test slice are recorded in
`docs/SANDBOX-MOTION-VALIDATION-20260727.md`.

Sealed remote deployment and sandbox-to-sandbox video profiling are described
in `docs/SANDBOX-INTERBOX.md`.

The harness does not replace, stop, restart, or reuse the installed
AllMyStuff stack. It also does not use the installed identity or state.

## Why this exists

The older test tools attached to the installed node. That made every test
dependent on the installed GUI, backend, sidecar, state, and exact version
combination. A failed deployment or mismatched helper could interrupt the same
connection being used to administer the machine.

The sandbox creates another local stack with:

- a separate AllMyStuff node pipe;
- a separate MyOwnMesh control pipe;
- a separate MyOwnMesh state root and device identity;
- a separate AllMyStuff update and settings root;
- a sealed manifest with the hash and size of every executable and script;
- self-update disabled in both the node and the sidecar;
- an external video probe that reaches the pipeline through node IPC;
- exact process records that allow a later `Stop` to clean up only the
  processes started by that sandbox;
- optional protected-port checks for services such as AllMyAgents.

The default network mode is `Isolated`. It parks the built-in LAN local-claim
network before the sandbox node starts and removes that network from the
sandbox MyOwnMesh config. The node and sidecar still start and the external
probe still tests their local process boundary, but the sandbox does not
discover or advertise to LAN peers.

Use `-NetworkMode LocalClaim` only when the test needs a live parallel mesh
connection. That mode enables the product's existing mDNS-only local-claim
network. A fresh sandbox identity can then discover LAN peers and advertise
its own screens. This is an explicit test choice, not the safe default.

The socket overrides are local process configuration. They do not add or
change signaling messages, rendezvous, SDP, ICE, STUN, TURN, route messages,
or media formats.

## Build a bundle

Build from the integration worktree and pass the MyOwnMesh binary that matches
the repository's `.myownmesh-rev` pin.

```powershell
.\scripts\build-allmystuff-sandbox.ps1 `
  -OutputDir C:\t\ams-sandbox-bundles\run-001 `
  -MyOwnMeshPath C:\Users\Admin\AppData\Local\AllMyStuff\myownmesh.exe `
  -TargetDir C:\t\target-ams-sandbox `
  -Jobs 16
```

The builder derives the expected MyOwnMesh version from `.myownmesh-rev`,
checks the supplied sidecar's reported version, builds the field-instrumented
backend, video probe, bounded remote transport, sandbox node-control helper,
detached process launcher, leased remote worker, and `amst`, then records
source state and file hashes in `sandbox-bundle.json`.

The output directory must be empty. The builder never edits the installed
application.

## Tool inventory

| Tool | Purpose |
| --- | --- |
| `build-allmystuff-sandbox.ps1` | Builds one sealed bundle, validates the pinned MyOwnMesh version, and records every file hash and source-state entry. |
| `stage-allmystuff-sandbox.ps1` | Verifies a bundle and rotates it into the stable sandbox runtime without changing the installed application. |
| `configure-allmystuff-sandbox-firewall.ps1` | Shows, installs, or removes only the two named stable-path MyOwnMesh inbound rules. |
| `allmystuff-sandbox.ps1` | Starts, inspects, controls, probes, and stops one exact local sandbox instance. |
| `sandbox_process_launcher.exe` | Starts a long-lived sandbox process with file-backed logs and returns its PID without tying it to the bootstrap terminal. |
| `sandbox_remote_worker.exe` | Executes fixed sandbox actions from the Files inbox under a bounded lease and seals every result. |
| `sandbox-motion-source.ps1` | Starts, inspects, and stops one leased full-screen checker pattern with independently moving orange and purple targets. |
| `sandbox_node_control.exe` | Exposes only the local sandbox identity, network, exact-peer, and temporary display-grant operations needed by the pair runner. |
| `p2_remote_transport.exe` | Transfers files and performs the one bounded bootstrap over existing authenticated Files and terminal data routes. |
| `bootstrap-allmystuff-sandbox-remote.ps1` | Validates the target, manifest, firewall, worker, and request before invoking the stable sandbox runner. |
| `deploy-allmystuff-sandbox-remote.ps1` | Applies the local target policy, creates a sealed request, verifies the remote result, and downloads a sealed collection. |
| `test-allmystuff-sandbox-pair.ps1` | Creates a two-host test network, authenticates exact peers, grants temporary screen view, optionally starts bilateral motion sources, runs bilateral probes, preserves completed legs after an opted-in probe failure, and cleans up. |
| `video_prod_probe.exe` | Opens a production video route through the sandbox node IPC and records frame, pacing, content, route, and rewatch results. |
| `summarize_video_profile.py` | Summarizes one or more process-local JSONL traces without subtracting clocks across hosts. |
| `sandbox-fleet-policy.example.json` | Documents the local allowlist shape for exact remote targets and protected ports. |

The tools do not install a second GUI. Only the firewall helper requests
elevation, and only when its explicit `Install` action is used.

## Use one stable firewall path

Windows Firewall program rules are path-based. Launching `myownmesh.exe` from
a new versioned bundle directory can therefore raise another firewall prompt
even when an identical binary was already approved elsewhere.

Stage each sealed bundle into one stable runtime directory:

```powershell
.\scripts\stage-allmystuff-sandbox.ps1 `
  -BundleDir C:\t\ams-sandbox-bundles\run-001 `
  -RuntimeDir "$env:LOCALAPPDATA\AllMyStuffSandboxRuntime"
```

The staging script verifies every source hash and size, refuses to update
while a backend or sidecar from the stable directory is running, prepares the
replacement beside the runtime, and preserves the previous runtime under a
timestamped directory. The firewall rule remains attached to the unchanged
stable executable path.

Install the two stable inbound rules once:

```powershell
& "$env:LOCALAPPDATA\AllMyStuffSandboxRuntime\configure-allmystuff-sandbox-firewall.ps1" `
  -Action Install `
  -RuntimeDir "$env:LOCALAPPDATA\AllMyStuffSandboxRuntime"
```

`Install` verifies the staged MyOwnMesh binary against its sealed manifest,
then requests one elevation if the current shell is not already elevated. It
creates one TCP and one UDP program rule for Private and Public profiles. The
current profile list is an explicit parameter:

```powershell
-Profile Private,Public
```

Do not add Domain unless that profile is part of the reviewed test
environment. `Show` is read-only and does not require elevation. `Remove`
deletes only the two named harness rules and requests elevation.

## Start and inspect a sandbox

```powershell
$bundle = "$env:LOCALAPPDATA\AllMyStuffSandboxRuntime"

& "$bundle\allmystuff-sandbox.ps1" `
  -Action Start `
  -InstanceId local-a `
  -BundleDir $bundle `
  -NetworkMode Isolated `
  -ProtectedPort 5299,7777

& "$bundle\allmystuff-sandbox.ps1" `
  -Action Status `
  -InstanceId local-a `
  -BundleDir $bundle

& "$bundle\allmystuff-sandbox.ps1" `
  -Action Probe `
  -InstanceId local-a `
  -BundleDir $bundle `
  -ProbeArguments '--list'

& "$bundle\allmystuff-sandbox.ps1" `
  -Action Stop `
  -InstanceId local-a `
  -BundleDir $bundle
```

Pass only ports that are already listening and must remain owned by the same
process for the whole run. The harness records every listener address, owner
PID, process start time, and executable path before it launches the sandbox.
It checks the same facts after startup, during status and probe operations, and
after shutdown.

For an opt-in LAN transport test, change only the start action:

```powershell
& "$bundle\allmystuff-sandbox.ps1" `
  -Action Start `
  -InstanceId lan-a `
  -BundleDir $bundle `
  -NetworkMode LocalClaim `
  -ProtectedPort 5299,7777
```

The runtime record stores the selected network mode. `Status`, `Probe`, and
`Stop` act on that recorded instance and do not change its network policy.

The default startup window is 15 seconds because the existing production
profile runner uses that deadline for the pinned mesh daemon to bind. The
default shutdown window is 8 seconds, matching the current backend's daemon
startup and recovery window. Both values are command-line parameters and must
be reviewed for a release test protocol.

## State and recovery

The default Windows state root is:

```text
%LOCALAPPDATA%\AllMyStuffSandbox\<instance-id>
```

The durable runtime record is `sandbox-runtime.json`. It contains the exact
backend and sidecar PID, process start time, executable path, executable hash,
bundle commit, socket names, and protected listener snapshot.

If the shell that started the sandbox disappears, run `Stop` with the same
bundle, instance ID, and state root. The stop path validates the complete
process record before terminating anything. It refuses to act on a reused PID,
different path, changed hash, or changed start time.

On Windows, `sandbox_process_launcher.exe` starts the backend with file-backed
standard output and standard error handles. It also clears inheritance on the
launcher's console handles before creating the child. The caller can exit
without waiting for an anonymous pipe held by the long-running backend or
sidecar. The launcher writes its exact child PID before it exits.

Each instance keeps these primary diagnostic files:

```text
logs\sandbox-node.stdout.log
logs\sandbox-node.stderr.log
logs\allmystuff-node.log
artifacts\video-profile.jsonl
```

The first two contain process streams. `allmystuff-node.log` is the backend's
own structured log. `video-profile.jsonl` is the bounded process-local stage
trace when trace profiling is enabled.

The state root is not deleted on stop. That preserves the sandbox identity,
joined test networks, logs, and evidence for the next run. Delete or rotate it
only as a separate reviewed action.

## Cross-host test shape

A two-box test uses one sandbox instance per box:

1. Transfer the same sealed bundle through the already-authorized production
   AllMyStuff Files data path.
2. Use one bounded terminal bootstrap on each host to start its sandbox and
   sealed leased worker.
3. Keep the default `Isolated` mode while staging each box.
4. Join both fresh sandbox identities to one short-lived, high-entropy test
   network.
5. Require each endpoint to see exactly the other expected identity on an
   authenticated active ICE pair.
6. Create one temporary display-consume share grant in each sandbox so the
   exact peer may pull its screen.
7. Run `video_prod_probe` against the sandbox node pipe.
8. Keep the production AllMyStuff connection and the AllMyAgents listener in
   the protected snapshot.
9. Revoke both grants, leave the test network, stop only the sandbox
   instances, collect both sealed profiles, and stop both workers.

The sandbox connection is a separate MyOwnMesh session. Video and application
payload still travel only on the authenticated ICE data path selected by the
normal product. Nothing in the harness sends application data through
signaling.

## What this first cut proves

- Multiple AllMyStuff node pipes can coexist on Windows.
- Multiple MyOwnMesh control pipes can coexist using the v0.3.2 config field.
- The local-claim network can be kept off by default and enabled explicitly
  for a LAN transport test.
- The external production probe can target a selected sandbox node.
- A failed or stopped sandbox does not require the installed GUI or backend to
  restart.
- Exact binary and source identities survive remote handoff in one manifest.
- Protected local services can be checked before and after every operation.
- One AMD and one NVIDIA host can complete native and compressed H.264 routes
  in both directions through disposable sandbox identities.
- AMD and NVIDIA hardware Media Foundation encode are active on the tested
  hosts.
- NVIDIA NVDEC is active on the tested NVIDIA viewer.
- Automatic decode on the tested AMD viewer currently falls back to OpenH264
  software. AMD hardware decode remains a product gap.

## What is not in the first cut

The portable GUI sandbox is not enabled yet. The desktop app currently uses a
fixed Tauri identifier and the single-instance plugin. It also initializes
updater, autostart, tray, WebView storage, and installed-path behavior. A safe
GUI sandbox needs:

- a separate Tauri identifier, product name, and WebView data directory;
- a sandbox build flag that disables updater, autostart, service management,
  and installed-app handoff;
- the same local socket and state overrides used by the headless cut;
- a separate executable and bundle manifest entry;
- tests proving that launching it cannot focus or modify the production GUI.

This is implementable as a dedicated Tauri config overlay plus a compile-time
feature. It should follow the headless isolation proof, not precede it.

The inter-box controller now automates sealed Files transfer, one bounded
terminal bootstrap per endpoint, leased worker requests, dedicated network
creation, exact-peer admission checks, temporary display grants,
bidirectional probes, cleanup, and artifact collection. The standalone runner
still does not restart AllMyAgents or any production process. A protected-port
change stops the harness with evidence and leaves recovery to the owner.
