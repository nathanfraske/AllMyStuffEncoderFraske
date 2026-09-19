# allmystuff-storage

Reusable fleet storage-plan records, validation and deterministic transitions.
The `plan` module depends only on the standard library, Serde and JSON. It does
not discover state directories, own a mutex, write files, connect to a daemon
or decide whether a caller has fleet authority.

`PlanState` preserves the existing persisted JSON layout, including ordered
maps and counters. Deserialize it and call `sanitize()` when loading durable
state. Sanitization preserves the original validation and ordered truncation;
it does not clean the counter map. `snapshot()` produces the existing public
record lists, and `digest()` hashes the same compact JSON bytes as the node.
The Serde name remains `Persisted`, with an explicit `struct Persisted` visitor
expectation to preserve deserialization diagnostics.

Local mutations use `PolicyUpdate`, `AllocationUpdate` and
`DeviceIntentUpdate`. Their constructors perform the original checks that
precede the host lock. `PeerPatch` similarly checks the original sender and
input-vector bounds. Calling `apply()` under the host's synchronization runs
the remaining transition and receives a synchronous persistence callback:

```rust
use allmystuff_storage::plan::{PlanState, PolicyUpdate, StoragePolicy};

let mut state = PlanState::default();
let update = PolicyUpdate::new("owner", StoragePolicy::default())?;
let record = update.apply(&mut state, |_candidate| Ok(()))?;
assert_eq!(record.stamp.counter, 1);
# Ok::<(), String>(())
```

The callback observes the candidate state at the original persistence point.
It runs once for a successful local transition and only for a changed peer
merge. Its `Result<(), String>` determines the existing rollback behavior:

- A failed setter restores its affected record but retains the advanced actor
  counter. An allocation rejected after stamp creation also retains that clock.
- A failed merge restores the complete prior state, including counters.
- A rejected or unchanged merge does not call persistence.

The prepared inputs do not grant authority. `PeerPatch` must receive the
authenticated sender and the caller's already established management decision.
Wire records cannot supply that decision. Existing sequential duplicate handling,
counter ordering, validation errors, byte limits and 512-record bounds remain.

The node keeps `StoragePlanStore` and its existing public paths through
compatibility reexports. Its adapter still owns state-path discovery, the mutex,
JSON loading/quarantine, directory creation and atomic replacement. Persistence
occurs while the same guard is held. Mesh keeps network admission, local manager
checks, advertised-volume/capacity checks, root materialization, reconciliation
and broadcasts. File-transfer, mounts, sites and adapters are outside this slice.

Source and compatibility fixtures are under independent review. Central
compilation, lint and isolated core/persistence tests remain pending; source
preservation alone does not establish runtime or platform coverage.
