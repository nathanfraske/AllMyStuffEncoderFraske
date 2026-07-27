# Sandboxed inter-box testing

## Status

This is the implementation guide for running real AllMyStuff video tests
between disposable sandbox instances. Each endpoint has its own backend,
MyOwnMesh sidecar, identity, state, sockets, logs, and profiler output.

The installed AllMyStuff instance remains the bootstrap carrier. It transfers
the sealed tester through Files and uses one bounded terminal bootstrap per
endpoint to start a leased worker. Later requests and results use Files. Once
the sandbox endpoints are ready, the video route is sandbox to sandbox.

The dated validation record states which parts have been exercised on real
boxes. This guide does not treat an untested target or GPU as validated.

## Transport boundary

The deployment has three distinct paths:

1. Local orchestration reaches the installed node through its existing local
   node-control socket.
2. Bundle files, worker requests, sealed results, and profile archives use the
   existing AllMyStuff Files application route. One short terminal command
   starts the sealed worker on each endpoint. Both application routes use the
   authenticated ICE data channel selected by MyOwnMesh.
3. Video tests run between the two sandbox identities on their own test
   network. Capture, encoded frames, input-free probe control, and decoded
   frames use the normal media route over authenticated ICE.

Normal MyOwnMesh signaling still exchanges the metadata needed to discover a
peer and negotiate ICE. The harness does not change that protocol. It does not
put bundle bytes, shell commands, profile samples, probe results, or video
payloads into signaling.

The harness adds no route, signaling, SDP, ICE, STUN, TURN, media, terminal, or
Files wire format. The only new process configuration is the local socket and
state selection for each sandbox.

The terminal carries only a fixed, short command that invokes the previously
uploaded bootstrap at an exact path. The initial bootstrap writes its full
result to a file and prints a short envelope containing the request identity,
remote path, byte size, and SHA-256. The controller downloads the full result
through Files and verifies the envelope before accepting it. This avoids PTY
line wrapping or repaint corrupting a large JSON result.

A terminal route is watched and disconnected on every exit path. If the PTY
does not complete its initial cursor-position handshake, no command has been
sent. The transport records one setup retry, disconnects the failed route,
waits one existing poll interval, and opens one fresh route. It never retries
after sending a command, so a lost response cannot replay a start, probe, or
control action.

The same bootstrap starts `sandbox_remote_worker.exe` from the sealed stable
runtime. The worker:

- records its PID, executable path, process start time, manifest SHA-256, and
  idle lease;
- accepts requests only from the fixed sandbox inbox path;
- invokes only the fixed stable bootstrap script;
- requires every request to match the stable manifest and exact target;
- writes a result file plus a companion seal with the request ID, host, target,
  size, SHA-256, and manifest SHA-256;
- exits after 900 seconds without a request;
- exits immediately after sealing a collection response;
- removes its process record on exit.

`Stage` and `Start` use the one terminal bootstrap. `Status`, `Control`,
`Probe`, `Stop`, and `Collect` use the worker and Files. A request is never
replayed after execution merely because its result download failed.

## Safety gates

Remote deployment requires a local fleet policy. A target is accepted only
when its canonical peer key occurs exactly once in that policy. The remote
bootstrap then compares the requested computer name with the target's real
`COMPUTERNAME` before staging or launching anything.

Copy the example and fill it from a fresh, read-only machine inventory:

```powershell
Copy-Item .\scripts\sandbox-fleet-policy.example.json `
  .\.sandbox-fleet-policy.json
