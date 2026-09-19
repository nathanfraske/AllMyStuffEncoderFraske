# Storage library: independent compatibility review

Source baseline: `ac548bddd23f413de60c4edd4b47c8e0b60342e0`.
Assignment: `manager:1ffb1a79-2f3e-40a5-bb6f-bdf34d714292`.

The extracted storage package and node adapter pass all 52 selected Windows
compatibility and persistence tests, both formatting checks, strict root and
node lint, and the node build check without default features. The tests caught
a persisted-state deserialization diagnostic change; the corrected source
preserves the original diagnostic and leaves the frozen expectations intact.

The operator authorized one `allmystuff-storage` package containing the fleet
storage-plan model, validation and deterministic transitions, with its existing
host persistence boundary and caller behavior preserved. File transfer, mounts,
sites, live daemon/state access and RISC-V work are outside this slice.

C2 owns independent core fixtures and this evidence report. C1 owns the library,
node adapter and Cargo wiring. A2 drafted explicit-path host persistence fixtures;
C1 completed that draft after a provider interruption, with final independent
review by C2. A1 completed caller, authority and wiring review before its own
provider interruption. Jackson owns all durable compiler, lint and test
execution. No worker native execution is part of this evidence.

## Frozen baseline and literal expectations

The [baseline manifest](../../../crates/allmystuff-storage/tests/baseline/manifest.json)
records eleven input identities and five verbatim snapshots, frozen before
reading C1's extracted implementation. The original 604-line `storage_plan.rs`
is Git blob `367b85c012c322b4fc571af2ddf951625ec07f67`, SHA-256
`28a4b8e7939aa4980c7d3d8ec7257aae273bf2865f155a2b8f2b588e39a1f965`.
The entire store, persistence helper and authenticated/local command/sync spans
remain available for source comparison. The seven existing store tests are
retained and passed in the central runs recorded below.

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

## Independent fixtures and source review

The [core fixture](../../../crates/allmystuff-storage/tests/plan_compatibility.rs)
contains 28 test cases. Its [oracle receipt](../../../crates/allmystuff-storage/tests/support/oracle-manifest.json)
identifies two verbatim original spans, covering all deterministic bodies and
the old load sanitization. Only append-only memory loading, persistence-result
and observation adapters replace host access. Each mutation compares its result,
candidate persistence bytes, complete resulting state, snapshot and digest;
fixed literals prevent the two implementations from being the sole oracle.
The source inventory is documented in the
[fixture guide](../../../crates/allmystuff-storage/tests/README.md).

C2 read C1's complete core and wrapper, then independently compared the model,
defaults, constants, Serde fields, sanitization, digest, snapshot, all four
mutation bodies and seven clock/validation helpers. The explicit substitutions
are the state type/visibility, original `Persisted` Serde name, caller-held state
reference and synchronous persistence callback. Prepared constructors preserve
the original checks before the mutex; the node keeps that mutex held through
mutation and persistence. Host path discovery and persistence are unchanged.
Seven existing node tests retain their bodies except one equivalent observation
through `snapshot()` after persisted fields become private to the library.

The accepted 11-path source candidate includes core SHA-256
`a7b217a50c7c25f70f481d9bdabf727732c2eb20dc79f2aa0de1caad6b7c0fd2`
and node wrapper SHA-256
`ea414cdbafd1c55d1b9057ec4cb4ab0836de482f8bda452fb9ce943bd9c07123`.
A1 independently accepted the node/caller/wiring boundary. A1 and C2 each
reversed only the new local package record and node dependency edge in all four
locks and recovered the entire old lock text. Existing pins, checksums and
external dependency records remain unchanged. Production dependencies are Serde
and JSON; `parking_lot` is a test dependency for the frozen oracle. Selected
native dependency closure is recorded below.

