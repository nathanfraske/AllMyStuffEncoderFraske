# Terminal compatibility fixtures

The source baseline is `ee9cc150f4bdc3fd49a547b563f0381dae3685f5`.
The [baseline receipt](baseline/manifest.json) records complete input identities,
verbatim ranges and independent literal expectations frozen before reading the
extracted implementation. Nested baseline files are data, not Cargo test targets.

`contract_compatibility.rs` has 18 test definitions: 17 in a viewer-only build and
18 with `host`. It compares the original viewer implementation and the new
explicit viewer, plus the real host's viewer queues when enabled. The fixtures
cover serialized field order, message and attachment shape, refusal, binary and
empty chunk framing, whole-chunk eviction at the four-MiB payload limit, eager
queue adoption, watcher-token replacement and close/stop/detach differences.
The explicit viewer's hosting refusal is checked even when `host` is enabled.
A non-Send/non-Sync spawn policy also checks the host handle's auto-traits.

`support/host_compatibility.rs` is included as `host::compatibility` in library
unit tests. Its 30 comparisons use channel-only sessions and a fake child killer.
The shared [test adapter](support/host_harness.rs) supplies identical observations
to original and extracted code. It checks that every call to `open_with` is an
existing-session attach or an already-full session-cap refusal before the native
PTY branch. No command or shell is launched by these comparisons.

The [frozen host receipt](support/frozen_host_manifest.json) describes the old
implementation's exact source prefix, excluding its original test module. Only
the ByteQueues import and two spawn calls are redirected for isolation. The
algorithm bodies, constants, error strings, comments and original control flow
remain frozen. The additional suffix installs the shared test adapter. Formatting
is skipped only for the frozen host and viewer modules.

Channel comparisons cover byte-ring tails, independent resize minima and the
`u16::MAX` sentinel, full/disconnected control queues, mutation after failed
resize delivery, attachment snapshot/live boundaries, duplicate route behavior,
session caps and counters, list metadata, close/stop/detach cleanup, and the
mutation order before a spawn-policy panic. Legacy bridge futures are captured
and manually polled: replay, empty replay, binary output, resize, continuation
after `Exit`, bounded 256-message delivery, broadcast lag and closed-sink behavior
all have explicit expectations.

Four reaper comparisons use a private current-thread Tokio runtime and the
dev-only `test-util` paused clock. They run the original one-hour delay without
wall-clock sleeping or node runtime registration. They check survival just before
the delay and cleanup just after it, reattach suppression, synthetic generation
mismatch and the existing same-generation recycled-ID behavior. The test crosses
the deadline by one millisecond; exact timer-tick equality is not asserted.

`support/lifecycle.rs` belongs to the separately reviewed A2 fixture lane. Its
nine Windows/Unix cases use real, isolated PTYs and explicit shell commands.
Those cases and the retained platform tests require manager-run native validation.

The local Ubuntu 24.04.4 WSL2 extension ran the exact Unix module selections
`host::tests::` and `host::lifecycle::` on one native x86_64 Linux binary built
with Rust 1.97.1. All 13 retained cases and nine lifecycle cases passed, totaling
20 PTY and two pure cases. Separate private PID/mount namespaces, clean private
HOME/TMPDIR and explicit shell commands bounded these runs; final descendant
censuses were empty. The 30 channel comparisons were discovered but not executed
in that extension, and viewer integration tests and doctests were outside its
scope. Other Unix systems and the minimum Rust version remain unvalidated here.

Manager-owned compiler, formatter and runtime results, retained failures and
platform limits are recorded in the [extraction report](../../../docs/reviews/modular-foundation/terminal-library-extraction.md).
Fixture definitions alone do not establish execution on every supported target.
