# Terminal library extraction: compatibility and validation

Validation record, 2026-09-19. The operator authorized one reusable terminal/PTY
package with behavior retained and scoped testing. The frozen input is
`ee9cc150f4bdc3fd49a547b563f0381dae3685f5`, after the completed storage extraction.
This report belongs to C2's original assignment
`manager:f272ab36-6959-4312-bb0f-91cda702f403`; the known task-board projection
failure does not create a replacement assignment.

**Current status:** all 67 distinct selected Windows test definitions passed,
including the corrected nine-case native lifecycle suite. The selected commands
produced 84 passing executions, including 17 public definitions repeated in
viewer and host builds.
All four dependency graphs and planned formatter, lint and consumer checks passed.
The initial replay fixture assumption and formatting failures remain recorded.
Production behavior is unchanged. Verified cleanup reclaimed 2.17 GiB while
preserving the retained binaries and evidence described below.

A separate native Linux extension subsequently passed all 22 selected Unix
cases on local Ubuntu 24.04.4 under WSL2: 20 PTY cases and two pure cases.
Its source, runner, cache failures and actual results are recorded in
[the WSL validation extension](#native-unix-validation-extension-under-wsl).

## Frozen evidence and ownership

The [baseline receipt](../../../crates/allmystuff-terminal/tests/baseline/manifest.json)
records 18 complete Git blob/SHA256 identities, nine verbatim source ranges and
four independent literal-vector files. C1 independently checked all entries
against the original source. C2 committed the 15 baseline files alone as
`99d7c1222bf0eeb132a8cc9a87e5d5d23f40e579`, directly on the frozen input.
The receipt's staged LF identity is blob
`1eea4fe1de4c24561bbf6e18280bb0a3bc7ba84e`, SHA256
`431ce476b13f0cbc7675573858624c301e5a421ee19f05c0cabd040b9e36117c`.
An initial handoff instead labeled its raw CRLF hash as LF; independent review
caught that accounting error before commit. The source and literal bytes were
unchanged.

The real terminal input is blob `2e6c0a323bc921264812048251554bc9a5bfeb5b`
(50,983 LF bytes, 1,317 lines); the viewer stub is
`9c5bf31baf8cc6d07504a102d6b9347f16320fff` (4,560 bytes, 144 lines).
Other frozen inputs include the node runtime, shared byte queues, Mesh terminal
callers, node-control dispatch, session wire framing, protocol session rows,
application manifests and all four lockfiles.

C1 owns production modules, node adapters, manifests, locks and package README.
C2 owns the independent baseline, public contracts, channel-only host comparisons
and this report. A1 independently reviews caller authorization/runtime/features,
node wiring and A2's lifecycle fixtures. A2 owns only
`tests/support/lifecycle.rs`, committed as
`ba5cf08ca2ff78b2fffda3120ce19e0b0653d837`, accepted source blob
`96d9feb816f8a320ce8c2c8110f3761eea9ddd8c`, SHA256
`e9eeebc787578c7abad833a8b49bb7e90ed2bd06ce3486169ffcecbc86e7cc20`.
Before that commit A1 caught a fixture expectation missing the four-byte length
prefix in `poll("b")`; the fixture was corrected to the frozen framing contract.
That was a test-authoring correction, not a production behavior change.

C1 accepted C2's exact seven-file fixture/report checkpoint, committed as
`449756c1802d666d4dea1e9b101e12d8d5ecef59`. The final host fixture at that
checkpoint is blob `57c1479933a4e1c406922342a3458f89536abebe`, LF SHA256
`4da3a0b8b992af3ccce22ceac7cf7e217031e8c5817663c2d1c576171b2bbfad`.
Peer review corrected one helper comment to distinguish manually polled bridges
from captured reaper futures later run on a private paused runtime. No test code,
literal expectation or oracle byte changed in that correction.

Manager integration `63b78ded-3ff0-4185-944c-e36a8c520306` succeeded (exit 0,
0.181 s, 1,597 stdout/0 stderr bytes). It mapped baseline `99d7c122` to
`32d3ddc5d7ea03dbdba7805a2f1bf6939014a1e7` and lifecycle `ba5cf08c` to
`fcb03f69a80bb2b5c0822a8e30b94652a58d9693`. The manager verified empty path
diffs to both source commits and a tracked-clean checkout, apart from managed
`AGENTS.md`. This integration is not a compiler or runtime result.

Production integration `f36307a8-2894-4e53-8f4f-507edef5ad24` succeeded (exit 0,
0.150 s, 476 stdout/0 stderr bytes), mapping C1 `6a39465f` to central
`c4229627b809604d8f7372224a85b1d9605e6668`. The manager's complete tracked-tree
comparison matched the reviewed source. C2 independently read both integration
records completely.

## Library and application boundary

The [package](../../../crates/allmystuff-terminal/README.md) is
`allmystuff-terminal`, separate from the existing `allmystuff-term` CLI. Its empty
default feature set provides `viewer::TerminalHost`, with real viewer queues and
the original hosting refusal. `host` enables the PTY implementation. The viewer
module remains explicitly available with `host`, so Cargo feature unification
cannot silently turn node's host-disabled adapter into a shell host.

The real `TerminalHost<S: TaskSpawner>` retains the original state and adds only
`PhantomData<fn() -> S>`. Its policy receives the original unit-output Send future
at the two old spawn sites: idle reaping and the legacy broadcast-to-mpsc bridge.
No runtime is captured on construction. Node aliases the generic host to a
doc-hidden `NodeSpawner` that calls the unchanged `crate::spawn`.

Node's `OnceLock` runtime still accepts the first registration and ignores later
ones. A missing runtime still panics at the original spawn site, potentially
after detach mutations or after a shell was created. Neither ambient Tokio
runtime fallback nor new early validation is introduced. The library has no
production runtime registry. Its retained old tests use separate test-only glue.

Independent inversion of C1's source commit
`6a39465f145baf41956589e700449e904c945920` restores the complete original host
file after only the generic policy/marker, import, nine explicit original tracing
targets, two spawn substitutions and old-test alias glue are removed. The stub
restores exactly after its import and two documentation links are mapped back.
All 14 existing host test bodies and literals are preserved. C2's first inverse
comparison omitted one stub documentation-link substitution; the corrected
comparison passed before acceptance.

The final 13-file source receipt has LF SHA256
`00582e1ef3b7423ea069bbb902fa5a8187465c3fafb25ffa47b0e8e92df21979`.
C1, C2 and A1 verified the applicable candidate identities. Node runtime/lib,
Mesh, node-control dispatch, shared byte queues and GUI/mobile manifests are
byte-identical to the frozen input after checkout line-ending normalization.

## Behavioral details preserved

- Viewer output keeps the four-MiB **payload** cap, whole-chunk eviction and
  little-endian `u32` length framing. Empty chunks count as queue entries while
  consuming no payload budget. An oversized single chunk can be evicted entirely
  while `enqueue` still returns the original empty-to-nonempty notification.
  Watch tokens come from the original per-host counter; replacing a watcher
  preserves bytes and stale unwatch calls cannot remove the current owner.
- Host queues remain 256 broadcast/control slots, the reader buffer is 8 KiB,
  scrollback is 256 KiB, and the session cap is 32. A cap rejection happens before
  automatic ID allocation. Existing-session attach bypasses that cap.
- A joining route inherits current reconciled dimensions, ignoring its initial
  placeholder. Reconciliation takes independent minima, clamps zero to one and
  preserves the `u16::MAX` sentinel fallback to 80x24. Even an unchanged size
  attempts a control send. Failed resize delivery can still update state and
  broadcast the new size; it does not roll those effects back.
- Snapshot and subscription stay under the scrollback guard. Repeated attach
  with the same route replaces one entry. Reusing a route for a different session
  can leave its old attacher entry; no cleanup correction is bundled into this
  extraction. Listing preserves metadata and unordered HashMap iteration.
- `close` kills once and removes all mappings to a session while retaining
  viewer queues. `stop` also removes only the named route's viewer queue. Real
  host `detach` removes that queue and preserves a shared shell; viewer-only
  `detach` remains a no-op. Natural process exit broadcasts status but does not
  itself remove session or route maps.
- The one-hour idle future checks emptiness and the captured generation. Actual
  creation initializes generation to 1 and never increments it. A recycled empty
  ID with generation 1 can therefore match an older timer. The stronger old
  comment does not override this implementation behavior.
- The legacy bridge replays nonempty scrollback as one Data message, forwards
  messages with bounded awaits, skips broadcast lag and continues after Exit.
  It ends on source closure or failed output send. With empty replay, a closed
  sink is noticed only when a live message arrives; no early-closed check is added.
- Shell fallback order, environment/cwd discovery, three blocking worker threads
  and error strings remain unchanged. The 120 ms exit linger is retained; it is
  not a reader join or proof that every future byte precedes Exit. Tracing keeps
  the `allmystuff_node::terminal` target, levels and fields.

## Caller authority and feature checks

A1 traced terminal offers, admission, host/loopback start, inbound frames and
session-list requests. Mesh keeps live `sender_may_drive` checks for the terminal
plane, consent and exact share grants. Generic terminal routes do not inherit
room authorization. Inbound data needs an active terminal route and its
authenticated canonical peer binding; viewer Exit cannot kill a host session.
Local `term_send` also checks active routes and the canonical local sink. Queue
watch/poll methods remain trusted local IPC plumbing, not shell authorization.

Teardown remains asymmetric: local `Mesh::disconnect` uses `terminal.stop`, while
`Effect::StopMedia` and unreachable-viewer pump cleanup detach. The GUI close-tab
path reaches disconnect; it cannot be described as universal detach. Desktop GUI
uses node IPC, mobile uses the existing in-process node-control dispatcher, and
the CLI uses the existing node-client terminal wrapper. These callers are outside
the extracted package and remain unchanged.

Node still defaults to `host`; desktop keeps that default, while mobile's
`default-features = false` plus `audio-io` keeps its explicit viewer path. The
original `portable-pty` dependency remains the `xpty` 0.3.6 alias with default
features intact, including the Windows default that avoids inherited-cursor DSR
handshake behavior. No new backend, wire protocol or authorization path is added.

## Manifest and lock review

Root/node manifests reverse exactly to baseline after removing the new local
workspace/package edges and restoring node's original host feature edge. Normal
library dependencies are byte queues, Serde and Tokio sync; optional host edges
are dirs, parking_lot, xpty, tracing and Tokio rt/time. Byte queues itself already
depends on parking_lot and tracing. The default therefore has no direct native
PTY edge, but features can be unified by another consumer; absence of native
dependencies in every possible combined workspace is not claimed.

Root lock records increase from 493 to 501: the local terminal package plus seven
identities already frozen in the node lock (xpty 0.3.6, downcast-rs 2.0.2,
filedescriptor 0.8.3, nix 0.29.0, shell-words 1.1.1, windows-sys 0.59.0 and winreg
0.55.0). Four existing dependency references gain explicit version qualifiers;
their resolved identities are unchanged. Every old external record, checksum and
resolved edge is preserved. Each imported external record and its resolved edges
matches the baseline node package.

Node grows 607 to 608 records, GUI 804 to 805 and mobile 735 to 736. Only the new
local package and intended node-to-terminal routing change; historical consumer
version differences remain. Those source comparisons alone do not establish
resolution; the separate Cargo evidence below establishes the captured graphs.

The final crate manifest is blob `e090ab2640468932e49009c418d9f1b7333ea2d3`,
LF SHA256 `b83fdb2b7f29f6163314f0ed59dc9cdb01889e93e940f0e1d22dc6458bc54c68`.
Serde JSON is dev-only for literal fixtures. Tokio's dev-only `test-util` feature
enables paused-clock checks; pinned Tokio source expands it to the already-used
rt/sync/time features without a new dependency record or production feature.

Four Cargo metadata runs at central `c4229627` used
`metadata --format-version 1 --filter-platform x86_64-pc-windows-msvc --locked --offline`,
with the additional arguments in the table. They ran through retained
`target/terminal-validation-20260919-01/capture-metadata.py`, SHA256
`608830805288acf03e69a8dfe8a8b5747d1b7ac078b6dd91d78bed86dd9903b6`.
The wrapper writes each complete JSON to an exclusively created file in that
directory, preserves stderr in the durable run and reports the exact Cargo
vector, exit code, file size and hash. C2 read the wrapper and all four terminal
run streams, rehashed all JSONs and independently walked their normal/build
edges. The four reviewed lock hashes remained unchanged afterward.

| Graph and extra arguments | Run | Exit, seconds, stdout/stderr bytes |
| --- | --- | --- |
| Root default; none | `607386c8-62c2-44f2-bf9d-2b99f8c2629b` | 0; 5.441; 1,427/0 |
| Root host; `--features allmystuff-terminal/host` | `6984b512-75f1-4aa4-ad9c-279cd8a0ef28` | 0; 0.718; 2,064/0 |
| Node default; `--manifest-path node/Cargo.toml` | `20d4e37f-68e1-45aa-939b-4f42070b931a` | 0; 2.286; 1,875/0 |
| Node viewer; `--manifest-path node/Cargo.toml --no-default-features` | `5f6c6c60-3c74-4f70-b9c4-b36dec9868d9` | 0; 0.902; 1,305/0 |

The exact retained JSON identities are:

| File in the validation directory | Bytes | SHA256 |
| --- | ---: | --- |
| `root-default-metadata.json` | 1,039,718 | `a303cd2ee3af625c0c34592ae6fb979f0a32d295c76d7166936e370fce0d4fc6` |
| `root-host-metadata.json` | 1,089,141 | `daae086af650baae76bff916f387724f1beb30bf89740bc2a178a8a2898a2e89` |
| `node-default-metadata.json` | 1,667,384 | `473cb20bb8378bb5d2847d34def9e7b10ecb45874d3d6a04cb322d8d12996ef9` |
| `node-no-default-metadata.json` | 1,362,387 | `ad5c0c1034cfb6d737f9f4811ca42638f33385749e26dc85657871cf59f3ebf3` |

The two viewer graphs enable terminal's `default` feature only and reach 28
normal/build package identities including terminal. Their direct normal edges
are byte queues, Serde and Tokio. Both host graphs enable `default, host`, add the
dirs/parking_lot/tracing/xpty direct edges and reach 52 identities. In all four,
the only local packages reached are terminal and byte queues: no reverse node,
node-client or CLI edge exists. Xpty appears exactly in the two host closures.
These are Windows workspace-resolved graphs, with workspace/dev feature
unification reflected in dependency nodes, including Tokio IO/runtime support.
They are not universal minimum-feature or cross-platform closure claims.

## Test inventory and isolation

The [fixture guide](../../../crates/allmystuff-terminal/tests/README.md) describes
the frozen oracle and new tests. Source counts and isolation are listed here;
actual platform-selected execution is recorded below:

| Group | Source inventory | Isolation |
| --- | ---: | --- |
| Public contract fixtures | 17 viewer-only / 18 with host | Memory, Serde and channels; no PTY or runtime registration |
| Host compatibility | 30 | Frozen old/new comparisons with fake children, channels and private paused clocks |
| A2 lifecycle | 9 on Windows or Unix | Explicit shells, exclusive private cwd, bounded observations and cleanup |
| Retained terminal tests | Windows 3; Unix 13 | Two pure cases plus platform-selected real PTY cases |

The four idle comparisons cover before/after-delay behavior, reattach, synthetic
generation mismatch and unchanged recycled-ID matching. They cross the deadline
by one millisecond for timer granularity; exact timer-tick equality is not asserted.
The bridge tests capture futures and poll them without a process-global runtime.
Fake sessions guard `open_with` so no channel comparison can enter native creation.
The complete old host prefix is retained as an oracle, but its old native tests
are excluded from that module and are not accidentally executed twice.

A2's native fixtures own an exclusive canonical directory and explicit child
commands. They retain receivers for teardown observation, close sessions on Drop
and keep the directory when shutdown cannot be observed. The Windows cmd and
native dimension helpers were exercised in the Windows runs below. Unix fixed-path
shell/stty behavior was pending at that checkpoint and was subsequently exercised
in the separate WSL extension. Synchronous native open/close is bounded by the
manager run's outer timeout.

A1 identified narrow existing node filters for terminal route shape, canonical
loopback identity, privileged-offer refusal and per-plane share grants. The
runtime registration test can run separately. The superficially pure terminal
dedup test is excluded because it constructs ControlClient/Mesh and can reach
default state; broad Mesh suites are outside the isolated test plan.

## Executed baseline evidence

The manager reused retained Windows binary
`allmystuff_node-601391fbaa519f2d.exe`, SHA256
`b30a0baac3602d9e9dfc57792705970837d0c109fa15214ad235363c397f50dc`.
It was compiled in storage run `8efddfd7-3211-4e2b-a632-0b96286b0d68` from
`fad0a4e2c24308960790a073124d45406923a711`. The difference to the frozen terminal
baseline is exactly four documentation files, recorded in
`target/terminal-validation-20260919-01/baseline-binary.json`. TEMP/TMP use the
manager's private terminal-validation scratch directory. This avoids a redundant
baseline rebuild while preserving the actual compiled-source identity.

C2 independently read both complete terminal records to EOF:

| Run | Exact binary arguments | Result and retained bytes |
| --- | --- | --- |
| `bc147af5-f1c4-47ad-93d8-c078060dcec1` | `terminal::tests:: --test-threads=1` | Exit 0, 0.820 s; 3 passed, 338 filtered; 327 stdout / 0 stderr |
| `48661f9b-590b-4d0d-9311-7526cce2ee8a` | `engine_spawn_runs_tasks_from_a_non_runtime_thread --test-threads=1` | Exit 0, 0.060 s; 1 passed, 340 filtered; 189 stdout / 0 stderr |

The first run covers DSR probe counting, scrollback tails and the cmd echo/exit
ConPTY test. The second is a separate process that registers a runtime and spawns
from a plain OS thread; it constructs no Mesh or default application state.
These are four distinct baseline tests, with no warning/error diagnostics in the
retained streams. They do not validate the newly extracted library.

## Initial formatting gates

The manager assembled the accepted source, fixtures and both test-only includes
at `0068be54e8e0fbfdb55f221fb924acdfacb4d276`, with a tracked Git tree identical
to C1's `5a41451b44d12461fcafbb27e356384da81aa95f`. C2 independently read both
complete formatting records to EOF:

| Run | Cargo arguments | Result |
| --- | --- | --- |
| `d51d99f4-f5f1-413a-a7c8-53b1b4cbee27` | `fmt --all -- --check` | Exit 1, 3.705 s; 38,102 stdout / 0 stderr bytes |
| `a2e26508-de51-44cc-aac9-5b4c8f8ce5d9` | `fmt --manifest-path node/Cargo.toml --all -- --check` | Exit 1, 2.185 s; 38,102 stdout / 0 stderr bytes |

Both stdout streams are byte-identical and contain exactly 52 formatting hunks:
37 in the channel fixtures, ten in the lifecycle fixtures and five in the public
contract fixtures. There are no production, frozen-baseline, oracle or harness
formatting hunks. C1 applied this bounded correction. C2 independently
reconstructed every requested file from the retained output and original Git
blobs, matched the resulting bytes and accepted them. All literal sequences,
test names and assertion counts remain; the other 841 tracked files, including
production, frozen baselines, oracle and shared harness, are unchanged.

| Corrected fixture | LF SHA256 | Git blob |
| --- | --- | --- |
| `tests/support/host_compatibility.rs` | `7f5dd118304e883a854771836d66e11b16ac6c2111ff86fad76871b53689c7c2` | `6279f0c301faa83d3ee9ab1508e270cee607100b` |
| `tests/support/lifecycle.rs` | `336e358e46237dc5ce6c8bb3ec9eb881597c856648d22bbc772969adc9be1d10` | `2219a0ef9ade3d619b6cb5f77442414ca72d9f26` |
| `tests/contract_compatibility.rs` | `e9b5679bb7928c9c324f1550ca0f38139f667005410d3265c4d6af70294f1ab8` | `e2b981a2e3659d08b767d1e71691ca1d70e427b2` |

C2 retains the original log, exact data-only reconstruction and acceptance in
`target/terminal-extraction/formatter-independent-review.json`. The common
stdout SHA256 is
`609a8fd994c53851b92e207956630b17c49827acc89e4db2719db30ac29ce2af`.
These initial failures remain part of the evidence and are not runtime-test
results. No worker ran Rust formatting, compilation or tests.

C1 committed the exact correction as
`b2cb2eae1c2b3ac4303607e6b607fad7f4835273`. Integration
`d9b71a74-b567-4c42-96ca-0ac04d2d53f1` succeeded, exit 0 in 0.104 s,
169/0 stdout/stderr bytes, yielding manager source
`29d982f93e567aa95c4cce5605271c814522dbc0` with the same tracked tree.

## Executed extraction validation

The following gates ran centrally on Windows x64 at `29d982f`. C2 read every
retained stream completely, verified byte lengths and terminal status, and found
no compiler warnings. Cargo commands use the manager's existing native build
environment, debug information and incrementality disabled, with TEMP/TMP in
the private terminal-validation scratch directory. Tests are serialized with
`--test-threads=1`; no ordinary application state or broad Mesh suite is used.
Durations below are durable run wall times, not Cargo's internal compile times.

| Gate / exact Cargo arguments | Run | Exit 0 duration; stdout/stderr bytes |
| --- | --- | --- |
| `fmt --all -- --check` | `5ad009a9-74c2-4424-8b13-0ba0dbb9980a` | 4.993 s; 0/0 |
| `fmt --manifest-path node/Cargo.toml --all -- --check` | `3e5724e2-8047-4fd4-b191-a94d38b3710b` | 2.655 s; 0/0 |
| `clippy --workspace --all-targets --locked --offline -- -D warnings` | `7a3120b5-539f-4331-833e-6b87109f062d` | 44.112 s; 0/8,296 |
| `clippy -p allmystuff-terminal --features host --all-targets --locked --offline -- -D warnings` | `fda0ff2a-ed94-46fd-8c01-e4957693c685` | 38.930 s; 0/1,022 |
| `clippy --manifest-path node/Cargo.toml --all-targets --locked --offline -- -D warnings` | `8989a778-e22d-4cff-9cda-96a1a5675eac` | 88.839 s; 0/9,913 |
| `check --manifest-path node/Cargo.toml --all-targets --no-default-features --locked --offline` | `3370c846-dfc4-4c87-94da-422db3823542` | 46.529 s; 0/1,785 |

All four following Cargo test commands end with `-- --test-threads=1`:

| Exact Cargo arguments before that suffix | Run | Actual result; duration; stdout/stderr bytes |
| --- | --- | --- |
| `test -p allmystuff-terminal --no-default-features --locked --offline` | `0ac8a4fd-d408-4595-93f0-2639510ea35d` | 17 passed; 16.117 s; 1,535/1,392 |
| `test -p allmystuff-terminal --features host --locked --offline --test contract_compatibility` | `8707b82c-62fb-4688-b1a7-b6e384affb4c` | 18 passed; 7.925 s; 1,371/1,098 |
| `test -p allmystuff-terminal --features host --locked --offline --lib host::compatibility::` | `f07705ec-eee5-42f7-85b3-7ed46430ce25` | 30 passed, 12 filtered; 2.620 s; 3,217/294 |
| `test -p allmystuff-terminal --features host --locked --offline --lib host::tests::` | `44f7f786-ddbf-4ff3-a7a6-b6d8bad4979a` | 3 passed, 39 filtered; 0.555 s; 314/156 |

These four test groups exited 0 without failures or ignored tests. The default
library unit and doc-test targets each contain zero tests; they add no cases.
Seventeen public definitions run in both feature configurations, so 17 plus 18
is a feature matrix, not 35 distinct definitions. The host marker auto-trait
case accounts for the extra host-only definition. The three retained Windows
tests are the same definitions exercised in the baseline, now in the library.

## Reproduced native replay fixture failure

The first lifecycle command was `cargo test -p allmystuff-terminal --features
host --locked --offline --lib host::lifecycle:: -- --test-threads=1` at
`29d982f`. Run `e0f66a83-fbee-4086-a95f-1ee2fdb7e8ea` failed with exit 101
in 2.565 s, 1,422/221 stdout/stderr bytes: eight passed, one failed and 33 were
filtered. `scrollback_then_live_output_has_no_gap_or_duplicate` observed two
`AMS:BEFORE` occurrences where its fixture expected one. All other lifecycle
cases, including native Windows size reconciliation, passed in that run.

Run `d13816ba-a37f-453a-a2d0-8c95fd2af0c2` reused the same retained host-test
binary with that exact test and `--exact --test-threads=1`. It reproduced the
same count failure: exit 101, 0.258 s, 683/0 bytes; one failed, 41 filtered.
The recorded binary SHA256 is
`abd011e712d69b2fbf476a9f3fe6e50e899488504f2d6c57d9cd19d0e11fd612`.
This historical identity predates the later diagnostic rebuild of that path.

A1 reviewed A2's diagnostic-only commit
`161b8b11c9109f5ecafde58076106a6f9eb1fc01`: 36 lines of bounded escaped/raw
observations, preserving commands, assertions and poll order. Integration
`390ab09f-1f58-414d-a738-faa75d7d75e5` succeeded, exit 0 in 0.104 s with
149/0 bytes, producing `c4ae019a45177fdfb5e1d9a4f9b2d8d760d7393e`.
Only the lifecycle fixture differs from `29d982f`; all production is identical.

Diagnostic run `226a4a3c-dfa3-4470-b963-75065a646b48` selected that one test
with `--exact --test-threads=1 --nocapture`, failed again with exit 101 in
2.493 s, and retained 297/12,062 stdout/stderr bytes. C2 independently decoded
all 12 raw byte segments and verified every reported length, with no omitted
bytes. At attach, A's 101 live bytes equal B's 101 replay bytes. Before CONFIRM,
A's full 359 bytes exactly equal B's 101 replay plus 258 live bytes. Both have
two BEFORE markers; the shared suffix contains cursor-home and erase-line
repaint sequences. Thus the extra marker was already present in the original
live-only observer, not introduced solely by replay. The final diagnostic
phase stops A at AFTER and B at CONFIRM, so that phase proves no equal-fence
comparison. No production resize, output filtering or deduplication was added.

A1 accepted A2's sole-fixture correction
`a79ecc4b63a22dd1a519e8fa7df9474f67c2b843`, blob
`6087e1ce90d123cbdc13168950b9faec699aa546`, LF SHA256
`3e4d2b3b4d72e7ed3955b49bf22ff4237586d3c75fa3f3ce66efd248322ec3ce`.
C2 read the complete difference from the formatted fixture: it uses the same
fresh CONFIRM acknowledgment for both observers, then compares their complete
raw streams and exact replay prefix, with an explicit scrollback-cap guard.
Temporary diagnostics and the unused counting helper are removed; commands,
other fixtures, cleanup and production remain unchanged. This replaces the
single-marker assumption with strict byte equality, without filtering output,
changing native resize, hardcoding two occurrences or adding retries. Integration
`7db57b5b-8c96-47f5-bfa7-eb9d4c3b65a0` succeeded, exit 0 in 0.120 s,
170/0 stdout/stderr bytes, producing
`7300cf71ae0ee9c9049c9029c6d74d62b588a40c`. The subsequent root formatting run
`c00a0004-5b8b-42a1-a5e4-55f489ab64a1` failed with exit 1 in 2.215 s,
1,567/0 bytes. C2 read both complete records; the latter contains two hunks
wrapping only three new assertions. A1 independently reconstructed and accepted
A2's formatter-only commit `eea5a6c1cc7a132e3e687d11c128e4e346d50265`, final
fixture blob `5b2e3905555fb95d388e6fc1d2df25ff89987fe3`, LF SHA256
`7f2fd66921754cd16e5eb75bed793243f6ca5ef19ddceeb4e5b50f6baba12dd6`.
Integration `b3392e53-e3be-468d-8859-36751c84f66e` succeeded with exit 0 in
0.097 s, 147/0 bytes, producing final tested source
`82af488dc926d5ded8711e511a54b91aef21d7c3`. C2 verified that only the lifecycle
fixture differs from the first tested `29d982f`; production and the previously
passing test groups remain byte-identical.

C2 independently read all four final affected gate records to EOF:

| Exact Cargo arguments | Run | Exit 0 duration; stdout/stderr bytes |
| --- | --- | --- |
| `fmt --all -- --check` | `2fcdf45f-b1b1-4c03-bc20-40049a92b9e1` | 2.182 s; 0/0 |
| `fmt --manifest-path node/Cargo.toml --all -- --check` | `b811cfa3-cd1d-44cf-a9a7-711d6a3e914c` | 2.101 s; 0/0 |
| `test -p allmystuff-terminal --features host --locked --offline --lib host::lifecycle:: -- --test-threads=1` | `eb449aa4-fe6a-40c8-a189-47966625cd03` | 4.872 s; 933/294 |
| `clippy -p allmystuff-terminal --features host --all-targets --locked --offline -- -D warnings` | `7c16f892-61eb-4a53-8fc2-64acd28bcd92` | 1.258 s; 0/198 |

The final lifecycle run passed all nine cases, zero failed or ignored, 33
filtered. It includes the corrected full raw replay/live equality assertion and
the original eight successful lifecycle cases. There were no compiler warnings.
Initial failures and diagnostic remain evidence and are not counted as passes.

## Selected node runtime and caller evidence

Run `71f4ea9e-ae54-449a-801c-aa3c41fab16a` used
`cargo test --manifest-path node/Cargo.toml --locked --offline --lib
mesh::tests::engine_spawn_runs_tasks_from_a_non_runtime_thread -- --exact
--test-threads=1` at `c4ae019`. It passed one test, 337 filtered, exit 0 in
101.687 s, with 189/10,289 stdout/stderr bytes and no compiler warnings.
The new node binary is `target/debug/deps/allmystuff_node-0d876981d2166e53.exe`;
C2 independently verified SHA256
`602adf0e62b55adcaeb20b9a2417a36ef3832568429f34179cfd96f96a6f81e9`.

The following six separate processes reused that binary. Every argument is
`mesh::tests::<name> --exact --test-threads=1`, with one pass, zero failures or
ignored tests, 337 filtered, and empty stderr:

| Name | Run | Duration; stdout bytes |
| --- | --- | --- |
| `terminal_routes_are_recognized_by_shape` | `e945b141-d424-4803-9720-403823c8326a` | 0.076 s; 179 |
| `loopback_terminal_route_is_recognized_as_self_hosted` | `fcdee9e5-7cfa-43d0-82f1-fb696d8ed156` | 0.071 s; 192 |
| `loopback_is_detected_across_node_id_forms` | `11005834-3695-477a-9c87-99456b8f14af` | 0.060 s; 181 |
| `term_send_loopback_check_is_canonical_across_id_forms` | `45dbc737-17f3-402e-85ef-95b8d1b4c496` | 0.073 s; 193 |
| `privileged_offers_are_refused_exactly_when_unauthorized` | `95b1a8e3-5d3a-4bf7-a495-b9229838bed8` | 0.120 s; 195 |
| `share_grants_authorize_exactly_their_own_plane` | `9f3af63a-0503-4135-bd79-dd2e095ee630` | 0.047 s; 186 |

These seven cases cover the isolated caller/runtime selections, not a complete
Mesh session or live authority exchange. The final selected set contains 67
distinct definitions: 18 public, 30 channel, nine native lifecycle, three retained
terminal and seven node cases. Across both public feature configurations and
the other selected groups, this yields 84 passing executions. Baseline passes, failed attempts
and diagnostic reruns do not inflate either final count. C2 retains the first
22 completed record reads and byte-level diagnostic review in
`target/terminal-extraction/audited-validation-01.json` and
`runtime-cleanup-independent-review.json`.

## Verified cleanup and remaining limits

C2 read and accepted C1's minimally adapted cleanup script, LF SHA256
`c22bdc2858bed0ca7990a2e0c22c3b5c19093ef37ccbe0c3f87f4b4ce88136b3`,
6,781 bytes. Reversing only its expected-HEAD parameter, terminal evidence path
and receipt-label changes reproduces the prior reviewed storage script exactly.
The fixed manager workspace and target guards, reparse checks, three permitted
intermediate directories, five permitted dependency-file extensions, protected
file SHA256 checks, Git status check and refusal to reuse plan/result receipts
remain unchanged. The manager ran that exact script after final gates with
`-ExpectedHead 82af488dc926d5ded8711e511a54b91aef21d7c3`. Durable run
`e3e40836-a04b-4506-b147-bb5aec5427a3` succeeded with exit 0 in 25.065 s,
956/0 stdout/stderr bytes. C2 read the complete command and streams to EOF and
independently verified the resulting plan, receipt and all protected hashes.

The cleanup removed 2,333,247,573 bytes (2.17 GiB), 6,730 regenerable files,
including 2,045 direct dependency intermediates. Only `target/debug/build`,
`.fingerprint`, `incremental` and the allowed `.rlib`, `.rmeta`, `.d`, `.lib`
and `.exp` files directly in `target/debug/deps` were selected. C2 confirmed
the selected paths were absent and independently rehashed all 1,336 protected
files, totaling 774,217,132 bytes, including retained EXE/DLL/PDB files, prior
proofs, metadata, source inputs and validation evidence. Git status was unchanged,
with only managed `AGENTS.md` untracked; source and test bytes were unaffected.

The recorded target size changed from 3,107,464,705 to 774,620,718 bytes before
the final receipt. The difference includes the newly written 403,586-byte plan:
`3,107,464,705 - 2,333,247,573 + 403,586 = 774,620,718`. The manager retains
`target/terminal-validation-20260919-01/cleanup-plan.json` and
`cleanup-result.json`; C2's independent receipt is
`target/terminal-extraction/cleanup-result-independent-review.json`.
The cleanup script was not replayed, and no worker executed deletion,
Rust tools or a PTY.

Windows evidence must remain separate from Unix/macOS/mobile execution. A
feature-disabled compile does not prove mobile signing, packaging or runtime
behavior. Isolated shells and channel models do not test live fleet permissions,
GUI tab interactions, service/session-agent launches or every user shell/profile.
Source preservation of those boundaries is recorded above; platform or live-state
coverage will only be claimed for actual reviewed runs.

## Native Unix validation extension under WSL

The separately authorized Unix validation starts from completed Windows commit
`6e6b8558212e58d540a09935303387d47b3a305e`. C2's new review and documentation
assignment is `manager:3847eff4-8ac7-4d0f-bfa5-d0e533a5e8f9`; the completed
Windows assignment and its evidence remain separate. No production or fixture
bytes changed. The reviewed native build and both selected Unix modules passed;
the 22 selected definitions comprise 20 PTY cases and two pure cases.

The manager's retained `environment-preflight.json` records Ubuntu 24.04.4 LTS
under local WSL2, x86_64, kernel `6.6.87.2-microsoft-standard-WSL2`, running as
root. Existing native Rust is selected through `/root/.cargo/bin` rather than
the default WSL PATH: rustc 1.97.1
(`8bab26f4f68e0e26f0bb7960be334d5b520ea452`, LLVM 22.1.6), Cargo 1.97.1,
GCC 13.3.0 and Python 3.12.3. The installed 1.88.0 toolchain is not the selected
validation compiler. A separate `unshare --mount --pid --fork --kill-child
--mount-proc /bin/true` preflight succeeded. This establishes available
prerequisites and namespaces, not a passing terminal test.

The scoped Linux normal/build dependency command was:

```text
cargo tree -p allmystuff-terminal --features host \
  --target x86_64-unknown-linux-gnu -e normal,build --locked
```

| Attempt | Durable run | Result; elapsed; stdout/stderr bytes |
| --- | --- | --- |
| Same command with `--offline` | `7b37b883-0f32-495b-98ba-9e2fc52f3fd2` | Exit 101; 4.529 s; 0/399 |
| Locked fetch and graph | `5e72466e-61b2-4d88-b13f-3ee8097d3b1a` | Exit 0; 3.419 s; 2,531/202 |

C2 read both complete commands and streams to EOF. The first attempt found no
`xpty` entry in the native Cargo cache. The second downloaded the already pinned
`filedescriptor` 0.8.3, `shell-words` 1.1.1, `downcast-rs` 2.0.2, `xpty` 0.3.6 and
`nix` 0.29.0. No installer or dependency-version refresh was used. Its graph has
37 distinct normal/build package identities, all present in the unchanged root
lock. The only local packages reached are terminal and byte queues. These graph
commands did not compile or execute tests. The retained manager inputs are under
`target/terminal-unix-validation-20260919-01/`; C2's complete graph audit and source
identities are under `target/terminal-wsl-review/`.

A1 and A2 independently audited the Unix source and cross-checked each other's
findings. C2 also read the retained test bodies and lifecycle shell, directory,
observation and cleanup helpers. The agreed native selection is two module
prefixes, in separate serialized processes: `host::tests::` selects 13 definitions
(11 PTY and two pure), and `host::lifecycle::` selects nine PTY definitions.
Module-prefix selection uses no libtest `--exact` option. Actual discovery and
passing test names matched this independently frozen inventory; no node/GUI or
unrelated contract suite was included.

The retained tests explicitly launch `/bin/sh -c` with `cat`, `sleep`, `stty size`
or a fixed exit status. They do not exercise default or login-shell discovery.
The `cat` smoke assertions also accept PTY input echo, so they do not separately
prove the child application's output. The newer lifecycle tests use private
shell scripts and a fixed native `stty` path, with markers absent from input,
shared-shell state, exact replay/live byte equality and resize observations.

The outer runner must supply a private existing Linux HOME and TMPDIR before
the first `CommandBuilder`, along with a controlled Unix PATH and no inherited
shell startup settings. xpty caches the initial environment and defaults an
unspecified cwd to HOME. Its child calls `setsid`; killing only the test process
group cannot contain every PTY descendant. The retained tests close after their
assertions and have no general close-on-panic guard. The newer fixtures close
owned sessions on Drop and observe output-channel closure with a bounded
deadline, but that does not prove every descendant or worker has joined.
Synchronous PTY open/close also lies outside those observation deadlines.

C2 independently accepted C1's complete runner and recipe, including
`run_unix.py` SHA256
`d934cb32856f2f2c54483457a09b4445d67069238312cf8646344370dc096f45`
(22,800 bytes). The source manifest SHA256 is
`889d5d5bc611089f346bd06c63b5a84172add89d8f26fa73c2281def4a1daab3`;
the archive is
`aedd49bb4249eca5b186df0c8fd7e0ab30f0cae68211f480bb127ed04329ab59`
(3,551,380 bytes). C2 verified every archived content hash, Git blob and file mode
against all 844 tracked files at the exact completed commit: 15,524,180 source
bytes, with no `.git`, untracked state or previous build output included.

The recipe extracts those committed LF bytes to a mode-0700 Linux ext4 directory
at `/var/tmp/ams-terminal-unix-20260919-01`. Source, target, private Cargo home and
per-attempt HOME/TMPDIR/XDG directories are separate from the preserved Windows
target. Only the existing registry cache is linked into the private Cargo home;
Cargo config and credentials are not imported. The build checks the inventoried
native rustc commit and executes only:

```text
cargo test -p allmystuff-terminal --features host --lib \
  --target x86_64-unknown-linux-gnu --locked --offline \
  --no-run --message-format=json
```

The runner accepts one recorded ELF64/x86_64 libtest executable. Each runtime
phase rechecks its hash, records full and filtered discovery, requires the exact
audited name set, and uses `--test-threads=1 --color never`. A zero exit plus the
exact passed/failed/ignored/measured/filtered summary is required. Every attempt
has exclusive retained logs and result files, with all source hashes, modes and
four locks verified before and after execution. No automatic retry, online build
fallback, installation or recursive deletion is implemented.

Each phase has a private mount/PID namespace and namespace-local `/proc`, verified
before cleanup is enabled. The inner PID 1 reaps children and applies bounded
TERM/KILL cleanup to remaining namespace processes. Inner build/test deadlines
are 1,200/600 seconds; outer Linux deadlines are 1,320/720 seconds. Interruption
or timeout kills the owned launcher group, with `unshare --kill-child=KILL` and
namespace teardown covering PTY descendants that called `setsid`. The outer
supervisor also checks for remaining members of that PID namespace. Native
devpts is inherited; this is not a general filesystem or network sandbox.
Source acceptance of this recipe is distinct from observing its runtime results.

The first native build attempt, `build-offline-01`, ran the exact accepted recipe
in durable record `9ac7f872-4f06-48a5-8fec-2fcbdde763db`. It exited 1 after
5.536 seconds (2,368/0 outer stdout/stderr bytes); the inner Cargo command exited
101 before compilation because pinned `memchr` 2.8.1 was absent from the native
cache. The complete 120-byte `build.stderr.log` records that offline download
failure. C2 read the full durable record, toolchain output, isolation record and
failure logs, and verified the before/after source receipt: all 844 files and
four lock hashes were unchanged. Both the inner cleanup and outer namespace
census were empty. This failed attempt remains retained; no compiler or test
pass is inferred from it.

The manager then ran the scoped `cargo tree` command with `-e normal,build,dev
--locked` in the private source/Cargo home. Record
`54fe9c6f-4f1c-4361-ae25-38f3aa62b8ed` succeeded in 2.636 seconds with
2,881/51 stdout/stderr bytes and fetched only `memchr` 2.8.1. The exact runner
was reused in a distinct `build-offline-02` attempt, preserving the first failure.
The following durable run timings include WSL and runner overhead; stdout/stderr
sizes are the outer run streams, whose JSON points to the complete nested logs.

| Phase | Durable run | Result; elapsed; stdout/stderr bytes |
| --- | --- | --- |
| Offline native libtest build | `84cb720b-7136-444d-b9e0-47d232604f65` | Exit 0; 13.775 s; 2,634/0 |
| Retained Unix module | `c9757952-90d2-40c1-9ebd-e9b28dba2671` | Exit 0; 13 passed, 39 filtered; 6.268 s; 4,320/0 |
| Unix lifecycle module | `bd2b36cc-ee79-4db2-ae2f-1c8ab297e793` | Exit 0; 9 passed, 43 filtered; 4.588 s; 3,467/0 |

C2 read every complete terminal command and stream plus the nested build, test,
discovery, launcher and isolation records. The build emitted 66 valid Cargo JSON
records: 54 compiler artifacts, 11 build-script records and one successful
build-finished record, with no compiler-message diagnostics. It produced one
33,320,632-byte native ELF64/x86_64 libtest executable,
`allmystuff_terminal-b2627474e65bbecd`, SHA256
`a53e40937475f9d1229d25cf6f0276f2ec78132561b483e37d4833a54b250a6b`.
C2 independently rehashed that binary and all 844 extracted source files through
read-only filesystem access, matching the recorded artifact and every frozen
Git blob/SHA256. No worker executed WSL commands, Rust tools or a PTY.

Both runtime phases used that same binary and discovered the same 52 unit tests.
Their exact commands were `BINARY host::tests:: --test-threads=1 --color never`
and `BINARY host::lifecycle:: --test-threads=1 --color never`, inside their
separate reviewed namespace processes. Every selected name passed, with zero
failed, ignored or measured cases. Nested test stdout/stderr sizes were 789/0
and 933/0 bytes; measured test-process durations were 1.222 and 1.572 seconds.
The other 30 channel comparisons were discovered but not executed in this
extension. No repeated Windows executions are added to this 22-case count.

All phases verified the same 844 source files, modes and four LF lock hashes
after execution. The successful build and lifecycle module had empty inner
cleanup inventories. The retained module left six already-exited zombie child
entries; namespace PID 1 reaped them. All final inner and outer namespace
censuses were empty. These observed cleanup results do not claim that an
injected timeout or cancellation was tested. The runner's separate bounded
failure handling is covered by source review, and the first offline-cache failure
also completed with empty censuses.

C2 independently reviewed C1's Linux target cleanup script, SHA256
`58d7a8e8ce851de4f2b7229b583c80a409eedf9f54f4c9c7906ce977204845ba`
(10,689 bytes). Its literal target is
`/var/tmp/ams-terminal-unix-20260919-01/target`. The script requires the exact
three successful phase receipts and original ELF, verifies source hashes/modes
and absent recorded PID namespaces, and uses canonical-path, symlink, mount,
device and per-file identity guards. It unlinks only planned regular files and
removes empty directories, preserving the ELF at its original path. Exclusive
plan/result files prevent replay. The manager ran the exact reviewed command
after validation in `f8c202b0-decf-44e7-aeae-3eee2d7de062`: exit 0, 8.426 seconds,
472/0 stdout/stderr bytes, with both streams complete and untruncated.

The cleanup removed 448 regular files totaling 312,272,155 logical bytes.
Summed regular-file sizes changed from 345,592,787 to 33,320,632 bytes; the sole
remaining target file is the unchanged tested ELF. These are logical file-size
totals, not measured physical blocks freed or the earlier `du` directory total.
The retained `cleanup-unix-plan.json` SHA256 is
`0848d0b9b0b91eb3cd160c3c1cd5c74f78ebf43dd70f75d344c7c337c4685d3c`;
`cleanup-unix-result.json` is
`1c0174a85814025abbd39c5d77c480654801f791a766e5eb8e2b37d6553ca0c9`.

C2 read the complete cleanup command, output and receipts, confirmed all 448
selected paths were absent and independently rehashed the original ELF plus
848 protected private files and 79 evidence files. Those private files include
all 844 frozen source files. The Linux script verified 1,069 private entries and
84 evidence entries unchanged, including directory metadata and the registry
link. The external registry's contents were not traversed or rehashed. Private
state and source remain retained; the Windows target was outside this cleanup
path. Manager Git status stayed at the same source commit with only managed
`AGENTS.md` untracked. No worker executed deletion, and the cleanup was not
replayed. C2's independent accounting and content checks are retained in
`target/terminal-wsl-review/cleanup-result-reviewed.json`.

This qualifies the selected explicit-shell terminal cases on native x86_64
Linux with Ubuntu 24.04.4, WSL2's recorded kernel and Rust 1.97.1. It does not
qualify macOS, BSD, other Unix systems or the declared Rust 1.88 minimum. Unix
viewer integration tests, the channel suite's runtime, doctests, Clippy, node,
GUI/mobile builds, live fleet/IPC behavior and user-selected shell profiles
were not part of this extension. Earlier Windows evidence remains valid within
its own recorded scope.
