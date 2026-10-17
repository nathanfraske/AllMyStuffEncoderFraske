This recipe qualifies a separately patched MyOwnMesh candidate. It does not
change the application pin, product defaults, installed services, accounts or
IPC permissions. The application baseline is published
`69f5913c21ff5db56ac51fac0a485fc5c74b8205`; the complete failing MESH03 source,
payloads and native results remain preserved. Use the final reviewed application
task commit for execution, not an uncommitted worker draft.

The upstream base is exactly
`db7818e09fedd98899490347b86ac9bc9f97b59b`, tree
`5492d1845e15dbc067acec3ec592bb9500b9e21d`. Its canonical Git Cargo.lock SHA256 is
`8f5a30ce863c43b0a8c5225f74d7446aed7656cd7032f47e70030526381930c6`.
The ordinary Windows CRLF working form has a different hash, `20db0e89...`;
it is not the canonical source-byte input below. A tree name alone cannot bind
silently converted working bytes to a binary.

Use a **new private source checkout**. Never apply patches to the retained
MESH03 baseline, a shared source tree or an installed binary. Disable automatic
newline conversion in this checkout before populating it. With new explicit
paths selected by the manager:

```sh
git init "$mesh04_new_upstream"
git -C "$mesh04_new_upstream" config core.autocrlf false
git -C "$mesh04_new_upstream" remote add origin https://github.com/nathanfraske/MyOwnMeshSecurityReview.git
git -C "$mesh04_new_upstream" fetch --depth 1 origin db7818e09fedd98899490347b86ac9bc9f97b59b
git -C "$mesh04_new_upstream" checkout --detach FETCH_HEAD
git -C "$mesh04_new_upstream" rev-parse HEAD 'HEAD^{tree}'
```

The selected patch set must be complete and reviewed before preparation. A
placeholder hash, an unordered patch directory or a branch name is not an
input. `patch-set.json` uses schema `myownmesh-corrections-patch-set-v1`:

| Field | Meaning |
| --- | --- |
| `kind` | Explicit `diagnostic` or `corrected`; a corrected set refuses diagnostic patches |
| `base_commit`, `base_tree` | The exact base identities above |
| `expected_tree` | The reviewed final index tree after all selected patches |
| `cargo_lock_sha256` | The exact canonical base lock hash above; dependency changes are refused |
| `patches` | Nonempty ordered objects with sibling `path`, exact raw `sha256`, `purpose`, `tree_before` and `tree_after` |

Each patch purpose is `production`, `regression` or `diagnostic`. The first
`tree_before` is the base tree, every successor matches the preceding result,
and the final result equals `expected_tree`. Individual author exports retain
their base/blob evidence. Preparation applies only the reviewed bytes through
stdin, with `git apply --check --index --whitespace=error-all` followed by one
application. It verifies every result tree and every actual tracked working
file against its index blob. No fuzzy fallback, patch retry, lock regeneration
or automatic source repair occurs.

With an existing evidence directory outside the private checkout and a new
receipt filename:

```sh
python3 scripts/ci/myownmesh-corrections/prepare-candidate.py prepare \
  --upstream "$mesh04_new_upstream" \
  --patch-set scripts/ci/myownmesh-corrections/patch-set.json \
  --patch-set-sha256 "$mesh04_reviewed_patch_set_sha256" \
  --output "$mesh04_evidence/candidate-source.json"
```

The source receipt uses `myownmesh-corrections-source-v1`, records
`source_kind:"patched-candidate"`, `source_commit:null`, the explicit base,
actual resulting `source_tree`, selected manifest SHA, ordered patches, lock
hash and the working/index-byte check. HEAD remains the base while the index
contains the selected changes; it is never reported as a clean unmodified db
checkout. A failed preparation preserves its private copy and a failed receipt
when possible. Inspect that exact state rather than resubmitting preparation.

Before and after each central build, reverify the selected source receipt:

```sh
python3 scripts/ci/myownmesh-corrections/prepare-candidate.py verify \
  --upstream "$mesh04_new_upstream" \
  --source-receipt "$mesh04_evidence/candidate-source.json" \
  --source-receipt-sha256 "$mesh04_source_receipt_sha256"
```

