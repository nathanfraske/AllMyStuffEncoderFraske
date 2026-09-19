# Storage-plan compatibility fixtures

`plan_compatibility.rs` contains 28 source-reviewed comparisons against the
original store and independently frozen literal expectations. The baseline is
`ac548bddd23f413de60c4edd4b47c8e0b60342e0`; `baseline/manifest.json` records its
source identities, verbatim snapshots and literal-vector hashes.

`support/oracle.rs` preserves the original types, `load_at` sanitization,
snapshot, digest, setters, merge, clock and validator bodies. Its receipt is
`support/oracle-manifest.json`. Global path discovery and the real persistence
function are omitted. Append-only test adapters provide memory JSON, controlled
persistence results and full-state observations. The root fixture's
`persist::load_json` reads that memory fixture. No oracle operation opens a path;
the persistence adapter asserts that the store has no path before recording a
candidate. `#[rustfmt::skip]` applies only to the frozen module.

Every paired mutation compares returned records or exact error text, candidate
bytes sent to persistence, resulting complete persisted bytes, snapshot and
digest. Literal assertions additionally cover defaults, field names, six
digests, identity encoding, validation/cap boundaries, stamp ties and overflow,
ordered duplicates, callback counts and operation-specific rollback. Preparing
an update before another accepted mutation also checks that the clock is taken
at application time.

The source inventory is 28 tests: 6 Serde/digest/order cases, 7 local
validation/clock cases, 10 peer/duplicate/cap cases, 2 callback-failure cases,
2 sanitization cases and 1 prepared-update case. This inventory is not a runtime
result. The manager runs the package through the ordinary Cargo test target;
workers do not compile or execute these fixtures.

Actual filesystem loading, quarantine, pretty persisted bytes, replacement and
failure paths live in `node/src/storage_plan_compatibility_tests.rs`, originally
drafted by A2 and completed by C1. Those fixtures use the node's private
explicit-path seam and disposable directories. Their separate review and the
central results belong in
`docs/reviews/modular-foundation/storage-library-extraction.md`.
