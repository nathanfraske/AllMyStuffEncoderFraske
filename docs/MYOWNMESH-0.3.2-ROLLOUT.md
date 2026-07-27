# MyOwnMesh v0.3.2 coordinated rollout

## Scope

AllMyStuff pins the desktop daemon and mobile embedded engine to MyOwnMesh
v0.3.2. The tag resolves to commit
`28c9e27f89fdb8c2af9a9691a0fe0271befbe060`.

This repository change does not install, start, stop, or restart MyOwnMesh on
any endpoint. It does not change AllMyStuff signaling code, media messages, or
route-control semantics. Application data remains on authenticated,
established ICE data channels. STUN is used for traversal and TURN is used when
a relay is needed. No application data is added to the signaling path.

At runtime, AllMyStuff reports a sidecar version mismatch but does not update
the installed binary by default. `ALLMYSTUFF_ALLOW_MYOWNMESH_UPDATE=1` is the
only supported opt-in for the updater. Use it only while coordinating every
communicating endpoint. A binary selected through `MYOWNMESH_BIN` and a
development build artifact are never updated by AllMyStuff.

## Why the rollout must be coordinated

MyOwnMesh v0.3.2 adds the negotiated DTLS certificate fingerprint to the
Ed25519-signed authentication transcript. MyOwnMesh v0.3.1 signs different
bytes and has no handshake version negotiation for this change. A v0.3.1 peer
and a v0.3.2 peer cannot authenticate each other.

Every communicating endpoint must move to v0.3.2 in the same maintenance
window. A mixed v0.3.1 and v0.3.2 fleet is expected to lose mesh connectivity.

## Security changes verified at the tag

- Commit `7d18841e1b668cf26a78834d5a2d23d6be6c75fa` binds the signed Ed25519
  handshake to the DTLS certificate fingerprint used by the connection.
- Commit `ea7adc8ce8d82029f5f9d38c4090c949954e5228` blocks application, RPC,
  reliable, and governance traffic until the peer is authenticated.
- Commit `2053871363697326b434c746ae4a3590694aa6b4` fixes eviction and role
  convergence behavior.

The AllMyStuff-facing IPC wire types, route handle shape, channel identifiers,
and reliable-delivery response shape are unchanged from v0.3.1 to v0.3.2.

## Release archive checksums

These values match the official `.sha256` assets published with MyOwnMesh
v0.3.2. The same values are machine-checked from
`.myownmesh-release-sha256`.

| Platform | Archive SHA-256 |
| --- | --- |
| linux-x86_64 | `3a8e57dd0b707714df04e10407680b588a8639ecc95bcb75c393d867139affd4` |
| linux-aarch64 | `afcc1ee620e46c4c161c4365ccbed6c3134603d6033894d5449a2431bdbcc2aa` |
| linux-aarch64-musl | `68374433c555229da2a8a320e243f0da5f46514a776ed3243253d3e591ca0004` |
| linux-riscv64 | `20aad34f1209f7597605a4d556e5461cd80bfadf6e6fed2ecd7bacf6591222e6` |
| macos-x86_64 | `785f2061418ef8973d6180434250b5457356e06de2488f8ed8e128b321846870` |
| macos-aarch64 | `b6ad194df9c51aedf247596c4dc279c7b905aa097f426afa8f6f5f3c83e4f336` |
| windows-x86_64 | `d1be7512a94d19727391905c73585e13a4bf26692c5dd2bc517a7e5ca87da593` |

The Windows archive was also downloaded and checked locally. Its extracted
`myownmesh.exe` reports `myownmesh 0.3.2` and has SHA-256
`e1f6dd92f5a17af4a94d24824c2a216542765da1078571367313b442c301cff0`.
The extracted binary hash is local verification evidence, not a replacement
for the official archive checksum.

## Staged rollout

1. Keep the running v0.3.1 processes in place while v0.3.2 packages are copied
   to every endpoint.
2. On every endpoint, verify the package against the platform checksum above.
   Verify the extracted daemon reports exactly `myownmesh 0.3.2`.
3. Confirm that every remote-only endpoint has an out-of-band recovery path.
   Do not restart a remote-only endpoint first when no recovery path exists.
4. Restart all endpoints within the same maintenance window.
5. Confirm both ends report v0.3.2, an authenticated active peer, and a
   selected ICE pair before testing AllMyStuff routes.
6. Run a bounded video, input, and route-relink smoke test in both directions.

Rollback must also be coordinated. Restore the previously verified v0.3.1
package on every endpoint before restarting them. Leaving part of the fleet on
v0.3.2 is not a valid rollback state.

## Build and packaging gates

- `scripts/check-myownmesh-release.ps1` verifies the tag, commit, complete
  platform matrix, and checksum format. It can also hash a supplied archive.
- The desktop build verifies the selected release archive before extraction.
- The cache signature includes tag, commit, platform, and archive hash.
- A failed acquisition truncates the sidecar slot and removes its cache
  sentinel, so older bytes cannot survive a pin change.
- Release CI requires a valid sidecar. Development and compile-only CI may
  still use the documented zero-byte placeholder and runtime fallback.

## Validation boundary

The v0.3.2 tag and Windows release archive were inspected locally. MyOwnMesh's
published Windows, Linux, macOS, ARM64, and RISC-V CI jobs passed at the tag.
The non-Windows archives have not been executed locally.

On July 23, 2026, the official Windows v0.3.2 binary was installed and
restarted on the local test host and the authorized across-town Windows host.
Each live daemon reported version `0.3.2` through its control socket. The two
hosts established authenticated direct ICE paths with no TURN relay. The
observed RTT range was 19 to 27 milliseconds. This proves matching-version
Windows connectivity only. Video and input regression results are recorded
separately after their tests finish.

An earlier observation appeared to show a v0.3.1 process communicating with a
v0.3.2 process. That observation checked the binary on disk rather than the
version returned by the live daemon. Source inspection proved the releases
sign different authentication transcripts and have no compatibility fallback.
The observation is not accepted as mixed-version evidence.