C2 also read all 15 explicit-path host fixtures, their helpers and the original
persistence source. The accepted fixture SHA-256 is
`37eeece7c2876eefc446f5eaf15844b037bd6eccd7f09358fb3c280595c76925`.
They reserve private disposable directories and cover defaults, parse quarantine
versus non-UTF-8 read failure, sanitize-before-cap loading without rewriting,
legacy documents, exact pretty bytes, replacement, blocked parent/temp/destination
paths, retry counters, unchanged merges, and `None` versus empty paths. They do
not call global store discovery, construct Mesh or alter environment variables.
The node's small test-module inclusion was checked separately: removing its
87-byte `cfg(test)` suffix reproduces the accepted wrapper exactly. The assembled
wrapper has SHA-256
`38166673088da34dba77be1d00996cc09700762255cb49567725c78bf873f4cb`.

## Central graph evidence

The reviewed production commit
`af310aadf30a257024379d4d4fdb9f217eb485f0` was integrated as
`c394b7ac85ea1813764bc8d67db37d4fea9f544c`. The integration record
`13e4b09f-0fe5-42dc-a0b1-d1a6154703fa` succeeded with exit 0 in 0.122 seconds,
with complete 410-byte stdout and empty stderr.

Both locked, offline normal/build dependency trees succeeded on Windows at that
commit. C2 independently read their complete streams. Each tree contains only
Serde and JSON as direct dependencies, with eleven external packages including
their derive-macro dependencies. Neither tree includes node, protocol, video,
native capture/codec backends or the oracle's `parking_lot` test dependency.

| Command after `cargo` | Durable record | Exit / seconds | Complete stdout / stderr bytes |
| --- | --- | --- | --- |
| `tree -p allmystuff-storage -e normal,build --locked --offline` | `e21246e6-7601-466b-a657-69a75413f268` | 0 / 0.449 | 725 / 0 |
| `tree --manifest-path node/Cargo.toml -p allmystuff-storage -e normal,build --locked --offline` | `f364694b-6364-4731-90ef-e0bfd0be1d9b` | 0 / 2.334 | 725 / 0 |

The independent workspaces retain their existing pin differences: the root uses
`syn` 2.0.117 and `memchr` 2.8.1; node uses 2.0.118 and 2.8.2 respectively.
These dependency trees establish the selected native dependency closure, not
compiler or runtime success, nor cross-platform resolution.

## Initial execution and corrections

The baseline, core fixtures and host fixture/include were integrated together at
`9ed6628b05f256be0d691316a8f559a8273bdad4`. Integration record
`d41ba21b-869a-4540-8f7d-a17947619d98` succeeded with exit 0 in 0.199 seconds,
with complete 1,746-byte stdout and empty stderr. Independent tree comparison
matched C1's assembled tree.

The first two formatting checks failed with the same 66,125-byte stdout and
empty stderr. C2 retrieved both through their byte cursors to EOF and reproduced
all 69 requested hunks in memory: 2 core, 52 library fixture and 15 host fixture.
The requested changes preserve literals and the frozen oracle; non-whitespace
changes are optional trailing commas and equivalent expression braces. C2 then
compared all three actual files with that reconstruction before accepting C1's
formatter-only commit `c51592becaf7f30505801a535ca173b1167fc24e`.
No worker formatter was run.

| Command after `cargo` | Durable record | Exit / seconds | Complete stdout / stderr bytes |
| --- | --- | --- | --- |
| `fmt --all -- --check` | `a6f261e3-8372-42e9-8128-8abb1537230a` | 1 / 2.357 | 66125 / 0 |
| `fmt --manifest-path node/Cargo.toml --all -- --check` | `97b22fd3-6553-49c4-8cee-f58aac2f9710` | 1 / 2.080 | 66125 / 0 |
| `test -p allmystuff-storage --locked --offline -- --test-threads=1` | `cc855e66-5543-4454-a4d3-d3e23b88a0ea` | 101 / 9.954 | 3309 / 1028 |
| `test --manifest-path node/Cargo.toml --locked --offline --lib storage_plan:: -- --test-threads=1` | `60787f02-5724-40c0-941d-7de4b1191032` | 0 / 124.179 | 2353 / 11873 |

