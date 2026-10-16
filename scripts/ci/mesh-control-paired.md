This recipe runs two **new private foreground daemons** and the opt-in library
or node realtime adapter. The ordinary product still selects MyOwnMesh v0.3.21;
this does not activate the candidate in an installed app. H264 and Opus remain
codecs. Application consent, current route approval and ledger access remain
application responsibilities; an Events capability owns daemon resources only.

The application extraction base is
`41ff847d2397af12da33750d02daaaff4a74e6a1`. Use the final assembled task commit
for builds and receipts. The reviewed candidate is the unmerged, draft
[PR135](https://github.com/mrjeeves/MyOwnMesh/pull/135) at exact source
`db7818e09fedd98899490347b86ac9bc9f97b59b`, not a published v1 release. A version
string alone is not a source-to-binary mapping. Native execution, setup and
workflow dispatch belong to the manager; the commands below are prerequisites
and recipes, not reports that the gates ran.

| Gate | Concrete assertion | Limit |
| --- | --- | --- |
| `realtime_contract` | Literal fake endpoints exercise capabilities, exact F ownership, framing, inclusive/aggregate bounds, cancellation, uncertain ACKs and idle local custody | Does not execute a daemon or deliver RTP |
| `prepare_realtime_pair_vectors` | Existing source OpenH264 and software Opus generate and decode three 64x64 pictures / six 20 ms mono packets; AMS AU identity SEI is present | Validates this native encoder's output, without devices or a peer |
| `isolated_realtime_pair` | Real daemon-to-library AV in both directions, audio after video close, native codec refusal, daemon frame-ceiling pipe termination, polled-read cancellation, owned B restart, stale generation refusal and fresh delivery | Complete local writes alone are never delivery evidence; no codec decode in the portable crate |
| `isolated_realtime_consumer_pair` | Actual node facade over catalog-approved Session Offer/Accept routes, independent AV sends, received-byte/SEI/clock checks, received software decode, peer replacement and app context/epoch refusal | No hardware capture, render, playback, service, UI or ledger acceptance |

The source helpers are separate: `tests/support/paired.rs` has no codec import;
`node/tests/support/mesh_realtime_codec.rs` uses the node's existing codec graph.
The portable realtime feature is off by default. A transport-only application
need not depend on node, GUI, capture or codec libraries.

Use separate clean source checkouts and a dedicated short Cargo target per
checkout. Retain native hostname, OS/distribution, architecture, account,
`git rev-parse HEAD`, `cargo --version`, `rustc -vV`, compiler/linker and CMake
versions before building. Linux acceptance requires native Linux rather than
WSL; inspect the actual kernel/distribution. Intel macOS acceptance requires
actual `x86_64` evidence. A gateway's run metadata is not native target evidence.

The stock candidate build selects only the daemon, without GUI/default features:

```sh
git clone --single-branch --branch v1.0.0-implementation https://github.com/nathanfraske/MyOwnMeshSecurityReview.git "$mesh_new_upstream_checkout"
git -C "$mesh_new_upstream_checkout" checkout --detach db7818e09fedd98899490347b86ac9bc9f97b59b
git -C "$mesh_new_upstream_checkout" rev-parse HEAD
```

In that new checkout, honor its `rust-toolchain.toml` (Rust 1.88) and record
the exact source/lock status before and after:

```sh
cargo build --locked --release --no-default-features -p myownmesh --bin myownmesh
```

The [candidate manifest](https://github.com/nathanfraske/MyOwnMeshSecurityReview/blob/db7818e09fedd98899490347b86ac9bc9f97b59b/crates/myownmesh/Cargo.toml#L17)
keeps `transport-lab` out of the stock artifact. The controller requires an
independent JSON build receipt containing at least:

```json
{"source_commit":"db7818e09fedd98899490347b86ac9bc9f97b59b","sha256":"<exact native executable SHA256>","default_features":false,"features":[]}
```

Record native Rust, exact Cargo arguments, clean source status and unchanged
Cargo.lock in that receipt too. Do not infer provenance from a matching version
or copy an installed daemon. The controller only copies and hashes supplied
executables; it never downloads, builds, installs or updates one.

Build both new application integration executables at the final task commit:

```sh
cargo test -p allmystuff-mesh-control --features realtime --test realtime_paired --no-run --message-format=json --locked
cargo test --manifest-path node/Cargo.toml --no-default-features --features mesh-realtime --test mesh_realtime_paired --no-run --message-format=json --locked
```

Select each `compiler-artifact.executable` by the exact target name and
`profile.test=true` from complete Cargo JSON. Hash the actual executables; never
guess a Cargo hash suffix. The consumer uses the captureless configuration but
still links software decode/Opus and its dev-only source OpenH264 generator.
Set `CMAKE_POLICY_VERSION_MINIMUM=3.5`, as in the existing
[node CI recipe](../../.github/workflows/ci.yml#L76). On Windows use the verified
Visual Studio BuildTools CMake path if it is outside PATH; preserve exact CMAKE
and native tool versions in the build receipt.

The existing narrow [Linux setup](mesh-control-setup-linux.sh) has `library`,
`captureless` and `host` modes. Library uses Rust/linker tools; the consumer needs
the captureless mode's CMake in addition. Supply a manager-observed exact native
`cmake=VERSION` when required by that script. Full host checks use its existing
media package list, which comes from [bootstrap.sh](../bootstrap.sh) and
[ci.yml](../../.github/workflows/ci.yml#L96); they are a separate gate, not a
reason to add media packages to the reusable control library. Keep task-local
`CARGO_HOME`/`RUSTUP_HOME` and their executable PATH in subsequent commands.

Windows needs a verified native Python 3.9+ interpreter, Git, Rust/MSVC and the
consumer CMake. A WindowsApps Python alias is not proof of an interpreter.
Discover known installed paths before provisioning. The existing
[bootstrap.ps1](../bootstrap.ps1#L20) supplies the reviewed Rustlang.Rustup and
Microsoft.VisualStudio.2022.BuildTools VCTools/Windows11SDK.22621 commands;
Git and Python acquisition must retain their independently reviewed primary
installer/catalog receipts if missing. Do not run the full GUI bootstrap for
this gate. On macOS use the existing Xcode command-line linker tools and a
verified CMake/Python; retain actual native versions and Intel architecture.

Create a short protected parent for the executing account and a separate new
evidence location. On Unix the parent must have no group/other permissions;
on Windows its existing DACL must protect this account's private state. The
controller does not widen permissions. Evidence must be **outside** that
private parent. All supplied output directories must be new; overwrites are
refused. For the generator, choose a new absolute child of a protected existing
parent and set only the child process environment:

```sh
AMS_MESH_PAIRED_VECTOR_OUTPUT="$mesh_new_vectors" "$mesh_node_probe" --ignored --exact prepare_realtime_pair_vectors --nocapture --test-threads=1
```

PowerShell's equivalent is to set `$env:AMS_MESH_PAIRED_VECTOR_OUTPUT` only in
the manager's owned run/helper process before invoking the exact executable.
The output has `video-0.h264` through `video-2.h264`, `audio-0.opus` through
`audio-5.opus`, and `vectors.json`. The manifest records each exact SHA256,
length, duration and source timestamp, plus software reference metadata. The
generator inserts the existing AMS `AuIdentity` prefix SEI (sequences 1000..1002,
Reset recovery) before hashing; it checks reference software decode. Those
source timestamps/key classifications are provenance, not invented wire fields.

Use the unchanged explicit TEST input [mesh-control-paired-policy.json](mesh-control-paired-policy.json),
SHA256 `e6729ca6d0887ec0b4ef2cd91da404ffb477b251e00a5a8c1381254c1a9a9fe6`.
The owner selected all eleven finite resource dimensions, network semantic,
scheduler/signaling/history/profile and client/time ceilings. These are test
inputs, not application defaults or a guarantee of admission. Refusal never
triggers a larger allocation or retry. H264 uses RTP 90000/channels0; Opus uses
RTP 48000/channels2 even for mono PCM, as required by
[RFC7587 section 7](https://www.rfc-editor.org/rfc/rfc7587.html#section-7).
The registered profiles are selected before peer connections; the stock
[serve entry point](https://github.com/nathanfraske/MyOwnMeshSecurityReview/blob/db7818e09fedd98899490347b86ac9bc9f97b59b/crates/myownmesh/src/cli/serve.rs#L325)
requires explicit resource funding, connector realtime enablement and profile.

Run the controller once for `transport`, then once with a new output for
`consumer`, retaining both native receipts. The supplied signaling port is the
owner-selected **18475**; a collision fails rather than finding another port.

```sh
python3 scripts/ci/mesh-control-paired.py \
  --daemon "$mesh_daemon" --daemon-sha256 "$mesh_daemon_sha" \
  --daemon-build-receipt "$mesh_daemon_build_receipt" \
  --probe "$mesh_transport_probe" --probe-sha256 "$mesh_transport_probe_sha" \
  --probe-kind transport --app-commit "$mesh_app_commit" \
  --policy scripts/ci/mesh-control-paired-policy.json \
  --vectors "$mesh_new_vectors/vectors.json" \
  --private-parent "$mesh_private_parent" --output "$mesh_new_transport_evidence" \
  --signaling-port 18475 --mode direct
```

For the consumer command change only the probe executable/hash, `--probe-kind
consumer` and new evidence destination. The controller uses separate private
homes, persisted identities, sockets/named pipes and an unguessable Open test
network. Peer A hosts only an explicitly funded loopback Nostr signaling
service. Before B starts, `paired_endpoint_ready` checks actual private Status,
profile/identity and the owned signaling service's running/listen address.
STUN, TURN, mDNS, public fallback, pinned peers and automatic approval are
explicitly off. No installed/default endpoint is probed. Both daemon invocations
use `myownmesh serve`, with config/home supplied in child-only environment;
private state precedes even `--version` because upstream updater settlement runs
before CLI parsing.

Each probe makes one explicit `network_connect_peer` with finite `wait_ms` and
requires actual ACTIVE. It does not blindly repeat ambiguous writes. Both
payload streams are received and compared, not inferred from successful writes.
The first units remain strict: upstream `add_track` does not provide a public
flow-specific bind-ready ACK and its pre-bind writer can return success without
packets. There is no hidden startup sleep, warm-up unit, first-frame discard or
retry to mask that possible window. A failed first-unit gate remains evidence.

H264 ingress is one whole Annex-B AU; the candidate assembles one received AU
and canonicalizes startcodes. Pinned rtp0.13 drops only AUD9/filler12 and packs a
complete SPS/PPS pair into STAP-A before the next nonparameter NAL. The fixture
checks that exact ordered pair fits the pinned 1188-byte payload MTU; comparison
allows only canonical startcodes and removal of types9/12. It never generally
filters/reorders/deduplicates parameter sets or AMS SEI6. All remaining NAL
payloads and AU identity/order must match before software decode. Opus packets
match byte-for-byte. See the
[pinned H264 writer](https://github.com/nathanfraske/MyOwnMeshSecurityReview/blob/db7818e09fedd98899490347b86ac9bc9f97b59b/vendor/webrtc-0.13.0/src/track/track_local/track_local_static_sample.rs#L134)
and [MTU](https://github.com/nathanfraske/MyOwnMeshSecurityReview/blob/db7818e09fedd98899490347b86ac9bc9f97b59b/vendor/webrtc-0.13.0/src/track/mod.rs#L10).
Received H264 marker means assembled IDR entry, while Opus preserves its RTP
marker. Compare wrapping relative RTP clocks (40 ms video=3600 ticks, 20 ms
audio=960), not source absolute microseconds or a zero RTP origin.

The bare transport gate closes video while continuing audio, verifies exact
`provider_configuration_invalid` for an unregistered H264 clock and tests the
owner-selected daemon frame ceiling. It requires remote pipe termination while
the event generation remains active, then audio progresses. The ceiling probe
is not a measurement of all resource-pressure fairness. It also cancels a
polled idle receive and requires permanent local refusal on that torn stream.
Retained leases and simulated refusal/uncertain-ACK cases remain in the separate
portable suite; no 64 MiB production allowance or RSS bound is implied.

For owned restart, the Rust probe atomically publishes `restart-ready` with its
custody nonce. Only the controller's peer-B Popen child is stopped/reaped;
the same checksum-pinned binary and B state restart, then an atomic
`restart-complete` lets the probe check stable identity and issue one reconnect.
An old F never resolves by label to the new session. The terminal observer must
report actual pipe/session ending; a local stale write is not acceptance. New
Events generations and fresh same-label flows then deliver new AV. The consumer
also invalidates current application approval/epoch and requires local refusal.

Explicit exact-F close is awaited. The source
[dispatch](https://github.com/nathanfraske/MyOwnMeshSecurityReview/blob/db7818e09fedd98899490347b86ac9bc9f97b59b/crates/myownmesh/src/control/dispatch/realtime.rs#L119)
consumes F before awaiting native retirement; an unknown/consumed close is not
idempotent. Deliberate peer replacement can produce typed refusal or
`CleanupUncertain`, which receipts retain separately from acknowledged closes.
No close is retried or re-resolved by label. The confirmed quiet Events EOF gap
and feasible ambiguous-open reproduction have their separate C1 harness and
handoff; this pair does not duplicate that probe or claim event EOF retired
unknown native flows. Successful owned daemon shutdown is the final test-state
cleanup boundary.

Complete stdout/stderr and byte/hash receipts live outside the private roots.
Unix requires SIGTERM shutdown and zero exit for every owned daemon lifetime.
Windows starts hidden owned children and uses forced owned termination, recorded
as `normal_shutdown:false`; it does not claim graceful daemon shutdown. Only
nonce-marked roots under the exact protected parent are removed after every
owned child is reaped and custody is rechecked. A failed cleanup preserves its
root and fails the gate. Do not upload private identity/state/config trees;
retain stream files and the sanitized result/build/policy receipts.

`--mode direct` excludes relay services and requires actual `host`/`host`
selected pairs on **both** endpoints, before and after owned restart. A
successful host-only pair cannot establish forced-relay behavior. For the
separate relay qualification build a distinct artifact/target and build receipt:

```sh
cargo build --locked --release --no-default-features --features transport-lab -p myownmesh --bin myownmesh
```

The relay artifact's receipt must have `features:["transport-lab"]`, with the
same exact source SHA, native toolchain evidence, clean source/lock and its own
binary SHA256. The stock receipt must retain `features:[]`; the controller
refuses mixing these identities. Source
[embedded startup](https://github.com/nathanfraske/MyOwnMeshSecurityReview/blob/db7818e09fedd98899490347b86ac9bc9f97b59b/crates/myownmesh/src/embedded.rs#L889)
honors `MYOWNMESH_TRANSPORT_LAB_ICE_POLICY=relay` only in that explicit feature.
No installed daemon is rebuilt or replaced.

The owner separately selected
[mesh-control-paired-relay-policy.json](mesh-control-paired-relay-policy.json),
SHA256 `0b9aa0ebfdfa58ab8bc1798c3b90caad70cc8f3094635563796ba9330f8220a9`.
It keeps the original direct policy and all resource/semantic/scheduler/client
ceilings unchanged. Only A additionally hosts its private loopback UDP TURN:
control18476, finite relay allocation window18480..18487, public/bind IP127.0.0.1,
realm `ams-mesh03-test`, 1 MiB/s per allocation direction. TCP/TLS listeners are
disabled; finite required TCP fields are 8 connections/8 per IP, 10 s auth/30 s
idle, and inactive TLS backend port18477. Fresh credentials are generated only
inside the owned root and injected into both peer configs; source guest/default
credentials are never used. The sanitized receipt contains the policy and its
hash, not the generated password or private config/identity files. The upstream
[TURN config](https://github.com/nathanfraske/MyOwnMeshSecurityReview/blob/db7818e09fedd98899490347b86ac9bc9f97b59b/crates/myownmesh-core/src/config.rs#L2376)
defines these exact fields. No firewall, global service, account or ACL setting
changes are part of the recipe.

Run the same two probe commands sequentially with the relay artifact/hash/build
receipt and new evidence destinations, replacing `--mode direct` with:

```sh
--mode relay-lab --relay-policy scripts/ci/mesh-control-paired-relay-policy.json
```

The startup selector requires the owned TURN service's actual listen/running
status before B starts. Both peers receive only the private TURN URL and the
explicit lab selector. During the strict real media gate, `peers_list` must
report `selected_pair:{local:"relay",remote:"relay"}` on **both** endpoints,
again after restart; configuration or available candidate counts are not enough.
These source-backed immutable fields come from
[PeerInfo](https://github.com/nathanfraske/MyOwnMeshSecurityReview/blob/db7818e09fedd98899490347b86ac9bc9f97b59b/crates/myownmesh-core/src/handle.rs#L1702)
and [SelectedCandidatePair](https://github.com/nathanfraske/MyOwnMeshSecurityReview/blob/db7818e09fedd98899490347b86ac9bc9f97b59b/crates/myownmesh-core/src/transport/diag.rs#L46).
Every occupied port, resource refusal or missing path observation remains a
failure, with no automatic port selection, larger grant or fallback to direct.
The loopback lab does not qualify real NAT/firewall/cellular traversal or WAN
performance. Device/network hardware, mobile execution, installed service
upgrades and product V1 readiness remain separate acceptance gates.

For AMST access reuse [mesh-control-amst.py](mesh-control-amst.py): same-account
argv, owned PTY with 40x120 geometry before exec, one physical PowerShell input
line, bounded streaming, complete EOF drain and sole final reap. Record actual
nested target OS/toolchain/source identities. An online catalog entry alone is
not native execution. Inspect any `outcome_unknown` target receipt before another
start; never replay it or signal an unrelated process group.

The actual paired-media gate remains **failed as of 2026-10-16** at candidate
`db7818e09fedd98899490347b86ac9bc9f97b59b`. Preserve these primary Windows runs
and their separate failure boundaries. The evidence directories below are under
the manager's retained `target/mesh03-realtime/`; each contains `receipt.json`
and complete child streams.

| Durable run | Application source | Evidence directory | Observed boundary |
| --- | --- | --- | --- |
| `fd99fee5-3c94-495f-8d44-ad87f717c218` | `479025b529d122a6af6b86f4743458bda2058b96` | `pair-transport-direct-479025b` | Original policy rejected during A's semantic startup, before B, FlowOpen or media |
| `cb8e4886-ecb9-4f15-a8e1-de558a16336f` | `949aa96ca54273132c8466edefb6392304c8b44d` | `pair-transport-direct-e6729ca6` | Startup passed; strict Opus comparison received exact packet 1 while expecting packet 0; phase not established by this log |
| `5ebcdcde-f129-4c19-971f-18f81bdc98c0` | `8a59dce9d277f205410d0c4066a5bd00bfd8bf87` | `pair-transport-direct-e6729ca6-backtrace` | Same comparison failure; `RUST_BACKTRACE=1` produced a header without caller frames, so phase remained unproved |
| `904b694b-cd73-49d5-9a7d-4b87270973ee` | `823c99fde9c160b11b932e6f7664081796d14c8c` | `pair-transport-direct-823c99f-phase` | Safe diagnostics establish the first receive in `initial_forward` as packet 1 instead of expected packet 0 |

The first run used original policy LF SHA256
`0e8b132f6373c771eb3bdcfea9e2d95d541fcc9c533c1aa8674858288d3755c7`.
The owner then selected only three lower values: retained WAL frames 64 to 48,
checkpoint threshold 263712 to 197792 bytes, and admitted bytes 4194304 to
1769472. The corrected policy is the `e6729ca6...` LF identity above; all other
ceilings remain unchanged. Windows receipts retain raw CRLF file hashes
(`85e2aaea...` originally, `45ad0d80...` corrected); their parsed JSON matches
the respective LF policy. The invalid-policy run remains a startup failure,
not media-delivery evidence.

In `904b694b`, source audio 0 (371 bytes, SHA256
`f05fbbb5c94e59cdcf0114e1f521db4df0760a279d4d19b88813f46478c6149e`)
and the next paced audio 1 (297 bytes, SHA256
`8a44f9f6c7774f8f87f2ee4ebde2304f57dc8f31e8cc0a766c791394df92246c`)
both completed local writes. The first receive had the expected audio label,
`expected_index:0` and `prior_units_verified:0`, but its entire payload matched
audio 1. No phase completed. This proves a strict initial-order failure; it
does not establish whether packet 0 was lost or delayed, nor prove video
delivery or received software decode. `paired.stdout` is 2002 bytes, SHA256
`a5d4e18831fee3471bdccea704f0b1e9baa1bb4f57d05584463cc0a17688a78f`.
Both peers reported ACTIVE, but that is not a flow readiness result. The run
selected `--mode direct`; actual host/host pair assertions occur after the
initial batch and were not reached.

[Hosted run 38947043848](https://github.com/nathanfraske/AllMyStuffEncoderFraske/actions/runs/38947043848)
used application `8a59dce9d277f205410d0c4066a5bd00bfd8bf87` and the same exact
candidate and corrected policy. Its native Windows AMD64 and Intel macOS
Darwin 24.6.0/x86_64 transport pairs each failed the strict Opus comparison.
Each received byte array matches that host's generated `audio-1.opus`, while
the expected array matches its `audio-0.opus`; these are not universal codec
goldens. Their earlier logs do not independently establish the exact phase.
Artifacts are retained in `ci-38947043848/mesh03-native-{windows-latest,macos-15-intel}-38947043848-1/`,
with the comparison streams in `transport-pair/` and actual source files in
`vectors/`. Both native vector generators and reference software decode passed
for three H264 AUs with AMS identity SEI and six Opus packets; this does not
qualify received-stream decode. The original Ubuntu job stopped at the earlier
portable fixture (21/22 passed), before an actual pair. Its corrected fixture
has not yet been executed on native Linux at this checkpoint.

The four primary Windows pair receipts and both hosted pair receipts record
all owned children reaped, private roots removed and no cleanup errors. Hosted
Mac records `normal_shutdown:true`; Windows records forced owned termination
and `normal_shutdown:false`. Cleanup success does not change the media failure.

The source handoff is **MESH03-UP-READY-04**, independently derived in A2's
retained `target/mesh03-fixtures/readiness-contract-analysis.json` (SHA256
`56faba8870271bdfc1bb7d4f21e39267a043e4b81de63191065474fcfefb5667`).
At the exact candidate, FlowOpen/pipe acknowledgements, ACTIVE, current flow
identity and the Status profile advert do not establish native readiness for
that newly opened flow. The fresh-sender
[add_track path](https://github.com/nathanfraske/MyOwnMeshSecurityReview/blob/db7818e09fedd98899490347b86ac9bc9f97b59b/vendor/webrtc-0.13.0/src/peer_connection/mod.rs#L1713)
creates a transceiver and schedules negotiation without awaiting later track
binding; a reused sender can perform binding work, so this is not a universal
binding barrier. The vendored
[sample writer](https://github.com/nathanfraske/MyOwnMeshSecurityReview/blob/db7818e09fedd98899490347b86ac9bc9f97b59b/vendor/webrtc-0.13.0/src/track/track_local/track_local_static_sample.rs#L87)
returns success without emitting RTP while its packetizer/sequencer is absent.
Conversely, the receiver's initial
[peek](https://github.com/nathanfraske/MyOwnMeshSecurityReview/blob/db7818e09fedd98899490347b86ac9bc9f97b59b/vendor/webrtc-0.13.0/src/track/track_remote/mod.rs#L280)
requeues its packet; an asynchronous receive hook alone does not prove it
discarded the first packet. These sources support a possible startup window,
not a proved cause of the observed mismatch.

The concrete upstream need is an exact-owner, flow-specific readiness contract
that establishes the applicable native sender binding before claiming readiness
to send, with bounded failure/refusal and lifecycle semantics. No public such
barrier was identified in the reviewed daemon/core contract. Existing session,
profile or current-identity checks cannot supply it. This is an upstream handoff,
not an upstream patch or issue submission; no hidden warm-up, sleep, packet
discard, retry or relaxed comparator has been added.

The initial transport failure leaves full AV delivery, received video/SEI/clock
and software-decode checks, later independent-lifetime/refusal/pressure/cancel/
restart scenarios, the actual node consumer pair and measured host/host path
gates unmet. The separately built transport-lab artifact has no actual relay
pair pass. Native Linux paired delivery, hardware capture/render/playback,
installed service behavior and product V1 activation remain unqualified.

The final native checkpoint is
[run 38952708616](https://github.com/nathanfraske/AllMyStuffEncoderFraske/actions/runs/38952708616)
at exact application `733c7889bfd5150a220b2047372a7d684d17d273`, candidate
`db7818e09fedd98899490347b86ac9bc9f97b59b` and the same corrected TEST policy.
It advances the earlier pending Linux checkpoint; the original four primary
runs and run 38947043848 remain separate failed evidence above.

All three native jobs passed the same 22 portable contract tests and four node
route tests. This includes the corrected cancelled-partial-receive fixture on
native Linux, resolving that earlier fixture failure. Minimal builds with the
optional realtime feature off/on, dependency metadata checks, both probe builds,
the stock candidate daemon build and native codec-vector/reference decode gates
passed. Each job retained identical before/after hashes for all five application
and minimal-consumer lockfiles. These passes qualify those bounded gates;
the overall workflow still failed at actual media delivery.

| Native target | Contract / route tests | Actual pair | `normal_shutdown` |
| --- | --- | --- | --- |
| Windows Server 2025 / AMD64 | 22 / 4 passed | First `initial_forward` receive equals own audio 1; expected own audio 0 | `false` (forced owned cleanup) |
| Intel macOS, Darwin 24.6.0 / x86_64 | 22 / 4 passed | First `initial_forward` receive equals own audio 1; expected own audio 0 | `true` |
| Native Ubuntu 24.04, Linux 6.17.0-1022-azure / x86_64 | 22 / 4 passed | First `initial_forward` receive equals own audio 1; expected own audio 0 | `true` |

Every pair now has phase diagnostics: audio 0 and the next paced audio 1
completed local writes; the first receive has `expected_index:0`,
`prior_units_verified:0` and `label_matches_expected:true`. Its complete payload
matches that runner's `audio-1.opus`, while the expected bytes match its
`audio-0.opus`. No phase completed. This extends the observed first-receive
failure to native Linux as well as Windows and Intel Mac; it does not prove
loss versus delay or the native binding cause. All owned pair children were
reaped, private roots removed and cleanup error lists empty.

Retained artifacts are under the manager's
`target/mesh03-realtime/ci-38952708616/mesh03-native-{windows-latest,macos-15-intel,ubuntu-24.04}-38952708616-1/`.
Each has complete command receipts/streams, source vector files and
`transport-pair/receipt.json`. The three `transport-pair/paired.stdout` identities
are Windows 2003 bytes/SHA256
`f9f914ae12930e1731e2647e5994fd23a8179bcf501c83afff55bf0e33015ec3`,
Intel Mac 2003 bytes/SHA256
`8e64d2f01fa655a2c28d47f81ef52ff870f7b71475999fadf2169bdfd3cdd602`,
and native Linux 2002 bytes/SHA256
`a62cb8163d8ccf0c477f0fc361e66910ecf3c4c6cf4c645012b0538b0ae093d7`.

The strict media predicate, vectors and pacing were preserved. Received video,
AMS SEI/clock checks, received software decode, a complete AV phase, later
lifetime/refusal/pressure/cancel/restart scenarios, measured selected paths,
the actual node consumer pair and forced-relay delivery remain unmet. The
source-supported readiness risk and unproved causality qualifications above
still apply. This documentation checkpoint leaves the tested code unchanged
and does not request another identical CI run or activate candidate product use.
