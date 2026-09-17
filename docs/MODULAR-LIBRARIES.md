# Selective AllMyStuff libraries

The current direction is to select reusable libraries individually, with an
optional AllMyStuff runtime hosting capabilities and the GUI consuming them.
There are no new consumer presets or profiles in this change. The optional
host and application registration model remain future work; these first two
extractions preserve existing node behavior.

| Library | Current contents | Direct dependencies |
| --- | --- | --- |
| [allmystuff-byte-queues](../crates/allmystuff-byte-queues/src/lib.rs) | Viewer byte queues, watcher tokens and local IPC chunk packing. | `parking_lot` 0.12, `tracing` 0.1. |
| [allmystuff-frame-timing](../crates/allmystuff-frame-timing/src/lib.rs) | `FrameCadence`, `AssemblyClock`, `SendBreakdown`, `send_breakdown` and `periodic_sample`. | Standard library only. |

Neither package depends on the node, GUI, codecs, capture backends or a Mesh
transport. Both inherit workspace version `0.2.121`, edition 2021 and declared
minimum Rust `1.88.0`; a declared minimum is not a new toolchain qualification.
The node modules are compatibility shims: the public
`allmystuff_node::byte_queues::ByteQueues` path remains available, and existing
internal frame-timing call sites use the extracted implementation.

**Selective use.** An application can depend on either package directly. For
a consumer beside this repository, choose the dependency lines it needs:

```toml
[dependencies]
allmystuff-byte-queues = { path = "../AllMyStuff/crates/allmystuff-byte-queues" }
allmystuff-frame-timing = { path = "../AllMyStuff/crates/allmystuff-frame-timing" }
```

Adjust these paths to the checkout location. Keep the repository workspace
metadata available when using these path dependencies; this example does not
claim that either package has been published to a registry. Public Rust usage
examples are in each crate's rustdoc and are compiled as doc tests.

Reuse the queue for a local viewer that can tolerate whole-chunk eviction;
reuse timing for local capture scheduling or assembly/send measurements.
Neither choice requires a running AllMyStuff host. This extraction supplies no
application SDK, provider registry or identity-approval system.

**Byte-queue policy.** These are the existing policies, retained unchanged:

- `ensure(key)` creates an eager queue with reserved token `0`, keeping bytes
  received before a viewer subscribes. It leaves an existing queue intact.
- `watch(key)` returns a new token and takes ownership of the same buffered
  queue. `unwatch(key, token)` removes it only if the token still matches, so
  stale window cleanup cannot remove a newer watcher's queue. `remove(key)`
  removes the queue regardless of token. Tokens scope cleanup, not permission.
- The configured limit bounds retained payload bytes **per key**. Overflow
  evicts whole oldest chunks until they fit; a chunk larger than the limit can
  evict itself and everything before it. This is not reliable delivery.
- `enqueue` drops bytes and returns `false` if the key has no queue. Otherwise
  it returns whether the queue was empty before appending, as a notification
  hint. Oversized chunks, including nonempty input at a zero-byte limit, can
  return `true` while leaving the queue empty. Callers poll after notification;
  the result is not a delivery acknowledgement.
- `poll` drains chunks in order, each packed as a four-byte unsigned
  little-endian length followed by its bytes. An empty or absent queue yields
  an empty buffer. Callers must keep each chunk length representable as `u32`;
  the current packing code casts the length without checking it.
- The limit excludes allocation/framing overhead and does not cap key count
  or empty chunks. It is not a total memory budget or Mesh admission control.

The three original unit tests moved with the implementation. A public API
integration test preserves the oversized-chunk/notification boundary; the
rustdoc example exercises use from outside the node crate.

**Frame-timing policy.** Callers supply local `Instant` values and perform the
actual wait. `FrameCadence` clamps the requested rate to 1 through 240 fps and
advances past missed slots without replaying them. `AssemblyClock` includes
the closing marker in total duration and longest-gap measurements.
`send_breakdown` uses saturating microsecond accounting. `periodic_sample`
selects matching sequence numbers divisible by 60; it does not synchronize
clocks. Never subtract timestamps from different hosts. The five original
tests move with the implementation, alongside a public API rustdoc example.

**Focused commands.** From the repository root, after both packages and their
Cargo wiring are assembled:

```sh
cargo test --locked -p allmystuff-byte-queues -p allmystuff-frame-timing
cargo clippy --locked -p allmystuff-byte-queues -p allmystuff-frame-timing --all-targets -- -D warnings
cargo fmt --check -p allmystuff-byte-queues -p allmystuff-frame-timing
```

The test command covers unit, public API integration and doc tests without
building the node. Node-shim compilation is separate integration evidence.
The [contribution guide](../CONTRIBUTING.md) describes wider checks. During this
managed extraction, important checks run through the manager's durable run
records; the commands here are recipes, not claims of successful execution on
every platform. The [stage-one ledger](reviews/modular-foundation/stage-one-verification.md)
records the earlier changes and does not qualify these new packages.

**Revisit with the MyOwnMesh v1 contract.** These flags remain open:

| Boundary | Work to reconcile later |
| --- | --- |
| Legacy daemon adapter and capability publication | The app still consumes the old pin and built-in capability/protocol stack. Review replacement transport and richer registration contracts before changing that adapter. These packages do not implement a custom-app registry. |
| App registration and identity grants | The proposed MyOwnMesh-owned app approval, identity custody and grant issuance/revocation work is in progress. AllMyStuff consumes scoped authority and applies capability policy; it must not issue or broaden Mesh authority. No upstream method names or finalized API are assumed here. |
| Local queue packing and future routes | The queue's `u32` records are local IPC packaging. They do not specify a future Mesh channel/route format. Keep application payload/framing decisions distinct from transport packetization and route ownership. |
| Resource policy and admission | Revisit injectable queue limits, notification semantics, aggregate memory/accounting and backpressure with application requirements and Mesh resource admission. Preserve the current drop policy until a separately reviewed change supplies the required boundary tests. |
| Timing and observability | Timing remains local application measurement. Future transport metrics may correlate frame identities, but must not turn these durations into cross-host clock arithmetic. |

The [original roadmap](MODULAR-FOUNDATION.md) and its reviews remain historical
evidence. Its proposed consumer-profile framing is superseded by selective
libraries, an optional host and a GUI consumer; the broader implementation and
platform qualifications have not been supplied by these two extractions.