The initial package test compiled without warnings and ran 28 comparisons:
26 passed, 2 failed, none ignored or filtered. Both failures expose the same
deserialization diagnostic drift on `null`: the new type says
`expected struct PlanState`, while the frozen baseline says
`expected struct Persisted`. The package's empty unit-test target passed;
the doctest stage was not reached after the integration-test failure.

Source acceptance had incorrectly assumed that retaining Serde's `rename`
would retain this visitor diagnostic. Independent reading of pinned
`serde_derive` 1.0.228 shows separate handling: `deserialize_struct` uses the
renamed metadata, while the visitor's default expectation uses the Rust type
name. The supported `expecting = "struct Persisted"` container attribute supplies
the original diagnostic. C2 accepted an exact one-attribute correction, with a
README explanation, yielding core SHA-256
`d774f1f202ce9a717d07cec334785e918bf8955710cc20d9190be47337ffe2ec`.
That correction was committed separately as
`50a0949d457e6116b51dff5d0cb3993b1a7d7b14`. The corrected central rerun below
passes all comparisons; the frozen oracle and test expectations remain unchanged.

The initial node storage run passed all 22 selected tests (15 new host cases and
7 retained store tests), with zero failures or ignored tests and 319 filtered
out. C2 read both complete streams; they contain no compiler warning or error
diagnostics. The native host stack compiled, but runtime selection executed
only these isolated storage tests. The corrected-source repeat below also passes.

## Corrected central validation

The corrected assembly is
`fad0a4e2c24308960790a073124d45406923a711`, whose entire tracked tree matches
C1's independently reviewed `50a0949d` assembly. Formatting became central
commit `b27a110dce10e3cdd5fce23076b8af3eaffec85b`; the diagnostic correction is
the resulting `fad0a4e2` commit. No fixture expectations, frozen source inputs,
Cargo manifests or locks changed in these corrections.
Integration record `6eb01c3d-a8c7-44f6-9613-a582f37cd801` succeeded with exit 0
in 0.168 seconds, with complete 349-byte stdout and empty stderr.

The following runs use that same frozen source on local Windows x64. The manager
supplied one shared target directory, `CARGO_PROFILE_DEV_DEBUG=0`,
`CARGO_PROFILE_TEST_DEBUG=0`, `CARGO_INCREMENTAL=0`, the existing Visual Studio
CMake executable and `CMAKE_POLICY_VERSION_MINIMUM=3.5`. `TEMP` and `TMP` point
to the exclusively created `target/storage-validation-20260919-0709/tmp`
inside the manager worktree. Test processes use one test thread. These are
manager-owned native runs; no worker compiler, formatter or test execution was
used. C2 independently inspected complete retained streams for every row.

| Command after `cargo` | Durable record | Exit / seconds | Complete stdout / stderr bytes |
| --- | --- | --- | --- |
| `fmt --all -- --check` | `ec9e2a10-a7ec-4f1d-9646-282d44fdbfaf` | 0 / 2.196 | 0 / 0 |
| `fmt --manifest-path node/Cargo.toml --all -- --check` | `6639b787-58db-4450-86f6-fef56185c185` | 0 / 2.071 | 0 / 0 |
| `test -p allmystuff-storage --locked --offline -- --test-threads=1` | `e9ac530d-cae9-4c89-9335-2cdd625748d2` | 0 / 3.425 | 2341 / 424 |
| `test --manifest-path node/Cargo.toml --locked --offline --lib storage_plan:: -- --test-threads=1` | `8efddfd7-3211-4e2b-a632-0b96286b0d68` | 0 / 21.324 | 2353 / 401 |
| `test --manifest-path node/Cargo.toml --locked --offline --lib persist::tests:: -- --test-threads=1` | `e2291a1c-722a-4184-bc3e-7ec91ebde267` | 0 / 0.923 | 251 / 152 |
| `clippy --workspace --all-targets --locked --offline -- -D warnings` | `cef4d726-c2a9-489d-bc92-b27b33fc37f3` | 0 / 41.693 | 0 / 8029 |
| `clippy --manifest-path node/Cargo.toml --all-targets --locked --offline -- -D warnings` | `1191fc73-168f-4a09-a3e6-6cfb3f4dc951` | 0 / 69.100 | 0 / 9197 |
| `check --manifest-path node/Cargo.toml --all-targets --no-default-features --locked --offline` | `37f872f6-f6c6-467e-971a-c39d390bd44a` | 0 / 43.701 | 0 / 1647 |