Native execution belongs to the manager's durable runner. Retain the actual
target hostname, OS/distribution/kernel, architecture, account, Rust/Cargo 1.88
and compiler/linker/CMake evidence. Native Linux must be non-WSL; Intel macOS
must actually be x86_64. Use the existing reviewed prerequisite recipes in
[mesh-control-paired.md](../mesh-control-paired.md), without another dependency
manifest or changes to services. Source copies and Cargo targets are private;
the stock-feature and transport-lab artifacts have separate target paths and
receipts. The exact stock-feature build command is:

```sh
cargo +1.88.0 build --locked --release --no-default-features -p myownmesh --bin myownmesh
```

The separate relay-lab build command is:

```sh
cargo +1.88.0 build --locked --release --no-default-features --features transport-lab -p myownmesh --bin myownmesh
```

Both commands require the explicitly selected `CARGO_TARGET_DIR` for that
artifact. A successful build command must have a retained JSON receipt with
the exact `argv`, source `cwd`, `result:"passed"`, `exit_code:0` and complete
stdout/stderr byte lengths, SHA256 and EOF. The native driver records these
fields; the preparation helper never compiles or starts a daemon itself.

After source verification and a successful build, bind the actual executable:

```sh
python3 scripts/ci/myownmesh-corrections/prepare-candidate.py record-build \
  --upstream "$mesh04_new_upstream" \
  --source-receipt "$mesh04_evidence/candidate-source.json" \
  --source-receipt-sha256 "$mesh04_source_receipt_sha256" \
  --build-command "$mesh04_evidence/corrected-candidate-build.complete.json" \
  --artifact "$mesh04_actual_daemon_executable" --feature-set stock \
  --rustc-evidence "$mesh04_evidence/candidate-rustc.stdout" \
  --cargo-evidence "$mesh04_evidence/candidate-cargo.stdout" \
  --output "$mesh04_evidence/candidate-build.json"
```

For the lab artifact select its own complete command/executable/evidence paths
and `--feature-set transport-lab`. Build schema
`myownmesh-corrections-build-v1` retains the explicit patched source identities,
source-receipt SHA, manifest SHA, ordered patches and actual executable
**top-level `sha256` and `bytes`**. It records native/toolchain evidence and
`default_features:false`, with `features:[]` or `["transport-lab"]`. Null
`source_commit` deliberately makes the old stock-only validator refuse it.
These are source/build evidence records, not application consent or a new
daemon authority. No raw client/flow capabilities belong in a receipt.

The actual paired controller keeps its existing protected state, same-account
IPC, finite owner-selected resource/profile policy, whole-AU/Opus payloads,
strict first-unit/SEI/clock/decode checks, pacing and owned cleanup. For a
corrected build add these explicit arguments to its reviewed command:

```sh
--candidate-kind corrected \
--daemon-build-receipt "$mesh04_evidence/candidate-build.json" \
--daemon-build-receipt-sha256 "$mesh04_build_receipt_sha256"
```

`--candidate-kind diagnostic` accepts only a diagnostic patch set and records
`diagnostic_only`; it cannot qualify a corrected artifact. The default stock
mode remains available for preserved baseline diagnostics and still requires
the original exact source/feature/artifact mapping. Patched bytes cannot enter
that stock mode. Actual executable SHA/length and the whole corrected receipt
SHA must match before startup. Direct and separately selected relay-lab runs
keep the existing explicit policy/ports, new evidence directories and no
automatic allocation/port retry.

For that verified diagnostic kind only, both owned daemon environments get the
fixed `MYOWNMESH_LOG=warn,mesh_first_unit=debug` and
`MYOWNMESH_LOG_FORMAT=json` values from the actual daemon main's supported
overrides. Inherited MYOWNMESH variables are still removed first. No arbitrary
parent filter is forwarded; stock/corrected logging remains unchanged. The
reviewed diagnostic patch must bound its first-unit counters and omit raw
capabilities, payloads, private config and response strings. These diagnostics
observe the unchanged strict pair; they do not insert warm-up units or waits.

Run new deterministic first-unit regressions at their final reviewed test
selectors, then the strict real transport and node-consumer AV gates on native
Windows, Intel Mac and Linux. Source or fake-endpoint regressions alone do not
establish delivery. Successful local writes do not establish remote delivery,
decode or track readiness. Preserve an actual failure and its stage; no hidden
warm-up, arbitrary delay, discard, relaxed comparator or larger resource grant
is a substitute for correctness. Quiet-EOF/ambiguous-ACK owner regressions and
their controller remain separately owned; their corrected receipts use the
same source/build schema.

