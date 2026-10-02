This recipe qualifies the portable `allmystuff-mesh-control` session boundary and
one privately launched daemon. Native builds, setup and execution belong to the
manager. The application continues to select MyOwnMesh v0.3.21; the candidate
adapter is explicitly selected only by this isolated test. H264 and Opus remain
codecs. Event capability C authorizes daemon resources, not approved application
identity, user consent or ledger access.

The extraction base is AllMyStuff `32320720f2b285289067e469226aebe86893b18a`.
Use the final assembled application commit for every command and receipt below.
Reviewed daemon sources are released v0.3.21 at
`59143cbdb094b6b99464c224a500e148fd7d2113` and the draft PR135 candidate at
`db7818e09fedd98899490347b86ac9bc9f97b59b`. A matching version string proves
neither binary provenance nor a published candidate release. Supply an
independent release/build receipt mapping the exact binary SHA256 to its source.

| Gate | What it establishes | What it leaves open |
| --- | --- | --- |
| `session_lifecycle` | 17 accepted literal-wire cases: both ACKs, required C/ID, secret-safe diagnostics, channel requests/inbound, stale/foreign generations, EOF, cancellation and idle/full receiver/session cleanup | Actual daemon implementation, peers and media |
| `contract_compatibility` | Separate portable Status/explicit-contract fixtures owned by A2 | Product version policy and daemon startup |
| `daemon_session` through the harness | Four real library event sessions, renewal, stale refusal, awaited local close, one owned daemon restart and stable private identity | Peer delivery, realtime flows, remote decode and device behavior |
| Node consumer checks | Existing product policy and 14 media regressions still compile/run against the extracted boundary | Installed service, GUI/mobile execution and hardware |

The 17-case port changes only the explicit public constructor/import seam.
It retains the full-queue test's completion barrier: all three fake server writes
and flushes finish before the full receiver is dropped. Server EOF must precede
session Drop; registration invalidation and stale refusal remain required.
Every fixture uses a unique owned Unix socket or Windows pipe. No default
endpoint, daemon discovery or process-wide environment mutation is involved.

Use a dedicated short `CARGO_TARGET_DIR` for the single checkout. Do not reuse
targets from another checkout or a negative control. Record native hostname,
OS/distribution/architecture, account, `git rev-parse HEAD`, `cargo --version`
and `rustc -vV` before compilation. Gateway metadata alone is not native target
evidence. The locked dependency graph must already be assembled and reviewed;
use `--offline` only after its exact artifacts are available.

```sh
cargo test -p allmystuff-mesh-control --test session_lifecycle --locked -- --test-threads=1
cargo test -p allmystuff-mesh-control --test contract_compatibility --locked -- --test-threads=1
cargo clippy -p allmystuff-mesh-control --all-targets --locked -- -D warnings
cargo test -p allmystuff-mesh-control --test daemon_session --no-run --message-format=json --locked
```

The last command builds an ignored integration executable; it starts no daemon.
Select the `compiler-artifact.executable` whose target name is `daemon_session`
and `profile.test` is true from its complete JSON output. Hash that exact native
executable; never guess the Cargo hash suffix or invoke an installed app binary.
The actual test name is `isolated_daemon_sessions`, ignored in ordinary runs.
Scoped Rust formatting and Python/Bash syntax checks are central gates too.

Node consumer recipes use the existing node manifest and frozen feature graph:

```sh
cargo test --manifest-path node/Cargo.toml --no-default-features --lib --locked mesh_contract::tests:: -- --test-threads=1
cargo test --manifest-path node/Cargo.toml --no-default-features --lib --locked control_client::tests -- --test-threads=1
cargo check --manifest-path node/Cargo.toml --no-default-features --lib --bin allmystuff-serve --locked
cargo check --manifest-path node/Cargo.toml --lib --bin allmystuff-serve --locked
```

Set `CMAKE_POLICY_VERSION_MINIMUM=3.5` for node checks as in
[ci.yml](../../.github/workflows/ci.yml). Default host checks additionally need
the native capture/audio libraries. They do not launch `allmystuff-serve`.

