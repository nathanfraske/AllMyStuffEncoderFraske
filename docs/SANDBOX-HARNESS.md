# AllMyStuff Sandbox Harness

## Status

The first sandbox cut is a headless, black-box AllMyStuff instance. It runs a
real `allmystuff-serve`, its pinned MyOwnMesh sidecar, and the production video
probe from a sealed portable bundle.

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
checks the supplied sidecar's reported version, builds the backend, video
probe, and `amst`, then records source state and file hashes in
`sandbox-bundle.json`.

The output directory must be empty. The builder never edits the installed
application.

## Start and inspect a sandbox

```powershell
$bundle = 'C:\t\ams-sandbox-bundles\run-001'

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

The state root is not deleted on stop. That preserves the sandbox identity,
joined test networks, logs, and evidence for the next run. Delete or rotate it
only as a separate reviewed action.

## Cross-host test shape

A two-box test uses one sandbox instance per box:

1. Transfer the same sealed bundle through the already-authorized production
   AllMyStuff data path.
2. Start each sandbox with a different instance ID and state root.
3. Keep the default `Isolated` mode while staging each box.
4. Join both fresh sandbox identities to a dedicated test network.
5. Approve those identities through the normal local ownership and roster
   flow.
6. Run `video_prod_probe` against the sandbox node pipe.
7. Keep the production AllMyStuff connection and the AllMyAgents listener in
   the protected snapshot.
8. Stop only the sandbox instances when the run is complete.

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

The first cut also does not automate test-network creation, roster approval, or
remote deployment. Those actions change mesh membership and remain explicit.
It does not restart AllMyAgents or any production process. A protected-port
change stops the harness with evidence and leaves recovery to the owner.