The final workflow extension is restricted to
`agent/1503a621-mesh04-correctness`. Existing MESH03 stock routing and cumulative
and default/manual workflow behavior remain. Only the reviewed final manifest,
patch SHA/tree chain and native-driver inputs may enter that corrected job.
No worker draft or placeholder patch-set is executable qualification input.
Publishing upstream changes is a separate final concrete-review boundary;
this recipe neither pushes upstream nor changes main or installed runtimes.

The startup controls now have an observed native failure to discriminate.
Retained diagnostic run `85ea5484` (receipt SHA256
`b8b19ae1d51221b1ab69eb24ce9a5a5d5dc50afb26cb2b8045a089fe1e80cdce`)
admits audio unit 0, 371 bytes/SHA `f05fbbb5...`, then reaches the native
`sample_not_ready` branch with neither packetizer nor sequencer. That original
branch returns `Ok` before packetization. Unit 1, 297 bytes/SHA `8a44f9f6...`,
is packetized, written and retained by the receiver peek; it is the first
**audio** received when index 0 was expected. The first overall received unit
was video. This establishes that run's prebind loss, not a later sender reuse
failure. The retained stock and earlier diagnostic failures remain failures.

`startup-regression.json` is an executable preparation manifest of kind
`diagnostic` for the baseline negative control: exact db plus only the test
patch, tree `9f096b27d5edc934b21e29bfe62284297f5e622f`. It cannot qualify a
corrected binary. `patch-set.json` instead orders the accepted owner cleanup
fix and its regression, the startup fix, then the identical startup regression
patch. Its exact tree is `bdbf1b96c7809107bd3a151b2ccf9bcad9ace73a`, and its
raw LF SHA256 is
`4cb553e829e61dab2858357b1a2c031a2cf5995d0e49eaa9770517c345a497e6`.
The regression patch's raw SHA256 is
`da7afd74b297aae0215789206ec429cf2d09c1154addf4be4c514ada2f8c6cf1`.
All include declarations are test-only; no application media comparator,
policy, vector, pace or product pin changes. Preparation uses new baseline and
corrected raw LF checkouts and the existing helper's exact manifest SHA input.

The four full names and their actual packages are recorded in
`startup-regression.json`. Both targets are **library unit tests**, with two
new names each:

| Package | Full test name |
| --- | --- |
| `webrtc` 0.13.0 | `track::track_local::startup_regression::mesh04_prebind_sample_waits_then_emits_first_once_in_order` |
| `webrtc` 0.13.0 | `track::track_local::startup_regression::mesh04_cancelled_prebind_sample_is_never_emitted_after_bind` |
| `myownmesh-core` | `transport::webrtc::tests::startup_regression::mesh04_prebind_queue_keeps_first_payload_funded_until_finite_close` |
| `myownmesh-core` | `transport::webrtc::tests::startup_regression::mesh04_connector_retirement_detaches_unbound_idle_sender_before_queue_close` |

The vendor cases use the existing `write_sample` API, actual `TrackLocal.bind`
and packetizer with a bounded capture writer. They prove pending first-sample
custody, exact once/order/clock on bind, and no emission from a cancelled
pending caller. They do not decode or establish network delivery. The core
cases call the existing `attach_outbound` entry with an actual unbound native
sender, the existing funded queue and cleanup owner. They check payload leases,
flow-close ACK after native detachment, quiescence and task joining, plus
connector retirement while the queue remains alive. A current-thread ownership
observation proves the unchanged pump has started; actual funding, detachment
and ACK are the behavior assertions. Finite five-second test bounds and
`yield_now` observations add no media delay, sleep, warm-up or retry. Expected
baseline assertion failures are checked after owned core cleanup, rather than
leaving a baseline's idle pump alive.

The vendor crate is excluded from the upstream workspace. Its **existing**
`env_logger` and `tokio-test` dev dependencies are absent from canonical lock
`8f5a30ce...`. The manager selected separate vendor unit copies and one explicit
test-only lock. Never resolve those dependencies into the official core or
daemon source. Use the unmodified vendored `Cargo.toml`; no new manifest,
dependency declaration, production patch override or package installation is
part of the unit recipe. The standalone graph uses registry dependencies in
place of the root workspace's local `[patch.crates-io]` overrides. Record that
distinction: these primitive controls do not qualify the canonical daemon graph.

