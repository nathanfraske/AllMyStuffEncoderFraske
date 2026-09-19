# Terminal library extraction: compatibility and validation

Source checkpoint, 2026-09-19. The operator authorized one reusable terminal/PTY
package with behavior retained and scoped testing. The frozen input is
`ee9cc150f4bdc3fd49a547b563f0381dae3685f5`, after the completed storage extraction.
This report belongs to C2's original assignment
`manager:f272ab36-6959-4312-bb0f-91cda702f403`; the known task-board projection
failure does not create a replacement assignment.

**Current status:** the production source has independent acceptance and the
new contract tests are awaiting exact peer review and central validation. The
only executed terminal checks recorded here are the four baseline cases below.
No extracted-library compiler, formatter, lint or runtime pass is claimed yet.

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

Manager integration `63b78ded-3ff0-4185-944c-e36a8c520306` succeeded (exit 0,
0.181 s, 1,597 stdout/0 stderr bytes). It mapped baseline `99d7c122` to
`32d3ddc5d7ea03dbdba7805a2f1bf6939014a1e7` and lifecycle `ba5cf08c` to
`fcb03f69a80bb2b5c0822a8e30b94652a58d9693`. The manager verified empty path
diffs to both source commits and a tracked-clean checkout, apart from managed
`AGENTS.md`. This integration is not a compiler or runtime result.

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
version differences remain. No Cargo resolver success is inferred from these
source-level comparisons. Central locked/offline closure checks remain pending.

The final crate manifest is blob `e090ab2640468932e49009c418d9f1b7333ea2d3`,
LF SHA256 `b83fdb2b7f29f6163314f0ed59dc9cdb01889e93e940f0e1d22dc6458bc54c68`.
Serde JSON is dev-only for literal fixtures. Tokio's dev-only `test-util` feature
enables paused-clock checks; pinned Tokio source expands it to the already-used
rt/sync/time features without a new dependency record or production feature.

## Test inventory and isolation

The [fixture guide](../../../crates/allmystuff-terminal/tests/README.md) describes
the frozen oracle and new tests. These are source counts, pending execution:

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
and keep the directory when shutdown cannot be observed. Windows cmd and native
dimension helpers, Unix fixed-path shell/stty behavior and synchronous native
open/close still require central platform execution and an outer run timeout.

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

## Pending validation and limits

Central compiler, formatting, strict lint, default/host library tests, node
default/no-default checks, selected caller tests and resolved dependency closure
evidence remain pending. New fixture source acceptance and final include bytes
will be recorded before those results are used to close this task. No final
cleanup result is claimed at this checkpoint.

Windows evidence must remain separate from Unix/macOS/mobile execution. A
feature-disabled compile does not prove mobile signing, packaging or runtime
behavior. Isolated shells and channel models do not test live fleet permissions,
GUI tab interactions, service/session-agent launches or every user shell/profile.
Source preservation of those boundaries is recorded above; platform or live-state
coverage will only be claimed for actual reviewed runs.
