# Storage library: independent compatibility review

Source baseline: `ac548bddd23f413de60c4edd4b47c8e0b60342e0`.
Assignment: `manager:1ffb1a79-2f3e-40a5-bb6f-bdf34d714292`.

The operator authorized one `allmystuff-storage` package containing the fleet
storage-plan model, validation and deterministic transitions, with its existing
host persistence boundary and caller behavior preserved. File transfer, mounts,
sites, live daemon/state access and RISC-V work are outside this slice.

C2 owns independent core fixtures and this evidence report. C1 owns the library,
node adapter and Cargo wiring. A2 owns explicit-path host persistence fixtures;
A1 reviews those fixtures, callers, authority checks and wiring. Jackson owns
all durable compiler, lint and test execution. No worker native execution is
part of this evidence.

## Frozen baseline and literal expectations

The [baseline manifest](../../../crates/allmystuff-storage/tests/baseline/manifest.json)
records eleven input identities and five verbatim snapshots, frozen before
reading C1's extracted implementation. The original 604-line `storage_plan.rs`
is Git blob `367b85c012c322b4fc571af2ddf951625ec07f67`, SHA-256
`28a4b8e7939aa4980c7d3d8ec7257aae273bf2865f155a2b8f2b588e39a1f965`.
The entire store, persistence helper and authenticated/local command/sync spans
remain available for source comparison. Existing seven store tests are retained
as source evidence, not a claim that they have run for this extraction.

[Wire vectors](../../../crates/allmystuff-storage/tests/baseline/wire_vectors.json)
freeze field names/order, aliases and defaults.
[Digest vectors](../../../crates/allmystuff-storage/tests/baseline/digest_vectors.json)
freeze six compact persisted strings and independently calculated FNV-1a values.
[Contract vectors](../../../crates/allmystuff-storage/tests/baseline/contract_vectors.json)
freeze limits, exact operation errors, UTF-8 byte identity examples, stamp order
and sequencing observations. These are source-derived expectations; neither old
nor extracted Rust code has been executed to generate them.

## Behavior to preserve

| Boundary | Baseline behavior |
| --- | --- |
| Serde | Policy defaults are replicas 2, reserve 10%, retention 30 days, rebalance 50 GiB/day and metered pause true. `ordinaryReplicas` aliases `replicas`; unknown legacy `criticalReplicas` is ignored. Snapshot fields use camelCase; Patch `device_intents` and persisted map keys retain snake_case. Missing Patch policy means `None`; missing intent vector defaults empty; required allocation vector does not. |
| Identity and validation | IDs are decimal UTF-8 device-byte length, colon, device then volume. Device/volume are nonempty, at most 512 bytes, and reject NUL. Actor checks enforce nonempty/512 bytes but accept NUL. Do not silently harden one into the other. Quota zero is invalid even for a disabled allocation. |
| Policy bounds | Replicas 1..=8; reserve 5..=50; retention 0..=3650; rebalance 0..=10000. Validation precedes clock consumption and preserves exact error text. |
| Stamps and clocks | Stamps order by counter then actor. A local stamp uses the maximum live-record counter and that actor's stored counter plus checked one; unrelated counter-map entries do not participate. Overflow errors without wrapping. Equal-value local writes still advance a clock. |
| Local operation order | Identity, policy and cap checks occur at their original stages. Allocation quota validation occurs after advancing the actor clock, so a rejected zero-quota allocation consumes a counter. Cap errors take precedence over actor validation for new records. |
| Merge authority | `sender_may_manage` comes from an authenticated caller, never a wire grant. Managers can relay valid other actors. Ordinary members may change only records whose device and stamp actor both equal the sender. An empty sender or either input vector above 512 rejects the whole patch. |
| Ordered merge | Process input vectors in order. Equal stamps retain existing values; duplicate equal-stamp records retain the first accepted value. At capacity, existing IDs may update but new IDs are ignored. Counters advance only for accepted newer allocations/intents; a policy merge has no direct counter-map write. |
| Persistence result | Setter failure restores the affected record while retaining the consumed counter. Changed merge failure restores the full previous state, including counters, and returns false. No-op merge skips persistence. These differences are part of the current contract. |
| Loading | Invalid policy becomes default; invalid/mismatched records are removed before lexicographic map truncation to 512. Counters are neither validated nor capped. Sanitization does not rewrite the file. Read failure defaults; parse/schema failure uses the original quarantine helper. |
| Digest | Hash compact serialization of the full persisted state, including ordered maps and counters, using wrapping 64-bit FNV-1a and sixteen lowercase hex digits. Snapshot equality does not imply digest equality. |
| Host authority | Fleet-network and authenticated-sender checks precede merge. Local manager/volume/capacity checks, resource materialization, notification and transport chunking stay in node; `PLAN_CHUNK` remains 16. No model operation creates authority or storage roots. |

## Current verification state

Baseline inputs and literal expectations are frozen. Core fixture construction
and source review are in progress. C1's proposed prepared operations will be
reviewed against the original pre-lock validation and locked mutation bodies;
API names alone are not acceptance. A2 covers the real persistence boundary
through explicit disposable paths, not ordinary state discovery.

Central compilation, actual dependency closure and execution remain pending.
No test count, passing gate, platform qualification or all-tests claim is made
at this stage. Final evidence will distinguish new core comparisons, retained
tests, host failure-path tests and broader application limits.
