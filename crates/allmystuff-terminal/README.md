# allmystuff-terminal

Shared PTY sessions and terminal viewer queues, extracted from the node. This
package hosts shells and buffers bytes; the terminal emulator and the
`allmystuff-term` command-line client remain separate consumers.

The default feature set is empty. `viewer::TerminalHost` keeps the functional
per-route byte queues and returns the original `this device cannot host a
terminal` error from hosting requests. It is available even when the `host`
feature is enabled, so an application's capture-less adapter can select it
explicitly. Root reexports select `viewer` by default and `host` with that feature.

```rust
use allmystuff_terminal::viewer::TerminalHost;

let terminal = TerminalHost::new();
terminal.ensure_queue("route");
assert!(terminal.enqueue("route", b"hello".to_vec()));
let token = terminal.watch_output("route");
assert_eq!(terminal.poll("route"), b"\x05\x00\x00\x00hello");
terminal.unwatch("route", token);
```

Enable `host` to use `host::TerminalHost<S>`, where `S` implements `TaskSpawner`.
Its generic `spawn` method receives a `Send + 'static` future with unit output
and returns its Tokio task handle. A caller can dispatch through its own stored
runtime handle. Construction never invokes this policy; it is called at the
existing idle-reaper and broadcast-to-mpsc bridge sites, and the returned task
handle is immediately dropped as before. The library owns no runtime registry.

The host keeps the existing shared-session model: routes attach to one shell,
receive a scrollback snapshot followed by broadcast output, share input and
reconcile their requested dimensions. The source's queue limits, control-send
outcomes, shell fallback order and error text are unchanged. In particular:

- A new attacher inherits the session's current size until its first resize.
  Reconciliation attempts control delivery even at an unchanged size; a failed
  control send still records and broadcasts a changed size.
- `detach` keeps a shell alive and arms the original one-hour idle task after
  the last attacher leaves. The stored generation remains the original value;
  extraction does not add a generation update or a stronger cancellation rule.
- `close` kills a session and removes its route mappings while leaving viewer
  queues. `stop` additionally removes only the requested route's viewer queue.
  Host `detach` removes that queue; viewer-only `detach` remains a no-op.
- The legacy mpsc bridge forwards `Exit` and keeps receiving until the channel
  closes or a send fails. Its bounded sends and broadcast-lag handling remain.
- The three blocking PTY workers retain their original ordering and lifetime,
  including the 120 ms exit linger and absence of joins or new drop cleanup.

Hosting uses the existing `xpty` 0.3.6 dependency under the `portable-pty` alias,
with its original default features. Shell candidates are constructed before
attach/cap checks. They retain `ALLMYSTUFF_USER_HOME` with `dirs::home_dir`
fallback, `TERM=xterm-256color`, and `COLORTERM=truecolor`. The moved tracing
events retain the `allmystuff_node::terminal` target, levels and message fields.

Node's existing `terminal` paths remain adapters. Its real host specializes the
spawn policy to the unchanged `crate::spawn`, retaining first-registration-wins
runtime selection and missing-runtime panic at the original call sites. Its
host-disabled adapter explicitly selects the functional viewer implementation.
Routing, capability advertisement, sender authorization, local IPC, media
framing, sequence checks and application supervision stay in node.

The source and independent fixtures are under review. Compiler, lint and
isolated PTY results remain pending central validation; source comparison alone
does not establish runtime or platform coverage.
