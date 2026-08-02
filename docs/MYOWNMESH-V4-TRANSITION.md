# AllMyStuff transition to MyOwnMesh V4

Status: docs-only implementation map for later execution.

Source mapped: `nathanfraske/AllMyStuffEncoderFraske@8fc90d85f5b7698e1c9c0a276b8ad9453d44a994`.

Target basis: the owner-adopted MyOwnMesh V4 hybrid architecture in `nathanfraske/MyOwnMeshSecurityReview`.

This document does not authorize implementation against an unstable or incomplete MyOwnMesh API. It identifies what must move, what application behavior survives, and the smallest safe order for moving it once the V4 Endpoint Auth, Session Broker, application contract, and real-time-flow surfaces are ready.

## 1. Target boundary

AllMyStuff should consume MyOwnMesh through this relationship:

```text
MyOwnMesh roster and reachability view
    -> request_session(exact MeshContext, exact DeviceId)
    -> AuthenticatedPeerSession / SessionCapability
    -> AllMyStuff capability and route negotiation
    -> session-bound message, RPC, stream, datagram, or real-time-flow handles
```

The application may retain its own concepts of a screen route, terminal route, file transfer, room, camera feed, clipboard session, or CEC support case. None of those objects may create or stand in for:

```text
endpoint identity
mesh participation
Closed authorization
a transport path
a live MyOwnMesh session
```

A MyOwnMesh connector finds and maintains working channels. AllMyStuff chooses application behavior over the resulting authenticated session.

## 2. What survives and what changes

### Survives as application logic

- `allmystuff-graph` capability compatibility and sharing rules.
- Application route intent for screen, input, terminal, files, clipboard, sites, camera, and rooms.
- Codec, capture, encoder, decoder, quality mode, monitor priority, and recovery policy.
- The pure `allmystuff-session` state-machine pattern.
- CEC Support approval scopes and application capabilities.
- The one-node-per-machine desktop/headless arrangement.
- The local process boundary between AllMyStuff and the MyOwnMesh daemon.

### Must move behind V4 authority

- Every directed send, reliable send, RPC call, subscription, media operation, and callback.
- Every use of a peer string, `ClientId`, route id, lane id, raw daemon event, or socket as effective authority.
- Fleet and Closed-network membership decisions currently derived or mirrored in application state.
- Presence and online state that currently conflate app profile, mesh participation, and transport liveness.
- Media Lane lifecycle and fixed audio/video transport semantics.
- Ordinary-member routing or topology-driven application forwarding.

## 3. Current-to-target map