With fresh owned unit directories outside every Cargo workspace, extract
each exact prepared vendor subtree. `$mesh04_base_tree` and
`$mesh04_fixed_tree` are the verified receipt trees stated above, and every
archive/directory named here must be new:

```sh
git -C "$mesh04_base_source" archive --format=tar --output "$mesh04_base_archive" "$mesh04_base_tree:vendor/webrtc-0.13.0"
git -C "$mesh04_fixed_source" archive --format=tar --output "$mesh04_fixed_archive" "$mesh04_fixed_tree:vendor/webrtc-0.13.0"
mkdir "$mesh04_vendor_base" "$mesh04_vendor_fixed"
tar -xf "$mesh04_base_archive" -C "$mesh04_vendor_base"
tar -xf "$mesh04_fixed_archive" -C "$mesh04_vendor_fixed"
cp "$mesh04_base_source/Cargo.lock" "$mesh04_vendor_base/Cargo.lock"
cargo +1.88.0 metadata --manifest-path "$mesh04_vendor_base/Cargo.toml" --format-version 1
```

The copied canonical lock is an explicit seed. That sole metadata command may
resolve the standalone test-only lock; it is not a compiler or daemon pass.
Retain its complete JSON/stdout/stderr and exit state. Before building, record
the actual package IDs, all lock records added/removed/changed versus the seed,
manifest/source hashes and resulting test-lock SHA. Compare these generated
records narrowly against the reviewed existing manifest; stop on a manifest
change or unexpected source input. Copy the resolved unit lock byte-exact to
the fixed unit copy, then verify it with:

```sh
cp "$mesh04_vendor_base/Cargo.lock" "$mesh04_vendor_fixed/Cargo.lock"
cargo +1.88.0 metadata --locked --manifest-path "$mesh04_vendor_fixed/Cargo.toml" --format-version 1
```

Both official core/daemon locks must still hash to `8f5a30ce...`, and both unit
locks must have the same recorded test-only SHA. No later test command may
resolve or update a lock. Select a separate owned `CARGO_TARGET_DIR` for each
baseline/fixed build. For each vendor copy the actual build command is:

```sh
cargo +1.88.0 test --locked --manifest-path "$mesh04_vendor_source/Cargo.toml" --lib --no-run --message-format json
```

For each official baseline/fixed core source, retaining its canonical lock:

```sh
cargo +1.88.0 test --locked --no-default-features -p myownmesh-core --lib --no-run --message-format json
```

Parse complete Cargo JSON and select only the actual named package's `lib` test
artifact with its executable present; retain the package ID, target/source,
executable SHA/bytes and native provenance. A binary target or a compiler
failure is not a negative-control result. For **each** of those four executables
first run the inventory, compare exact names against the two appropriate rows
above, then execute the same filtered cases:

```sh
"$mesh04_actual_test_executable" startup_regression::mesh04_ --list
"$mesh04_actual_test_executable" startup_regression::mesh04_ --nocapture --test-threads=1
```

Require exactly two listed/executed tests per executable and no ignored cases.
Preserve baseline runtime failures with their exact expected assertion stage;
zero matches, a linker error or an unrelated timeout is not proof of the old
defect. Require the corrected executables to pass all four source-qualified
cases. Retain complete command EOF, statuses and source/lock/artifact hashes;
then reverify both official source receipts. These controls precede the
unchanged strict real transport and node AV probes on native Windows, Intel
macOS and non-WSL Linux. Sender close/reopen, received software decode, peer
restart, direct/relay paths and hardware/product gates remain separate actual
evidence; the startup patch or primitive controls alone do not qualify them.

The first standalone vendor build exposed a recipe API mismatch. Run
`0908ec6a`, audit SHA256
`1fccbfd211ac2d6ef4568735eee668e4e81674ddad66a0bd2525e1b82d3a658a`,
failed Rust compilation with exactly two `E0277` diagnostics at unchanged
`src/ice_transport/mod.rs:270` and `:288`. Both await
`agent.add_remote_candidate(&c)`. Registry `webrtc-ice` 0.13.0 makes that method
synchronous; the exact-db vendored API is async. There was no test executable
or runtime negative-control result. Preserve the complete failed build and
the earlier generated lock `aa288ed5...` and record check `c1f20648...`.
That earlier acceptance covered the actual input records, not compatibility
with this source or a successful build.

