# Frozen storage-plan inputs

Source revision: `ac548bddd23f413de60c4edd4b47c8e0b60342e0`.
These inputs were frozen before reading the extracted library implementation.
`manifest.json` identifies eleven complete source/manifests/lock files, five
verbatim snapshots and three sets of independent literal expectations.

The `*.rs.txt` files are raw Git UTF-8/LF bytes. They are evidence, not automatic
Rust test targets. Preserve them through ordinary formatting. The full original
store includes its seven existing tests; their inclusion is not an execution
claim. The original persistence helper and authenticated/local caller spans
freeze boundaries that remain host-owned.

`wire_vectors.json` records struct/enum field order, aliases, defaults, required
fields and distinct wire/snapshot/persisted naming. `digest_vectors.json` records
literal compact persisted JSON and its wrapping FNV-1a result, independently
calculated from those strings without executing either Rust implementation.
`contract_vectors.json` records validation boundaries, exact error strings,
resource IDs, ordered stamps and operation-specific sequencing observations.
Its UTF-8 identity cases count bytes, not characters.

The compatibility fixtures will combine these fixed expectations with original
method bodies. Any test-only import/persistence adapters must be separately
identified; original decision bodies must remain inspectable and unchanged.
Actual host read/write/quarantine/failure tests are A2's separate
`node/src/storage_plan_compatibility_tests.rs` lane, using private explicit paths.
No fixture may discover ordinary user state or contact a daemon.

No worker compiler, test, service, native device or storage execution has been
performed. Source preparation and checksums are not runtime qualification.