All 52 distinct selected tests pass at the corrected assembly: 28 core comparisons,
15 new host persistence tests, 7 retained store tests and 2 existing persistence
helper tests. None failed or was ignored. The node storage filter excludes 319
other node tests; the helper filter excludes 339. The package's unit and doctest
targets each contain zero tests; those successful empty targets add no cases.
Initial and repeated executions do not inflate this distinct total. The listed
successful streams contain no warning/error diagnostics.

## Output cleanup

After validation, the manager's cleanup record
`3f89dd34-75da-4923-9523-2198f3b0ae57` succeeded with exit 0 in 15.076 seconds,
with complete 944-byte stdout and empty stderr. C2 read the full retained
command/output and the cleanup script, plan and result. It removed 2,273,530,560
bytes (2.12 GiB) in 6,512 regenerable files: native `target/debug/build`,
`.fingerprint`, `incremental`, and 1,960 `.rlib`, `.rmeta`, `.d`, `.lib` and
`.exp` intermediates in `debug/deps`.

The guarded cleanup checked absolute paths, rejected reparse points and tracked
target files, and verified SHA-256 for all 1,307 protected files after removal.
C2 independently rehashed those files and checked the receipt's byte accounting.
Retained test executables, DLLs/PDBs, prior proof/log artifacts and storage
evidence were preserved. Git status and the tested source were unchanged.
The recorded target size fell from 2,987,427,127 to 714,288,467 bytes; the latter
includes the newly written cleanup plan and precedes the result receipt.
The manager retained `cleanup-plan.json`, `cleanup-result.json`,
`cleanup-intermediates.ps1` and `final-gates.json` under
`target/storage-validation-20260919-0709/`. No worker cleanup or rebuild ran.

The final package README was independently accepted and integrated as
`4da38bc9997cd1d8fc4f5cc8cb66b44bade55ed5` via record
`2dee441a-77b4-4352-8e9e-3dd018f1765d` (exit 0, 0.095 seconds, complete
158-byte stdout and empty stderr). Its sole change from the tested assembly is
documentation; the tested production and fixture bytes remain unchanged.

## Verification scope and limits

The baseline commit `0505d7956bbbee47a845d4db6db2f9712269a0dd` and fixture/report
commit `57dd694c64b00372de4651ccbacbf997d0550f85` have C1's independent exact-byte
source acceptance. All selected runtime cases and all eight final gates pass
as recorded above. Compilation and lint across targets do not imply execution
of all root or node tests.

Unchanged Mesh source preserves authenticated network/sender admission and
caller-derived management permission, local volume/capacity/materialization
checks, and durable-change-before-notification ordering. No direct existing
test of those live Mesh storage paths was found; core authorization decisions
and isolated filesystem results do not establish fleet integration coverage.
Unix permission checks require a Unix run. Replacement and selected I/O failures
do not establish power-loss behavior, directory-fsync durability or coverage of
every pre-rename error. Broader file-transfer, mount, site, GUI/mobile runtime
and RISC-V qualification remain outside this slice.
