# Sandbox Harness Validation, 2026-07-26

## Verdict

The headless sandbox is implementable and the local prototype works.

It runs a real AllMyStuff 0.2.49 backend and MyOwnMesh 0.3.2 sidecar beside the
installed application. External tools reach the selected sandbox through its
own AllMyStuff node pipe. The sandbox has its own identity, state, mesh control
pipe, node pipe, logs, updater policy, and exact process records.

The safe default is peer-isolated. A live parallel LAN connection requires the
explicit `LocalClaim` network mode.

The full desktop GUI is not part of this cut. It is feasible, but it needs a
separate Tauri application identity, WebView state root, single-instance
namespace, updater policy, autostart policy, and installed-app handoff policy.
The headless cut proves the lower-risk process and transport boundary first.

## Integrated cut

| Component | Validated revision |
| --- | --- |
| Latest AllMyStuff upstream | `b5199eb`, tag `v0.2.49` |
| Media foundation input | `e031b7d` |
| Integration merge | `f182b5c` |
| Final harness commit | `a15b30e` |
| MyOwnMesh binary and source pin | `0.3.2` |

`upstream/main` is an ancestor of the final harness commit. The branch is 17
commits ahead and zero commits behind that upstream revision.

The media merge produced nine source or lockfile conflicts. Each source
conflict was combined so the upstream route and input durability work remains
present with the media pipeline changes. Cargo and frontend checks then
validated the combined APIs.

## Build and contract checks

| Check | Result |
| --- | --- |
| `allmystuff-protocol` tests | 56 passed |
| AllMyStuff node library tests | 352 passed |
| GUI behavioral tests | 35 passed |
| Svelte check | 0 errors, 0 warnings |
| Production frontend build | Passed |
| `allmystuff-term` check | Passed |
| Node library check | Passed |
| Tauri desktop Rust check | Passed |
| Release backend, probe, and `amst` build | Passed |
| PowerShell parser checks | Passed |

The node suite passed the host-gated NVENC, NVDEC, Media Foundation, D3D11VA,
OpenH264, input durability, route lifetime, reconnect, queue recovery, and
pipeline profiling tests that were available on this Windows host. This does
not substitute for AMD, Intel, macOS, Linux, or ARM hardware runs.

## Live isolation checks

The protected production snapshot at 22:32:29 CDT was:

| Process or listener | Identity before and after |
| --- | --- |
| Installed GUI | PID 3520 |
| Installed backend | PID 36672 |
| Installed MyOwnMesh | PID 9140 |
| AllMyAgents desktop | PID 17288 |
| AllMyAgents web listener | `[::1]:5299`, PID 28192 |
| AllMyAgents hub listener | `127.0.0.1:7777`, PID 4444 |

All listed process start times, executable paths, listener addresses, ports,
and listener owner PIDs stayed unchanged through startup, probe, and shutdown.

### Default isolated mode

Final isolated run:

| Field | Observed |
| --- | --- |
| Local sandbox node | `bgoqz4px...-626F3` |
| Remote screen sources | 0 |
| Selected ICE paths | 0 |
| Live configured networks | 0 |
| Parked networks | `allmystuff-local-claim-v1` |
| Parked file shape | JSON array, accepted without quarantine |
| Protected production processes | 4 |
| Protected AllMyAgents listeners | 2 |

This proves a developer can start and probe the local backend and sidecar
boundary without advertising the sandbox or connecting it to LAN peers.

### Opt-in parallel LAN mode

Two `LocalClaim` instances ran at the same time:

| Field | Instance A | Instance B |
| --- | --- | --- |
| Local node | `dy2cgu4...-ACC04` | `ryo6mvi4...-A03CD` |
| Node PID | 11940 | 24460 |
| Mesh PID | 3048 | 27496 |
| Remote screen sources | 13 | 13 |
| Selected ICE paths | 6 | 6 |
| Active authenticated paths | 6 | 6 |
| Protected production processes | 4 | 4 |
| Protected AllMyAgents listeners | 2 | 2 |

The instances used different node and mesh pipe names. Stopping instance B
left instance A, the installed stack, and both AllMyAgents listeners alive.
Stopping instance A then left only the original installed stack. No remote
input or video route was opened during this smoke test.

## Defects found by the smoke test

1. Listener snapshots used PowerShell ordered dictionaries. The same listeners
   could serialize in a different order and raise a false protection failure.
   The records now use sortable typed objects.
2. A one-item disabled-network list serialized as an object instead of an
   array. The backend correctly quarantined it, then re-enabled local claim.
   Array shape is now preserved and the final isolated probe saw zero peers.
3. Versioned bundle paths caused repeated Windows Firewall prompts. The active
   rules confirm that Windows created a separate TCP and UDP program rule for
   each versioned `myownmesh.exe` path.

## Stable firewall path

The final bundle was staged twice at:

```text
C:\Users\Admin\AppData\Local\AllMyStuffSandboxRuntime
```

Both staging passes verified every source hash and size. The second pass
preserved the prior runtime in a timestamped sibling directory before placing
the same sealed cut at the stable path.

The one-time firewall helper is present and its read-only `Show` path passed.
Its non-elevated failure path also passed. The actual rule installation has
not been run because the current shell is medium-integrity and installing the
rules requires one UAC approval.

The proposed rules are:

- exact program path:
  `C:\Users\Admin\AppData\Local\AllMyStuffSandboxRuntime\myownmesh.exe`;
- inbound TCP and inbound UDP;
- Private and Public profiles;
- no Domain profile unless separately reviewed.

The active Ethernet connection on this host is Public. A Private-only rule
would not prevent the prompt here.

## Boundary review

The harness-specific Rust changes add only two process-local endpoint
overrides:

- `ALLMYSTUFF_MESH_SOCKET`;
- `ALLMYSTUFF_NODE_SOCKET`.

They select local filesystem sockets on Unix or local named-pipe segments on
Windows. The harness changes do not add or modify rendezvous messages, route
messages, SDP, ICE negotiation, STUN, TURN, media payloads, or peer wire
formats.

`LocalClaim` uses the product's existing mDNS discovery and authenticated ICE
data path. The harness does not place application or media data on signaling.

## Remaining validation

The following work is still required before calling this a complete remote
test platform:

1. Approve the two stable firewall rules once, then launch the stable runtime
   and confirm that no firewall prompt appears.
2. Stage the same sealed runtime on a second box.
3. Keep both instances isolated while staging.
4. Join the two fresh identities to a dedicated test network through the
   normal product flow.
5. Run an actual sandbox-to-sandbox video route, motion pattern, codec switch,
   decoder switch, reconnect, and forced sandbox crash.
6. Confirm the production AllMyStuff route and AllMyAgents listener remain
   usable throughout.
7. Repeat on AMD and Intel hosts. The current live smoke test was local
   Windows process and LAN path validation, not cross-vendor media evidence.

The current cut is suitable for the one-time firewall approval and the first
two-box sandbox media run. It is not yet evidence for cross-host video quality
or performance.
