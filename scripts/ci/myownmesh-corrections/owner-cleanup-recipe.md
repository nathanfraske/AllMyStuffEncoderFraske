# Exact Events-owner cleanup correction

This is a source candidate against MyOwnMesh
`db7818e09fedd98899490347b86ac9bc9f97b59b`, tree
`5492d1845e15dbc067acec3ec592bb9500b9e21d`. Native validation is pending.
`owner-cleanup-fix.json` records the original and resulting blob/SHA256 for
each of the three production files. The separate C2 regression patch supplies
`crates/myownmesh/src/control/owner_cleanup_regression.rs`; apply both before
compiling tests. No dependency, lockfile, feature, authorization, codec, pacing,
endpoint, resource-budget, or installed-runtime change is included.

The corrected lifetime is the exact Events registration. Its already-admitted
reader observes departure while either the subscription ACK or event writer
is blocked. Events is a one-way mode: EOF, a read error, or an unsolicited
client byte ends it, matching the existing inbound-realtime-pipe rule. Normal
release still awaits the existing cleanup path. An error, panic, or cancelled
connection task uses an exact-owner guard and the existing synchronous native
flow Drop and route-join custody paths. This fallback does not claim an awaited
native-retirement result.

Registry identity checks and removal share the tables lock. A stale or foreign
handle cannot remove a current registration just because its numeric id matches.
Pending opens, closes, and their response writes observe the authenticated Events
owner's cancellation. Flow install/disconnect serialization and take-before-close
remain unchanged. A successful flow-open write is committed only for a current
owner; revocation on either side of that check owns the same exact flow drain.

`Wrote::Sent` establishes a completed server write/flush, not client receipt. An
unknown open ACK outcome requires retiring that Events owner, or closing an exact
F that was actually learned. A lost close ACK does not restore F: a second close
still returns unknown capability. There are no guessed-coordinate/label retries,
tombstones, implicit idempotence, timers, detached cleanup tasks, or new budgets.

Only the manager executes the following commands, from a fresh private exact-db
checkout with the two reviewed patches applied. Verify the base and resulting
blobs first; retain the patch identities, source/lock hashes, toolchain/platform,
complete stdout/stderr and actual test names/counts. Use Rust 1.88.0 and a fresh
target directory dedicated to that checkout. The existing reviewed upstream
prerequisite recipe and native toolchain remain the prerequisites; this packet
does not install tools. Each test command needs a finite outer durable deadline
(300 seconds once compiled); build/lint gates need the manager's bounded build
deadline. Do not reuse the failing-base artifacts as corrected binaries.

```text
git apply --check <absolute-owner-cleanup-fix.patch>
git apply <absolute-owner-cleanup-fix.patch>
git apply --check <absolute-C2-owner-cleanup-regression.patch>
git apply <absolute-C2-owner-cleanup-regression.patch>

rustup run 1.88.0 rustfmt --check --edition 2021 --config skip_children=true crates/myownmesh/src/control.rs crates/myownmesh/src/control/handoff.rs crates/myownmesh/src/ipc/clients/registry.rs crates/myownmesh/src/control/owner_cleanup_regression.rs
cargo +1.88.0 test -p myownmesh --bin myownmesh --no-default-features --locked --offline control::owner_cleanup_regression:: -- --test-threads=1
cargo +1.88.0 test -p myownmesh --bin myownmesh --no-default-features --locked --offline control::handoff::tests:: -- --test-threads=1
cargo +1.88.0 clippy -p myownmesh --all-targets --no-default-features --locked --offline -- -D warnings
cargo +1.88.0 build -p myownmesh --bin myownmesh --release --no-default-features --locked --offline
```

The new module's names/count are frozen by C2's regression receipt; the retained
handoff filter has three definitions. They exercise production tables/guards
using already-retired handles and cannot prove native flow retirement. The
daemon package is a binary target, so these commands deliberately use
`--bin myownmesh`.

The real quiet-EOF gate uses C2's corrected raw probe/controller with explicit
`--expect revoked`, the newly built checksum-pinned corrected daemon, a freshly
selected Cargo-JSON probe executable, and a fresh private parent/evidence output.
Require B2's corrected-build receipt and its full SHA256 through
`--candidate-build-receipt` and `--candidate-build-receipt-sha256`; retain the
stock-daemon negative control as a separate source-qualified observation.
Keep the existing explicit grant, event capacity and 2,000-ms quiet observation.
Its original failing-base control remains preserved. Foreign-owner/successor
survival must pass alongside revocation. This zero-FlowOpen gate proves only
owner authority revocation. Actual native-flow retirement, first-unit AV delivery,
and the relevant platform outcomes require the separately owned native paired
gates. Keep successful-server-write/lost-client-receipt and forced Windows process
teardown qualifications explicit in the resulting evidence.