```

The local policy is intentionally excluded from Git. It contains deployment
identities and machine names, not product source.

Every start records the PID, start time, executable path, and executable hash
of the sandbox backend and sidecar. It also records every installed
AllMyStuff, MyOwnMesh, and AllMyAgents process as protected. A requested
protected TCP port must keep the same listener owner for the whole operation.
Status, probe, control, and stop operations fail if those facts change.

The runner stops only a process that still matches its complete sandbox
record. It refuses a reused PID, changed executable, changed hash, or changed
start time.

## Strict isolation

`Isolated` now parks every saved network in the sandbox state, including the
built-in local-claim network and any test network left from an earlier run.
It is not limited to a fresh state directory.

`LocalClaim` enables only the built-in LAN claim network.

`TestNetwork` enables only the exact network IDs passed through
`AllowedNetworkId`. Every other saved network remains parked. A test network
must already exist in that sandbox state before it can be enabled on a later
start.

The pair runner begins both endpoints in `Isolated`, obtains their fresh
identities, creates one dedicated test network, joins both, and then requires
each side to see exactly the other expected peer on an authenticated active
ICE pair. Any additional peer aborts the run before a video route opens.

Fresh sandbox identities are not owners or fleet members. After the exact peer
checks pass, each sandbox creates one temporary share grant allowing the other
identity to consume display media. The grant does not cover input, terminal,
files, clipboard, sites, camera, or audio. Teardown removes both grants before
either sandbox leaves the test network.

Share invites and route control use the existing application
`ChannelSendTo` request on `CHANNEL_CONTROL`. That request is carried by the
authenticated MyOwnMesh data channel. The harness does not place the grant,
route, or media payload in signaling.

The test network ID concatenates four independent values returned by
MyOwnMesh's `NetworkIdGenerate` request. MyOwnMesh v0.3.2 derives each
eight-character component from eight `OsRng` bytes. The most likely character
has probability 1/32, so four components provide a 160-bit min-entropy lower
bound. The value is transferred only through the authenticated bootstrap
route and is removed from both sandboxes during normal teardown.

AllMyStuff currently turns on auto-approval for ordinary non-fleet networks.
The harness does not pretend that manual roster approval remains in force.
Instead it uses the high-entropy, short-lived network ID, exact expected peer
keys, the v0.3.2 authenticated channel binding, and the extra-peer abort gate.

## Stable firewall path

Each box stages the current bundle at:

```text
%LOCALAPPDATA%\AllMyStuffSandboxRuntime
```

Windows Firewall program rules remain attached to that stable
`myownmesh.exe` path across bundle updates.

The remote bootstrap checks for the two exact inbound rules before launch. If
the existing AllMyStuff terminal is already elevated, it can install the
reviewed Private and Public rules without opening another UAC prompt. If the
terminal is not elevated, the bootstrap stages the bundle but does not start
the sandbox. It returns `staged_firewall_required`. It never creates a remote
UAC dialog that could block an unattended test box.

## Profiling collected from both endpoints

Sandbox builds compile the `field-telemetry` feature and opt it in at runtime.
Each backend records:

- process and total CPU once per second;
- WDDM 3D, video encode, video decode, copy, and dedicated VRAM counters;
- per-media-thread CPU when a registered stage is busy;
- monitor topology and topology changes;
- verbose backend and MyOwnMesh logs;
- bounded process-local pipeline events for capture, conversion, encode,
  outbound queues and writes, inbound parsing and dispatch, decoder queues,
  decode, viewer polling, and local IPC delivery;
- p50, p95, p99, average, and maximum stage summaries;
- exact source, sink, route activation, codec delivery mode, frame pacing,
  keyframe, payload, dimension, fingerprint, green-band, black-row, and
  optional motion-palette results;
- selected ICE pair, authentication state, RTT, and TURN use from both peer
  views.

The exact event trace is capped at 100,000 events per endpoint in the pair
runner. Its writer uses the existing bounded non-blocking queue, so profiler
load cannot backpressure video.

After teardown, each box creates a sealed ZIP containing only sandbox logs,
probe reports, runtime records, and profiler traces. It excludes MyOwnMesh
identity secrets, saved network configuration, credentials, and every
production state directory. The controller downloads each ZIP through the
Files application route and verifies its reported SHA-256 and size.

The controller expands and summarizes each trace independently, then produces
a combined text summary. It does not subtract monotonic timestamps from
different hosts. Those clocks have unrelated origins. Cross-box transit or
glass-to-glass latency still needs a synchronized external capture or a
reviewed data-plane timing experiment.

## Build a sealed tester

```powershell
.\scripts\build-allmystuff-sandbox.ps1 `
  -OutputDir C:\t\ams-sandbox-bundles\interbox-001 `
  -MyOwnMeshPath "$env:LOCALAPPDATA\AllMyStuff\myownmesh.exe" `
  -TargetDir C:\t\target-ams-sandbox-interbox `
  -Jobs 16