For this failed criterion only, the following explicit test config supersedes
the earlier vendor metadata/build invocations. The existing root
`[patch.crates-io]` already maps `webrtc-ice` to
`vendor/webrtc-ice-0.13.0`. Select **only that override** with
`vendor-unit-ice-config.toml`, an explicitly passed Cargo config, without
changing either existing Cargo.toml. The selected common dependency is the
already verified canonical baseline source at
`C:/ams-mesh04-vendor-baseline-0b4ac426/vendor/webrtc-ice-0.13.0`:

| Input | Exact identity |
| --- | --- |
| Upstream commit | `db7818e09fedd98899490347b86ac9bc9f97b59b` |
| ICE subtree | `19ff26c90544c3dfe97986ade93598cf94ee7d0f`, 62 files |
| ICE Cargo.toml blob | `cc64b4cbfd728d51af1e8b40e3d5f5165aff1d78` |
| ICE Cargo.toml SHA256 | `97822d89789682b6d0cf9bec2637a6fd5f9215dfec0bd60360dcd3f1f602a74a` |
| Cargo config SHA256 | `ddd9953d330206b9854c744308bbcd47a8eab1d08b46de812a7450cb598c5dc4` |

The ICE manifest is byte-identical to the existing registry manifest.
Compared with that registry source, its non-test Rust changes are confined to
`agent/mod.rs` and `agent/agent_internal.rs`; they implement its async
candidate and owned-task API using already declared dependencies. Other
production files, including TURN gather/client-close/error calls, are exact.
The vendored ICE tests do use additional vendored TURN test APIs, but ICE is a
dependency here: root `webrtc --lib` does not select ICE's test or example
targets. Adding TURN for those unselected tests, or copying all root overrides,
would enlarge this repair beyond the observed mismatch. Keep TURN,
interceptor and SCTP on the previous registry graph. Do not remove either
await or change production code to fit the wrong dependency API. Central
resolution and compilation must still prove this minimal selection.

Use **new** private unit and target directories outside every Cargo workspace;
do not overwrite the old graph, failed build, unit copies or targets. Extract
the same reviewed baseline/corrected vendor subtrees into these new copies,
preserving each archived original vendored lock. Recheck the existing
93-file inventories and unchanged root `webrtc` manifest `12dbdf3e...`.
Verify all 62 shared ICE working files against the exact subtree above, with
no extra source/manifests or symlinks, before and after commands. The common
dependency source and all four official root locks remain unchanged.

The following manager-owned names are new inputs; fail if any already exists:

| Purpose | Path |
| --- | --- |
| New baseline unit | `C:/ams-mesh04-vendor-api-baseline-unit-0b4ac426` |
| New fixed unit | `C:/ams-mesh04-vendor-api-corrected-unit-0b4ac426` |
| New baseline target | `C:/ams-mesh04-vendor-api-baseline-target-0b4ac426` |
| New fixed target | `C:/ams-mesh04-vendor-api-corrected-target-0b4ac426` |

`$mesh04_vendor_unit_config` must be the absolute path of the exact reviewed
`scripts/ci/myownmesh-corrections/vendor-unit-ice-config.toml`. Record its raw
SHA256 and use the same file for **both** sources and **every** command below.
Its absolute ICE path prevents dependence on config-file-relative lookup or
the current working directory. Do not add default config, environment patch
overrides, another Cargo manifest, or another source/package acquisition.

Copy the retained lock from the failed standalone graph, SHA256
`aa288ed53273a6f307335698e86842da487419994c81681a1295ea33468acaff`,
into only the new baseline unit copy as the explicit forward seed. Retain the
seed separately. Resolve exactly once with the changed config:

```sh
cp "$mesh04_retained_aa_lock" "$mesh04_vendor_api_base/Cargo.lock"
cargo +1.88.0 --config "$mesh04_vendor_unit_config" metadata --manifest-path "$mesh04_vendor_api_base/Cargo.toml" --format-version 1
```