For an actual daemon, provide a regular checksum-pinned executable and the
checksum-pinned probe executable. Choose an existing, short private parent
owned/protected for the executing account and a new evidence directory outside
it. On Windows, verify a real Python 3.9+ interpreter first; a WindowsApps alias
is not that verification. Use Python 3.9+ on Unix too.

```sh
python3 scripts/ci/mesh-control-isolated.py \
  --daemon "$mesh_daemon" --daemon-sha256 "$mesh_daemon_sha" \
  --probe "$mesh_probe" --probe-sha256 "$mesh_probe_sha" \
  --app-commit "$mesh_app_commit" --contract legacy \
  --private-parent "$mesh_private_parent" --output "$mesh_new_evidence"
```

The manager's cec native preparation observed `/usr/local/bin/myownmesh`
reporting 0.3.21, 11,663,128 bytes, SHA256
`ccbba6d2680d98e3d43ded05df24e79ce45e26c9e1824515c220206db94ebd14`
(durable `8d4f495d`). This is a binary identity observation; its source mapping
still needs release/build evidence. The harness copies supplied executables into
its new owned root and hashes the copies. It never modifies the originals.

Before **every** daemon invocation, including `--version`, the harness supplies
a fresh private `MYOWNMESH_HOME`, explicit config/identity/control endpoint and
child-only `MYOWNMESH_AUTOUPDATE=0`. Both reviewed mains apply pending updates
before CLI parsing; disabling background updates alone would not isolate an old
home. Foreground startup is exactly `myownmesh serve`, with no invented
`--state-dir` or socket flags. See
[released main](https://github.com/mrjeeves/MyOwnMesh/blob/59143cbdb094b6b99464c224a500e148fd7d2113/crates/myownmesh/src/main.rs#L75),
[candidate main](https://github.com/nathanfraske/MyOwnMeshSecurityReview/blob/db7818e09fedd98899490347b86ac9bc9f97b59b/crates/myownmesh/src/main.rs#L80),
[home selection](https://github.com/mrjeeves/MyOwnMesh/blob/59143cbdb094b6b99464c224a500e148fd7d2113/crates/myownmesh-core/src/dirs.rs#L33)
and [released daemon config](https://github.com/mrjeeves/MyOwnMesh/blob/59143cbdb094b6b99464c224a500e148fd7d2113/crates/myownmesh-core/src/config.rs#L381).

Legacy mode uses config v3 and a newly named private network. It explicitly
disables signaling drivers, mDNS, public fallback, STUN/TURN, pinned peers,
automatic peer approval and infrastructure services. It checks successful real
ordinary-channel registration and release three times; no peer is created.
Status and EventsSubscribe/ChannelSubscribe use the same respective connection,
as supported by the
[released control loop](https://github.com/mrjeeves/MyOwnMesh/blob/59143cbdb094b6b99464c224a500e148fd7d2113/crates/myownmesh/src/control.rs#L694)
and [channel dispatch](https://github.com/mrjeeves/MyOwnMesh/blob/59143cbdb094b6b99464c224a500e148fd7d2113/crates/myownmesh/src/control.rs#L1530).

Candidate mode uses config v2, infrastructure-only startup, no networks and an
explicit event capacity. It requires all eleven owner-selected finite u64 grant
dimensions. It has **no grant/history defaults** and does not invent a connector
media profile. The manager selected these exact bounded inputs for this isolated
qualification; these are test ceilings, not product allocations or a guarantee
of startup:

```text
accounted_memory_bytes=67108864,queued_bytes=8388608,socket_or_handle=64,native_transport_object=64,worker_or_task=64,callback_or_scheduled_work=256,storage_bytes=4194304,storage_object=128,relay_or_provider_allocation=64,parsing_or_cpu_work=1000000,opaque_dependency_residual=67108864
event_capacity=32
```

Supply the comma-separated grant as one quoted `--resource-grant` argument and
the selected capacity as `--event-capacity`, adding them to the same command
with `--contract candidate`. A refused budget/startup remains a recorded
failure; there is no silent retry with a larger grant. The schema and separate
connector/profile requirements come from
[candidate serve](https://github.com/nathanfraske/MyOwnMeshSecurityReview/blob/db7818e09fedd98899490347b86ac9bc9f97b59b/crates/myownmesh/src/cli/serve.rs#L323).

Candidate ChannelSubscribe must return exactly `unknown network: <private-name>`.
This is positive evidence that the real event capability passed authentication,
which occurs before network lookup in
[candidate channel dispatch](https://github.com/nathanfraske/MyOwnMeshSecurityReview/blob/db7818e09fedd98899490347b86ac9bc9f97b59b/crates/myownmesh/src/control/dispatch/channel.rs#L35).
It is deliberately not successful channel installation or inbound peer delivery.
[Infrastructure-only startup](https://github.com/nathanfraske/MyOwnMeshSecurityReview/blob/db7818e09fedd98899490347b86ac9bc9f97b59b/crates/myownmesh/src/embedded.rs#L922)
cannot enable node participation. A future connector test requires its own real
ResourceProviderPort grant, semantic network policy, validated profile and
truthful RealtimeAdvert; this harness does not provide that migration.

All library waits share a 120-second lifecycle deadline. The Python controller
bounds version, barrier, probe and child reap waits. Only the child PID it
created is stopped. Unix uses the daemon's reviewed SIGTERM shutdown and requires
exit zero for both owned daemon lifetimes. A timeout records forced termination
and fails that normal-shutdown gate. Windows starts hidden foreground children;
the daemon waits for Ctrl-C rather than CTRL_BREAK, so this harness terminates
only its owned child and records `normal_shutdown:false`. A passing Windows
probe therefore proves local library lifecycle plus forced owned restart, not
graceful daemon shutdown. See
[released serve](https://github.com/mrjeeves/MyOwnMesh/blob/59143cbdb094b6b99464c224a500e148fd7d2113/crates/myownmesh/src/cli/serve.rs#L11)
and [candidate serve](https://github.com/nathanfraske/MyOwnMeshSecurityReview/blob/db7818e09fedd98899490347b86ac9bc9f97b59b/crates/myownmesh/src/cli/serve.rs#L18).

`EventSession.close().await` proves local reader/socket release. The released
idle server event loop notices a broken writer on future traffic; this probe
does not assert prompt server-side unregister during complete idleness. The
owned process restart releases its remaining server state. See the
[released event loop](https://github.com/mrjeeves/MyOwnMesh/blob/59143cbdb094b6b99464c224a500e148fd7d2113/crates/myownmesh/src/control.rs#L2280).
Same-account IPC is a separate supported mode. This does not solve the
candidate's unsupported cross-account service/approved-app policy, weaken its
current-user DACL, or identify the cause of an installed EPERM report.

The harness retains complete separate stdout/stderr files, exact byte counts and
SHA256 hashes, native facts, input provenance, selected grant/config, child
commands/PIDs/exits, probe result, stop modes and cleanup outcome. It recursively
removes only its nonce-marked absolute root after all children are reaped and
custody checks pass. Failed custody/reap/cleanup preserves that root and returns
failure. Transfer evidence files, never private identity/state trees. Inspect
any ambiguous remote receipt before another start; a timeout is not permission
to replay a possibly completed command.

Native Linux setup uses the narrow `mesh-control-setup-linux.sh` recipe. Its
library mode needs Rust plus linker tools, not the GUI/media package set:

```sh
bash scripts/ci/mesh-control-setup-linux.sh library "$mesh_task_tools"
export CARGO_HOME="$mesh_task_tools/cargo"
export RUSTUP_HOME="$mesh_task_tools/rustup"
export PATH="$CARGO_HOME/bin:$PATH"
```

Run setup centrally as the intended account with root or noninteractive sudo
available. It uses existing Rust/tool recipes from
[bootstrap.sh](../bootstrap.sh), the node media list from
[ci.yml](../../.github/workflows/ci.yml), and the Ubuntu 22.04 host PipeWire PPA
from [release.yml](../../.github/workflows/release.yml). `captureless` adds
CMake; `host` adds the actual media/bindgen prerequisites. The observed Ubuntu
22.04 CMake candidate is `cmake=3.22.1-1ubuntu1.22.04.2`. For another distribution,
first read its native `/etc/os-release` and `apt-cache policy cmake`, then pass
the confirmed exact `cmake=VERSION` third argument. Do not apply the 22.04
version to an advertised 26.04 runner. The host 22.04 PPA step requires existing
`add-apt-repository`; the library mode adds no PPA. Setup is a separately retained
durable prerequisite, followed by tool verification and build, not a worker run.

Windows discovery precedes provisioning: inspect the executing token, winget,
`USERPROFILE\.cargo\bin`, normal Git paths, VS Installer `vswhere.exe` and the
BuildTools `Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin` path.
The existing narrow Rust/linker commands in [bootstrap.ps1](../bootstrap.ps1)
are `winget install --id Rustlang.Rustup --silent --accept-source-agreements
--accept-package-agreements` and the BuildTools command with
`Microsoft.VisualStudio.Workload.VCTools` plus
`Microsoft.VisualStudio.Component.Windows11SDK.22621` and `--includeRecommended`.
Use those exact reviewed commands centrally only after discovery, then refresh
the child PATH and verify real tool execution. Full bootstrap also installs GUI
tools; those are not portable-library prerequisites.

For source acquisition when Git is absent, the primary Microsoft winget catalog
explicitly provides `Git.Git` 2.55.0.5 at
[catalog commit dad4f6b6](https://github.com/microsoft/winget-pkgs/blob/dad4f6b66830099e8e70fd831d50414cd9e68d09/manifests/g/Git/Git/2.55.0.5/Git.Git.installer.yaml).
Its x64 installer SHA256 is
`D065A4E23C3D9A6B5073D609B5BE0830227EC3CA053C083BA385061DDFAF94C6`.
First verify the actual native catalog with `winget show --id Git.Git --exact
--version 2.55.0.5 --source winget`; retain that receipt before a central
`winget install --id Git.Git --exact --version 2.55.0.5 --source winget --silent
--accept-source-agreements --accept-package-agreements`. No installer was run
while preparing this recipe. Python provisioning requires its own verified
runtime/catalog evidence; WindowsApps stubs do not satisfy the actual harness.

For gateway AMST execution, `mesh-control-amst.py` accepts the already verified
argv rather than discovering peers or constructing flags. Primary attach requires
a local raw-mode terminal even with `--run`. The helper sets its owned PTY to
40 rows by 120 columns **before** exec, retains every merged PTY byte through
EOF, records child exit status and bounds cleanup of only its own process group.
It requires `waitid`/`WNOWAIT` and keeps its leader unreaped until every possible
group signal and PTY close has finished, followed by one final reap.
The manager must assert that the selected peer matches the requested target
before invoking this wrapper and retain that selection evidence.

```sh
MYOWNMESH_HOME=/home/cecadmin/.myownmesh python3 scripts/ci/mesh-control-amst.py \
  --output "$mesh_gateway_evidence" --timeout 1800 --windows-run -- \
  /usr/sbin/runuser -u cecadmin -- /usr/bin/env \
  MYOWNMESH_HOME=/home/cecadmin/.myownmesh TERM=xterm \
  /usr/local/bin/amst DESKTOP-BISE755 --run "$mesh_one_line_powershell"
```

The final Windows argument must be one physical PowerShell line; the helper
rejects embedded CR/LF. ATC-GPU-WS uses the same verified argv with that exact
peer name and without `--windows-run`. Put actual nested OS/account/hostname,
toolchain and application SHA output in the selected native command and drain
the full EOF. The gateway receipt describes the gateway, not the nested host.
Use manager-owned whole-file transfers under the granted root for source and
evidence; retain each transfer ID and terminal checksum receipt.

Current preflight limits are not qualification passes: corrected BISE
`3d8a919e-b8ca-4ba6-9755-26e29a99807a` reached actual Windows/AMD64/Admin and
full EOF; subsequent discovery found tools concretely absent. ATC `72c9dd35`
reached actual native Linux/x86_64/UID1000 with git/cc/Python but no Rust/CMake/make.
Mac fleet probes returned no response before command execution, so Intel CI
library testing remains a separate fallback, not Mac app/device reachability.
The original BISE hub `outcome_unknown` remains retained even though its target
receipt later proved a rendering timeout. The changed sized-PTY probe establishes
new facts without rewriting that original outcome. Final native test receipts
must name the actual source, machine, executable, full streams and gate counts.