| Current surface | Current role | V4 destination | Required treatment |
|---|---|---|---|
| `crates/allmystuff-protocol/src/control.rs` | Hand-kept mirror of the complete legacy daemon control protocol | Versioned V4 application-gateway wire contract | Split the public client contract from legacy daemon internals. New code must use roster, reachability, session request, session operations, and diagnostics. Move the current raw request enum under an explicitly named `legacy_v1` compatibility module. |
| `node/src/control_client.rs` | Generic request/response socket client, raw event subscription, binary media pipes | `V4MeshClient` or equivalent narrow gateway client | Keep the local IPC transport. Replace generic request construction with typed methods whose arguments and results reflect the V4 application contract. Do not expose raw candidates, routes, relays, signaling, or internal capability tokens. |
| `node/src/mesh.rs` | Large application and mesh integration owner | Application coordinator over narrow MyOwnMesh and application-plane ports | Split transport/session ownership out. Keep route, capture, terminal, file, clipboard, site, room, and CEC product behavior in their application owners. Replace raw peer and daemon state with exact session handles and reachability views. |
| `crates/allmystuff-session` presence map | App profile presence also acts as the live peer picture | MyOwnMesh roster/reachability plus app profile cache | MyOwnMesh supplies durable participation, authorization, and live observations. `NodeProfile` remains application metadata exchanged over an authenticated session or an explicitly defined app discovery service. Cached profile data is never authority. |
| `crates/allmystuff-session::Route` | App route plus route offer/accept/teardown state | `ApplicationRoute` over one exact authenticated session | Retain the pure state machine, but make its peer binding an authenticated session reference. Route ids and incarnation tokens are application workflow identifiers only. They never identify a carrier, path, or MyOwnMesh session. |
| `route-incarnation-v1` boot and sequence values | Fences delayed app route messages | Opaque application operation lifetime | Preserve replay and stale-message protection at the application layer. Remove any claim that a monotonic application sequence establishes current mesh or transport authority. The final token construction should be selected with the application protocol migration. |
| `ChannelSendTo`, `ChannelSendReliable`, `RpcCall` | Peer-string addressed application operations | Session-bound message/RPC handles | Require a current `SessionCapability` internally. Serialization, queue insertion, and delivery occur only after the live handle guard. |
| `ChannelSendAll` | Broadcast over the current mesh peer set | Explicit application fanout | Enumerate eligible participants from the app's policy and open or reuse independent sessions. Failure to reach B does not route B's operation through C. |
| `MediaLaneOpen`, `MediaLaneClose`, `VideoSend`, `AudioSend`, media source/track pipes | Fixed WebRTC video/audio lane API | Session-bound generalized real-time flows | AllMyStuff owns codec and media meaning. MyOwnMesh exposes connector-native flow capabilities. Use independent flow keys and per-flow pressure policy. Retain the existing optimized RTP and local IPC mechanisms behind the adapter while callers migrate. |
| `VideoSample`, `AudioSample`, `MEDIA_KIND_*`, lane numbers | Codec/media-specific transport contract | AllMyStuff media profile and WebRTC compatibility adapter | Remove from the basal MyOwnMesh-facing API. They may remain private inside the legacy WebRTC adapter until every caller uses generalized flows. |
| `desired_routes`, daemon epochs, watcher recovery, route replay | Reconstructs application intent after daemon/session loss | Application intent plus V4 session recovery | Keep user intent. Stop reconstructing transport authority from route state. When a session suspends, retain app intent and request/rebind a valid session through MyOwnMesh. |
| `PeersList`, presence events, clock-skew voting, local online flags | Mixed peer discovery and reachability evidence | Roster view plus reachability vector | Represent participation, signaling responsiveness, candidate state, authenticated channel, and active session separately. Missing observations never synthesize removal. |
| `RosterApprove`, `RosterRemove`, governance requests | Application drives legacy mesh authority directly | Locally authenticated V4 administration API | AllMyStuff requests a semantic change. MyOwnMesh constructs, validates, and commits the exact Open or Closed facts. Raw governance and roster mutation formats do not remain ordinary app APIs. |
| `OwnedRoster` fleet gossip | Fleet membership/display metadata and partial authority mirror | V4 Closed roster plus app fleet metadata | V4 Closed semantics become the sole membership authority. Retain fleet name and other app metadata separately. App gossip may not admit, restore, or revoke a fleet member. |
| LAN claim rendezvous and claim code | Application onboarding workflow | App onboarding over bounded V4 contexts and sessions | Preserve LAN-first behavior. A claim code is a rendezvous secret, not endpoint or Closed authority. Successful adoption must end in the reviewed Closed governance transition. |
| topology settings and shaped routing | Connection shape plus ordinary-member forwarding | Local connector preference only | Topology may guide local connection policy. It cannot grant authority or forward another endpoint pair's app payload. |
| desktop/headless sidecar model | One AllMyStuff node and one mesh daemon per machine | Retain | Start the daemon with explicit V4 connector policy. The GUI remains a thin client of the AllMyStuff node. |
| mobile embedded daemon | In-process MyOwnMesh daemon and node engine | Retain with the same V4 contracts | Mobile uses the same session and application APIs. Process placement does not change authority. Connector policy and local-principal binding remain explicit. |

## 4. CEC Support inside AllMyStuff

The existing CEC Support crates are application-level product components. They should remain application-level after the V4 migration.

### 4.1 Help discovery

The current well-known help area may continue to distribute bounded support beacons, but it must not become a source of endpoint identity or authorization.

```text
support beacon or phone lookup
    -> exact DeviceId hint
    -> request_session(...)
    -> endpoint authentication
    -> customer approval policy
    -> CEC application capability
```

A `SupportId` remains a display and lookup aid. It is not a session handle or cryptographic identity.

Infrastructure hubs may provide signaling, TURN, or an explicit relay profile. Hub placement and topology do not make a technician trusted and do not make a customer application payload readable by the hub.

### 4.2 Consent

`allmystuff-cec-consent` should migrate from a canonicalized string as the effective runtime authority to:

```text
exact authenticated remote DeviceId
+ authenticated local application principal
+ current CEC grant
+ current live session or session-bound app capability
```

The existing `Once`, `ThreeHours`, and `Forever` choices remain valid application policy.

- `Once` must be tied to the exact support session lifetime, not merely the process lifetime.
- Persistent grants remain keyed by the exact technician Device identity and capability set.
- Revocation invalidates subsequent protected operations immediately.
- Per-operation checks remain useful defense in depth, but the operation must already be entering through the valid session-bound CEC capability.

### 4.3 Separate CEC profile and storage

Keep the current CEC-specific signing-domain and storage separation during migration. Review whether V4 MeshContext and protocol-profile separation can replace any of it only after the final V4 profile and mixed-version design are fixed. Do not collapse identities or stores during the transition.

## 5. Legacy API policy

The goal is full migration, not permanent dual architecture.

### 5.1 Immediate classification

Every public mesh-facing item must be classified as one of:

```text
V4 stable
LegacyV1 compatibility
internal migration adapter
scheduled for removal
```

### 5.2 Compatibility module

Move incompatible raw operations under an explicit feature and namespace:

```text
feature: legacy-v1
module: allmystuff_protocol::legacy_v1
```

The current product may temporarily enable that feature while it is being migrated. New applications and new modules must not enable it.

### 5.3 Compiler guidance

Use `#[deprecated]`, visibility restriction, and feature gating. Do not use Rust `unsafe` as an architectural warning.

New V4 modules and new first-party consumers should use:

```rust
#![deny(deprecated)]
```