```

The manifest includes the backend, MyOwnMesh v0.3.2, video probe, bounded P2
transport, sandbox node-control helper, detached process launcher, runner,
leased remote worker, deployer, pair controller, firewall helper, artifact
analyzer, and every remote bootstrap script.

## Stage or start one remote sandbox

```powershell
& C:\t\ams-sandbox-bundles\interbox-001\deploy-allmystuff-sandbox-remote.ps1 `
  -Action Start `
  -BundleDir C:\t\ams-sandbox-bundles\interbox-001 `
  -PeerId '<exact target device id>' `
  -InstanceId 'sandbox-a' `
  -RunId 'smoke-001' `
  -PolicyPath .\.sandbox-fleet-policy.json `
  -NetworkMode Isolated `
  -Execute
```

`Stage` transfers and verifies the bundle without starting it. `Status`,
`Control`, `Probe`, `Stop`, and `Collect` reuse the stable runtime and require
the same source commit as the local bundle. A successful `Stage` or `Start`
response also proves that the exact worker is running. Later actions verify
that worker record before uploading a request.

## Run a complete two-box profile

The default five-second phase and two cycles are the existing
`video_prod_probe` defaults. Pass different reviewed values explicitly when a
test protocol calls for them.

```powershell
& C:\t\ams-sandbox-bundles\interbox-001\test-allmystuff-sandbox-pair.ps1 `
  -Action Full `
  -BundleDir C:\t\ams-sandbox-bundles\interbox-001 `
  -FirstPeerId '<exact first device id>' `
  -SecondPeerId '<exact second device id>' `
  -FirstInstanceId 'sandbox-a' `
  -SecondInstanceId 'sandbox-b' `
  -RunId 'pair-001' `
  -PolicyPath .\.sandbox-fleet-policy.json `
  -ArtifactDir C:\t\ams-sandbox-results `
  -Seconds 5 `
  -Cycles 2 `
  -Delivery both `
  -Execute
```

`both` runs native decode and compressed delivery in both directions. Tests
run sequentially so one diagnostic route cannot steal resources from the
other. Every probe tears down its exact generation-aware route.

Before the first probe, `Full` creates the two temporary display-consume
grants described above. It records both grant results in the pair session.

`Full` removes both grants, leaves the dedicated network, stops both exact
sandbox processes, downloads both artifact archives, verifies them, expands
them, and writes:

```text
pair-profile-summary.txt
sandbox-pair-session.json
```

An empty trace is valid evidence when a run stops before media begins. The
collector preserves that file without trying to summarize it. A nonempty
trace that contains no valid profiler events remains an error.

Use `Prepare` when a sandbox pair must remain online for a reviewed manual
step. Run `Teardown` against the emitted session file afterward. `Prepare`
does not provide a lease watchdog yet, so it must not be used as an unattended
long-running deployment.

## Current limits

- The remote deployer is Windows PowerShell 5.1 tooling. The underlying
  backend and socket overrides remain cross-platform, but macOS and Linux need
  native staging and firewall wrappers.
- The sandbox is headless. It validates the backend media pipeline and native
  decoder output, not WebView transfer, canvas upload, browser paint, or GUI
  interaction.
- Motion-palette scoring is meaningful only while the source displays the
  reviewed moving test pattern. Ordinary desktop interaction still provides
  frame pacing, fingerprint, banding, black-row, queue, encode, and decode
  evidence.
- A remote box whose installed AllMyStuff Files or terminal route is already
  unavailable cannot be bootstrapped from this controller. The harness does
  not add a second signaling or management backdoor.
- The installed terminal host currently retains exited shell sessions until
  its one-hour idle reaper runs. The leased worker limits a pair run to one new
  bootstrap shell per endpoint, but the product should still release exited
  sessions promptly.
