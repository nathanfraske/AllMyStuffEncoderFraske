This handoff characterizes `MESH03-UP-EOF-01` in draft MyOwnMesh PR135 at
`db7818e09fedd98899490347b86ac9bc9f97b59b` (tree
`5492d1845e15dbc067acec3ec592bb9500b9e21d`). The AllMyStuff preparation base is
`41ff847d2397af12da33750d02daaaff4a74e6a1`. Source inspection supports the gap;
native reproduction is pending. This is an issue packet for review, with no
upstream posting or upstream implementation change.

An Events connection is the lifetime owner of its daemon-issued client ID and
capability C. On a quiet daemon, closing that connection should make its
authority unavailable to later commands and retire its owned registrations.
The candidate Events loop receives only the writer, waits on its two mailboxes
and cancellation, and never observes the peer's read EOF. The cancellation
flag in turn depends on registry removal, which normally happens after that
loop returns. A completely quiet disconnect therefore has no observed branch
that starts this cleanup.

The exact source chain is:

| Source at db7818e | Observation |
| --- | --- |
| [control.rs:1196](https://github.com/nathanfraske/MyOwnMeshSecurityReview/blob/db7818e09fedd98899490347b86ac9bc9f97b59b/crates/myownmesh/src/control.rs#L1196) | The connection reader and writer are split. |
| [control.rs:1370](https://github.com/nathanfraske/MyOwnMeshSecurityReview/blob/db7818e09fedd98899490347b86ac9bc9f97b59b/crates/myownmesh/src/control.rs#L1370) | A sent Events ACK enters `run_events_stream` with the writer; unregister and owned-registration release follow its return at line 1409. |
| [control.rs:3053](https://github.com/nathanfraske/MyOwnMeshSecurityReview/blob/db7818e09fedd98899490347b86ac9bc9f97b59b/crates/myownmesh/src/control.rs#L3053) | The stream selects runtime/registry cancellation and frame sources, without reading the socket. A later write could discover a closed peer; silence supplies no such write. |
| [control.rs:335](https://github.com/nathanfraske/MyOwnMeshSecurityReview/blob/db7818e09fedd98899490347b86ac9bc9f97b59b/crates/myownmesh/src/control.rs#L335), [registry.rs:843](https://github.com/nathanfraske/MyOwnMeshSecurityReview/blob/db7818e09fedd98899490347b86ac9bc9f97b59b/crates/myownmesh/src/ipc/clients/registry.rs#L843) | Owner cancellation waits for a flag cleared by unregister; it does not independently monitor the OS connection. |
| [dispatch/realtime.rs:113](https://github.com/nathanfraske/MyOwnMeshSecurityReview/blob/db7818e09fedd98899490347b86ac9bc9f97b59b/crates/myownmesh/src/control/dispatch/realtime.rs#L113) | Close authenticates ID+C before looking up F. Unknown F and invalid C therefore distinguish retained authority from removal, without opening a real flow. |

The bounded raw-wire reproduction uses a new infrastructure-only daemon, with
all network participation and infrastructure services disabled. It never opens
a network, peer, realtime flow or media pipe. It uses
[quiet_eof.rs](../../../crates/allmystuff-mesh-control/tests/quiet_eof.rs) and
[mesh03_quiet_eof.py](../../../scripts/ci/mesh03_quiet_eof.py).

1. Wait for Status on the nonce endpoint. Require candidate version, zero joined
   networks and unsupported/empty realtime advert.
2. Subscribe subject Events A and observer Events B. Hold both raw streams and
   their private ACKs; do not serialize or print IDs/C.
3. On separate command sockets, submit
   `realtime_flow_close` with A's ID, wrong C and `flow_capability="not-issued"`,
   then with A's real C and that same never-issued F. Require respectively
   `invalid local client authority` and the exact unknown-F refusal.
4. Drop A's entire raw connection. Query its real C immediately on a fresh
   command socket. Keep B pending in a read for the explicitly selected quiet
   interval. Any traffic, EOF or read error on B fails quietness qualification.
5. Query A's real C again on a fresh socket after the quiet interval and confirm
   Status still answers. Record only fixed classifications and elapsed time.
   The current source predicts unknown F, meaning A's authority remains
   registered at that observation. The desired removal result is invalid C.
6. Reap only the checksum-pinned daemon this controller launched, then remove
   its nonce-marked private root. This final process cleanup is separate from
   the authority observation.

The raw probe has one ignored test, `isolated_quiet_events_eof`. It uses raw
`interprocess`/Tokio JSON sockets, independently of the library's EventSession
implementation. It bounds every connection/command, caps each response at
16 KiB and has a 90-second total deadline. Quietness accepts an explicit
100..10000 ms interval. It reports both immediate and later classifications;
no finite interval proves indefinite retention, an OS scheduling bound or
native flow retirement. The probe has no real flow to retire.

Manager-only native recipe, after independent packet review:

```sh
cargo test -p allmystuff-mesh-control --test quiet_eof --no-run --message-format=json --locked --offline
```

Select the exact `compiler-artifact.executable` for target `quiet_eof` and
`profile.test=true` from the complete Cargo JSON and hash it. Independently
retain the stock daemon build receipt mapping its supplied SHA256 to the exact
upstream source above. A `1.0.0` version string alone does not establish that
mapping. Do not invoke an installed binary for this test. Syntax/format/lint
gates remain central; no worker compiled or ran the probe.

Use Python 3.9+ and an existing short private parent protected for the native
executing account. The output directory must be new and outside that parent.
Use the final reviewed application commit. The following grant and event
capacity are the previously selected isolated infrastructure inputs documented
in [the existing recipe](../../../scripts/ci/mesh-control-isolated.md), not
product resource defaults:

```sh
python3 scripts/ci/mesh03_quiet_eof.py \
  --daemon "$mesh_stock_candidate" --daemon-sha256 "$mesh_candidate_sha256" \
  --probe "$mesh_quiet_probe" --probe-sha256 "$mesh_quiet_probe_sha256" \
  --app-commit "$mesh_reviewed_app_commit" \
  --private-parent "$mesh_private_parent" --output "$mesh_new_evidence" \
  --resource-grant 'accounted_memory_bytes=67108864,queued_bytes=8388608,socket_or_handle=64,native_transport_object=64,worker_or_task=64,callback_or_scheduled_work=256,storage_bytes=4194304,storage_object=128,relay_or_provider_allocation=64,parsing_or_cpu_work=1000000,opaque_dependency_residual=67108864' \
  --event-capacity 32 --quiet-ms 2000 --expect retained
```

On Windows call the verified Python executable with the same argument vector;
use double-quoted native paths and the one quoted grant. The controller invokes
exactly `--ignored --exact isolated_quiet_events_eof --nocapture --test-threads=1`
on its own copied probe. A normal direct test invocation leaves the sole case
ignored. There is no automatic retry with another budget or endpoint.

Exit zero means the explicitly selected observation and cleanup were recorded.
With `--expect retained`, it confirms the suspected defect for that measured
quiet interval; it is not a correctness pass for disconnect cleanup. If the
same source/platform returns invalid authority instead, the observation is
still retained but expectation fails. `--expect revoked` is an explicit future
qualification choice, not an instruction to repair or replace upstream now.

The controller reuses the unchanged guards and child helpers in
[mesh-control-isolated.py](../../../scripts/ci/mesh-control-isolated.py), LF SHA256
`82dece01deba45027125cc9fdff1d3f64f0358d93a1d5486e7bd075ee110829d` at
the preparation base. It verifies that normalized source hash before importing;
the receipt records the actual platform file hash too. Before even `--version`,
it sets a fresh empty private
`MYOWNMESH_HOME`, disables updates in environment/config and supplies only the
explicit grant to infrastructure mode. Both binaries are copied and rehashed.
The daemon uses a unique custom endpoint, never the default pipe/socket.
No shared daemon/process lookup, installed service or ACL operation exists.

Only fixed classification records enter probe logs; malformed/unexpected raw
responses are replaced with static diagnostic text. Daemon stdout/stderr are
discarded at child stdio, so upstream diagnostics cannot export C; this limits
startup-failure detail to exit status, probe classification and provenance.
Evidence retains complete version/probe streams, their hashes/byte counts,
selected private config/grant, native facts and each owned child PID/exit/stop
mode. Never export the private identity/state directory.

Unix teardown sends SIGTERM only to the Popen-owned daemon and requires exit
zero; a timeout/forced kill fails normal-shutdown qualification and preserves
private state. Windows hidden CLI children cannot safely receive Ctrl-C through
this harness, so only the owned child is terminated/reaped and the receipt
truthfully reports `normal_shutdown:false`. That is a forced process cleanup
observation. Custody/reap failures preserve the private root and fail; neither
outcome means quiet EOF retired a native flow.

`MESH03-UP-OPEN-02` is a related source limitation, not a second runtime claim.
[control.rs:1953](https://github.com/nathanfraske/MyOwnMeshSecurityReview/blob/db7818e09fedd98899490347b86ac9bc9f97b59b/crates/myownmesh/src/control.rs#L1953)
arms a provisional flow handoff while writing the ACK.
[handoff.rs:67](https://github.com/nathanfraske/MyOwnMeshSecurityReview/blob/db7818e09fedd98899490347b86ac9bc9f97b59b/crates/myownmesh/src/control/handoff.rs#L67)
rolls back exact F on server failure/cancellation but commits on server
`Wrote::Sent` after write/flush. That does not establish client receipt of F.
A caller can lose its ACK after the server commits and remain uncertain about
its unknown flow. A pending-open adapter may still learn a late F and close it;
a genuinely unavailable F cannot safely be replaced by retrying a label or
closing peer/label coordinates.

This infrastructure reproduction cannot produce a successful FlowOpen: there
are no peers, network or media profile. An unknown-network refusal, malformed
request or an ACK deliberately read by this probe would not demonstrate the
lost-successful-ACK case. It therefore executes no FlowOpen, duplicates no
media and records `ack_loss_flow_open=not_executed_requires_real_flow`.
The one proposed real-flow follow-up belongs with the independently reviewed
paired harness: observe a successful server-side handoff at a controlled relay,
withhold that ACK from its client, retain the owner, then check exact
cleanup/recovery without automatically retrying the open. It requires its own
feasibility/ownership qualification and is not an instruction to execute here.

An upstream repair should make peer EOF initiate exact-owner cancellation and
await its existing registration/flow cleanup while the mesh is quiet, with
tests for EOF before/after ACK, registry shutdown races and owned native-flow
retirement. It must preserve per-client authority and unrelated clients.
ACK ambiguity separately needs a supported reconciliation/retirement contract
or an honest uncertain outcome. This packet changes neither upstream behavior
nor AllMyStuff's application authorization, consent or ownership decisions.

Actual native result and durable run IDs are pending manager execution. A2's
Oct16 source packet reverified PR135/tree identity and the source chain; C1
independently read the relevant immutable sources and prepared this diagnostic.
No published release, successful native reproduction, missing-flow cleanup or
unbounded-leak claim follows from the source review alone.
