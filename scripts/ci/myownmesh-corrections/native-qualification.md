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