Retain complete command streams, actual package/source/feature records and all
forward-lock additions/removals/changes versus that seed. Record the actual
new lock SHA only after resolution; no unresolved hash is a build-ready input.
Require the exact shared vendored ICE path and manifest above, while the root
baseline/fixed source distinction remains the reviewed startup patch. Other
dependencies keep the previous registry selection unless actual records
establish a necessary difference for the existing manifest. Stop for narrow
review on any unexplained source, version, feature or manifest change. No
crate's own dev tests become new qualification targets. This mixed graph is
still a standalone primitive-test graph, not the canonical daemon graph.

For the paired graph check, copy that one new lock byte-exact to the fresh
fixed unit copy and check it without resolution:

```sh
cp "$mesh04_vendor_api_base/Cargo.lock" "$mesh04_vendor_api_fixed/Cargo.lock"
cargo +1.88.0 --config "$mesh04_vendor_unit_config" metadata --locked --manifest-path "$mesh04_vendor_api_fixed/Cargo.toml" --format-version 1
```

Both forward locks must have the same recorded SHA and both config inputs
must have the same raw SHA. Verify those actual generated records before
compilation. Then, with distinct new owned target paths, build each vendor
root with the exact same explicit config:

```sh
cargo +1.88.0 --config "$mesh04_vendor_unit_config" test --locked --manifest-path "$mesh04_vendor_api_source/Cargo.toml" --lib --no-run --message-format json
```

The exact library artifact selection, two-name inventory and semantic
baseline-negative/corrected-pass requirements above remain unchanged. Recheck
both 93-file unit inventories except their selected test locks, the 62-file
shared ICE input, config hash, forward locks and all four canonical `8f5a...`
root locks after the commands. The regression patch, all four test bodies,
corrected source manifest `4cb553e8...`, daemon build and strict AV predicates
are unchanged. The canonical core controls already have their separate
before/after result; this recipe correction requests no repeat of those tests.

The current final candidate extends the earlier four-patch input
`4cb553e829e61dab2858357b1a2c031a2cf5995d0e49eaa9770517c345a497e6`.
That earlier input and its native results remain historical evidence. The
current `patch-set.json` is kind `corrected`, with raw LF SHA256
`a9edda5de43430f4eb0407bad11352d9d7eeda4e36c866bdeb7298873e8fe279` and exact resulting tree `c87e382172530f898a0f351557c24552926daad1`.
It covers owner cleanup, startup first bind and sender reopen source. Strict
native AV, reconnect and relay qualification remain pending. Temporary
flow-bind/refusal/first-unit observation patches are absent from this input.

The first four ordered records are byte/data-identical to the retained
four-patch manifest. Record five is the independently accepted
`sender-reopen-fix.patch`, raw SHA256 `4e93bbc7e96ae871fd53b40b1eeaf4204e162116c4c0ff9582ab07de4017e012`, applied from
`bdbf1b96c7809107bd3a151b2ccf9bcad9ace73a` to `717d7a360f9ce8196b17cb75cfee8f00e54bf50e`.
Record six is the unchanged stamped test export from original
`eec65c8209109740e4edc2f4a86f3cac51688f97`,
`sender-reopen-regression.patch`, raw SHA256
`978d54016c38e318043eaf4bb03dc28eb0146dca665ef2a7442ccab301650173`.
Its declared before/after trees reflect its position after the production
patch: `717d7a360f9ce8196b17cb75cfee8f00e54bf50e` to `c87e382172530f898a0f351557c24552926daad1`. The patch bytes and test child
blob `1895c20ec3df068233a2e634f6b66f934eeb5fea` are unchanged. All six records
retain the canonical root lock SHA256 `8f5a30ce...`; no manifest, dependency,
test predicate, media vector, pace, resource ceiling or product pin changes.

The sender control has two retained native results. The original unnegotiated
fixture at tree `e64f9bf4e6fec84bcba4e787120500f68c212cd3` passed one test in
`0.020771` seconds. Its expected-negative wrapper failed because the fixture
did not reach the reuse precondition; audit SHA256
`4f62b1fb228dffd8e585b99b018614e5ddf2b874ba1951f350f0ea97bc2833a3`.
The stamped fixture at tree `31313a0c0a071d6f97522fde65cc3eb163c788d2` then
produced the intended native failure: zero passed, one failed, 1087 filtered,
exit 101 in `0.049043` seconds. Expected-negative wrapper run `4a943301`
passed; audit SHA256
`51045a276f8d588870398466021f227152b68b2d9b83256a4dc5ece22c1f7731`.
The final same-ID `Attached` assertion observed `RefusedIncorrectEnvelope`
after the offer MSID, initial attachment/removal, different-ID `Attached`
control and bounded successful PC close. These are native lifecycle results;
the earlier unnegotiated passing result remains valid control history. The
original `f76a69d3` receive timeout is a separate unresolved observation.