Serious bypasses, such as raw `NetworkState`, raw connector control, raw candidate application, ordinary-member routing, and raw authority constructors, should become crate-private rather than merely warn.

### 5.4 Deletion

A compatibility adapter gains no new product behavior and names its deletion slice. The migration is complete only when:

- no first-party caller uses the raw legacy control mirror;
- no app operation is authorized by a peer string, `ClientId`, route id, lane id, or socket;
- every app operation crosses a live session capability;
- legacy roster/governance authority is gone;
- fixed Media Lane APIs are gone from the public surface;
- legacy ordinary-member forwarding is gone.

## 6. Minimal execution playbook

### AMS-V4-0: Freeze and mark the legacy surface

1. Inventory every use of the raw control mirror, channel/RPC send, media operation, roster/governance request, route lifetime, and daemon event.
2. Add the `legacy-v1` namespace and deprecation annotations without changing behavior.
3. Prohibit new imports with a CI source/dependency check.
4. Record positive-control behavior for desktop, headless, mobile, direct, TURN, reconnect, screen, audio, terminal, files, clipboard, sites, rooms, fleet, claim, and CEC support.

Gate: no new feature code is added to a legacy adapter.

### AMS-V4-1: Add the V4 client facade

1. Mirror only the owner-adopted V4 application contract in a new serde-only module.
2. Add typed roster, reachability, session request/watch/close, diagnostics, and session-operation methods to `ControlClient`.
3. Keep the process boundary and exact daemon-restart behavior.
4. Implement explicit protocol/profile negotiation. Never silently treat a legacy peer as V4.

Gate: a new test application can observe a peer and obtain a V4 session handle without importing a legacy request type.

### AMS-V4-2: Migrate non-real-time application operations

Migrate in bounded groups:

1. directed typed messages;
2. explicit fanout;
3. acknowledged delivery;
4. unary and streaming RPC;
5. terminal, files, clipboard, and sites.

For every group, move outbound serialization and inbound dispatch behind the session guard, then remove the old bypass.

Gate: stale, closed, foreign-context, or foreign-principal handles cannot send or deliver.

### AMS-V4-3: Migrate real-time flows

1. Add a session-bound AllMyStuff flow registry.
2. Map each screen, camera, microphone, system-audio, or other real-time stream to an independent codec-neutral flow key.
3. Keep codec and media policy in AllMyStuff.
4. Keep RTP, packetization, congestion, and native transport behavior in the connector provider.
5. Preserve low-copy binary IPC and hardware encode/decode paths.
6. Remove public `MediaLane*`, `VideoSend`, and `AudioSend` dependencies after all callers move.

Gate: one saturated video flow cannot starve audio or another monitor, and no encoded unit is exposed before session promotion.

### AMS-V4-4: Migrate fleet, claims, and application presence

1. Derive fleet authority only from V4 Closed semantics.
2. Separate app fleet metadata from membership authority.
3. Move claim completion through the V4 administration boundary.
4. Replace app peer presence authority with roster/reachability views plus non-authoritative `NodeProfile` metadata.

Gate: app gossip, topology, carrier state, or a claim code cannot create Closed authority.

### AMS-V4-5: Migrate CEC Support

1. Treat the help area as bounded discovery/signaling only.
2. Establish every technician/customer relationship through an exact V4 session.
3. Bind CEC approval to the authenticated technician, local principal, and session-bound app capability.
4. Keep support control and media as ordinary CEC application operations over that session.
5. Preserve immediate revoke and explicit customer consent.

Gate: a visible beacon, SupportId, hub connection, TURN selection, or expired grant cannot authorize support access.

### AMS-V4-6: Delete LegacyV1

1. Default-disable, then remove the `legacy-v1` feature.
2. Delete raw request variants and adapters no longer used by supported mixed-version migration.
3. Delete application route-around paths, duplicated authority state, and old media-lane APIs.
4. Update desktop, headless, mobile, installer, and mixed-version tests.

Gate: the repository builds and all first-party products operate with no LegacyV1 feature or deprecated import.

## 7. Required measurements and owner inputs

No numeric queue, flow, timeout, candidate, or resource value is selected by this document.

Before V4 production policy is adopted, measure representative:

- direct LAN and TURN sessions;
- one and several monitors;
- H.264, HEVC, AV1, lossless, and fallback modes actually supported by the selected backend;
- audio plus sustained video load;
- terminal, file, clipboard, and sites concurrency;
- reconnect, sleep/wake, and daemon replacement;
- desktop, headless, and mobile targets;
- several meshes in one process.

Surface queue occupancy, flow service delay, unit size, assembly retention, throughput, latency, CPU, memory, close duration, and failure behavior for owner review.

## 8. Completion definition

AllMyStuff reaches V4 parity when this is the only production authority chain:

```text
MyOwnMesh authenticates and promotes the endpoint session
    -> AllMyStuff authorizes the requested application operation
    -> the operation uses a session-bound data-plane capability
```

The application may preserve its rich route and media behavior. It no longer owns or reconstructs mesh identity, transport authority, or session promotion.