Use the final reviewed application task commit containing the accepted new
production export, unchanged stamped regression and this selected manifest.
Materialize all selected manifest/patch files as their exact canonical LF
bytes; rehash their raw contents before preparation. The existing helper
records actual patched source identity, ordered trees and `source_commit:null`.
Preserve the earlier test-only `e66706be...` manifest and every historical
failure/source/receipt. No diagnostic/test-only receipt qualifies the final
release artifact.

One newly prepared final source may serve the canonical unit gate and release
build. The manager reserved `C:/ams-mesh04-sender-reuse-corrected-0b4ac426`
and its unique `C:/ams-mesh04-sender-reuse-corrected-target-0b4ac426` target.
Its pristine preparation receipt is
`target/mesh04-correctness/sender-reuse-corrected-lf-base-preparation.json`
in the manager's evidence tree. Verify that actual source is still exact
db/tree5492/canonical8f5 before the first preparation; preserve a failed input
rather than recreating it. Select a new outside evidence directory. Apply the complete
manifest once with the existing `prepare` command above, selecting the exact
current SHA256 `a9edda5de43430f4eb0407bad11352d9d7eeda4e36c866bdeb7298873e8fe279`, and retain its new source-receipt SHA.
Never run `prepare` again on that patched source. Use the same selected source
receipt with `verify` immediately before and after every following command.
Stop on a failed or ambiguous verification and inspect that exact state.

With the source root as cwd and that explicit private `CARGO_TARGET_DIR`,
the one changed canonical core gate builds with the existing locked graph:

```sh
cargo +1.88.0 test --locked --no-default-features -p myownmesh-core --lib --no-run --message-format=json
```

Select the actual `myownmesh-core` library test executable from complete Cargo
JSON, recording its package/target/source, SHA256 and bytes. Its full test name
is unchanged. Require exactly one listed and executed unignored case:

```sh
"$mesh04_actual_core_test_executable" transport::webrtc::tests::sender_reopen_regression:: --list --format terse
"$mesh04_actual_core_test_executable" --exact transport::webrtc::tests::sender_reopen_regression::native_sender_same_id_reopens_after_awaited_remove --nocapture --test-threads=1
```

The final candidate must exit zero with one passed and zero failed or ignored.
An absent name, unrelated setup/control/close failure or wrapper-only success
does not satisfy it. The already retained meaningful baseline negative is the
comparison; this sequence requests no identical baseline or startup-control
rerun. Keep the complete unit build/list/runtime receipts, EOF streams,
artifact identity and before/after source verification separate from the
daemon release receipts. `record-build` is for the exact daemon build command,
not for this unit executable.

After the unit gate and its source re-verification pass, retain the same
source receipt and manager-selected target. Debug unit and release artifacts
have separate profiles and separate retained receipts. Run the exact
stock-feature command from that source root:

```sh
cargo +1.88.0 build --locked --release --no-default-features -p myownmesh --bin myownmesh
```

Reverify that source and its canonical lock, then use the existing
`record-build` command above with the actual complete release command receipt,
stock executable and Rust/Cargo evidence. Retain a new build-v1 receipt and
its whole-file SHA; the source and ordered patches must match the final
selected manifest. The actual command receipt records the source cwd and
complete streams. No unit result or an older candidate binary substitutes for
this release evidence. A separately selected transport-lab build keeps its
own target, feature set, before/after verification and build receipt.

The native driver and exact task-branch workflow still bind the previous
four-patch identity until their separately reviewed binding refresh. Hold that
refresh until this final manifest is frozen and the central component result
is available. The refreshed route must pin this exact six-record manifest,
result tree and honest scope, retaining existing stock/cumulative routing.
It must not accept an arbitrary hash, partial scope or diagnostic candidate.
The unchanged strict transport and node AV gates then require their actual
received payload/SEI/order/clock/decode, independent lifetime, refusal,
pressure, cancel/reconnect and selected direct/relay evidence. Source or unit
passes do not qualify those gates, and no installed service or upstream
publication is part of this recipe.
