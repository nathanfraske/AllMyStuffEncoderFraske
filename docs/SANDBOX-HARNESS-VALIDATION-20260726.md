# Sandbox Harness Validation, 2026-07-26 to 2026-07-27

## Verdict

The headless sandbox now completes a real, bidirectional video test between
two disposable AllMyStuff stacks without replacing or restarting either
machine's installed application.

The completed run used:

- AllMyStuff 0.2.49;
- MyOwnMesh 0.3.2;
- an Intel Core Ultra 9 285K system with an AMD Radeon RX 9060 XT;
- an AMD Ryzen 9 3900X system with an NVIDIA GeForce RTX 3080;
- one isolated sandbox identity and state root on each host;
- one short-lived test network;
- native decoded delivery and compressed delivery in both directions.

All four media legs completed. Both temporary screen grants were revoked, both
sandboxes left the test network, both sandbox stacks stopped, both artifact
archives passed their size and SHA-256 checks, and both leased workers removed
their runtime records.

This is not yet a motion-quality or glass-to-glass latency result. The source
desktops were mostly static, each phase lasted three seconds, and the reviewed
moving pattern was not displayed. The run proves isolation, authorization,
capture, H.264 hardware encode on AMD and NVIDIA, NVIDIA hardware decode,
software decode fallback on the AMD host, route rewatch, artifact collection,
and clean recovery.

## Validated build

| Field | Value |
| --- | --- |
| Branch | `codex/sandbox-harness-20260726` |
| Source base | `31d16f461c5a710070791386e2524460558a58ca` |
| Bundle source state | Dirty prototype, fully listed in its manifest |
| Bundle | `C:\t\ams-sandbox-bundles\interbox-dev-worker-share-20260727` |
| AllMyStuff | `allmystuff-serve 0.2.49` |
| MyOwnMesh | `myownmesh 0.3.2` |
| Backend SHA-256 | `CE2BBB556DD692E00C82A74118E439E916CF44518CFCA2D7276904B4CC3B25BE` |
| MyOwnMesh SHA-256 | `E1F6DD92F5A17AF4A94D24824C2A216542765DA1078571367313B442C301CFF0` |
| Sandbox control SHA-256 | `2943659EC9A23A4EBC83A459BD0EF14010EBFDA18D1F1EECAF32FAE3E88FD564` |
| Leased worker SHA-256 | `A0F3FCE5463480671DE160C9CB0BC672AA50941E71FF8B5EA59A7E1E76DD01EA` |

The dirty state is not hidden. `sandbox-bundle.json` records the source commit,
every modified and untracked source path, and the exact size and hash of all 16
files in the bundle.

## Endpoint proof

| Role | CECWorkstation2 | Ray Workstation |
| --- | --- | --- |
| CPU | Intel Core Ultra 9 285K, 24 cores | AMD Ryzen 9 3900X, 12 cores and 24 threads |
| GPU inventory | Intel Graphics and AMD Radeon RX 9060 XT | NVIDIA GeForce RTX 3080 |
| OS | Windows 11 build 26200 | Windows 11 build 26200 |
| Sandbox node | `e7jxljyq...-13996` | `2dz6fdkc...-D6C5E` |
| Selected ICE pair | host to host | host to host |
| Authenticated | yes | yes |
| TURN required | no | no |
| Reported path RTT | 1 ms | 1 ms |

Each exact-peer check rejected any additional identity before media began.
The network ID contained four independent MyOwnMesh-generated components and
was removed during teardown.

The installed process records remained valid through each sandbox operation.
The stop path would have failed if an installed GUI, backend, MyOwnMesh
sidecar, protected listener, executable path, process start time, or PID had
changed.

## Screen authorization

The first media attempt reached route setup and was rejected by the product's
capture authorization gate:

```text
not authorized: capturing this device's screen, camera, or microphone needs owner/fleet or a share
```

That refusal was correct. Fresh sandbox identities are not owners or fleet
members.

The pair controller now creates one mutual, temporary person-share grant after
both exact peer identities are authenticated. Each grant is limited to:

```text
media: display
role: consume
capability: any display on that exact peer
```

It does not grant input, terminal, files, clipboard, sites, camera, or audio.
Cleanup calls `share_stop` on both sandboxes before either leaves the network.
The completed session records both grants as `granted` and reports zero
cleanup errors.

The grant is sent with the existing `ChannelSendTo` application data request
on `CHANNEL_CONTROL`. It is not a signaling message. No signaling, route,
media, SDP, ICE, STUN, TURN, terminal, or Files wire format changed.

## Completed media matrix

`first` is CECWorkstation2. `second` is Ray Workstation.

| Viewer and source | Delivery | Initial first frame | Rewatch first frame | Frames initial | Frames rewatch |
| --- | --- | ---: | ---: | ---: | ---: |
| CEC views Ray | native decoded | 1,045.573 ms | 228.818 ms | 4 | 5 |
| Ray views CEC | native decoded | 1,707.400 ms | 105.007 ms | 4 | 5 |
| CEC views Ray | compressed | 637.636 ms | 117.588 ms | 4 | 7 |
| Ray views CEC | compressed | 223.277 ms | 119.125 ms | 4 | 3 |

Every received frame had a distinct sampled fingerprint and no timestamp
regression. The native checks reported:

| Viewer and source | Maximum green-dominant sample ratio | Maximum nearly-black row ratio |
| --- | ---: | ---: |
| CEC views Ray | 0.000487924 | 0 |
| Ray views CEC | 0.000243962 | 0 |

The probe did not flag green bands or black rows in these static samples. This
does not rule out tearing, smearing, or corruption during sustained motion.

The low reported frame rates, roughly 1.0 to 2.3 fps in these phases, are not a
throughput ceiling. DXGI duplication was observing mostly static desktops and
the NVIDIA source logged static-frame skips. A moving source is required
before using frame rate as a performance result.

## Encoder results

Both sources selected DXGI duplication and a hardware Media Foundation H.264
encoder at 1920 by 1080, 19.9 Mbps, and a requested 60 fps.

| Source host | Confirmed backend | Stage | Samples | Average | p50 | p95 | Maximum |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: |
| CEC | `AMDh264Encoder` | convert | 4 | 2.386 ms | 2.461 ms | 2.624 ms | 2.624 ms |
| CEC | `AMDh264Encoder` | encode | 19 | 9.025 ms | 9.659 ms | 12.485 ms | 12.485 ms |
| Ray | `NVIDIA H.264 Encoder MFT` | convert | 16 | 4.629 ms | 4.491 ms | 5.252 ms | 5.252 ms |
| Ray | `NVIDIA H.264 Encoder MFT` | encode | 31 | 4.508 ms | 4.117 ms | 8.721 ms | 9.522 ms |

Encoder queue waits stayed below 0.02 ms at p95 on both hosts. Outbound pipe
writes stayed below 0.18 ms at p95. In this sample, encoding and conversion
dominated the measured source-side busy time.

## Decoder results

The NVIDIA host selected NVDEC for H.264. The AMD host did not select an AMD or
D3D11VA decoder. It attempted NVDEC, could not load `nvcuda.dll`, and fell back
to OpenH264 software.

| Viewer host | Backend | Stage | Samples | Average | p50 | p95 | Maximum |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: |
| CEC | OpenH264 software | decoder prepare | 9 | 0.276 ms | 0.000 ms | 1.430 ms | 1.430 ms |
| CEC | OpenH264 software | decoder queue | 9 | 0.097 ms | 0.013 ms | 0.643 ms | 0.643 ms |
| CEC | OpenH264 software | decode and convert | 9 | 13.167 ms | 4.874 ms | 28.608 ms | 28.608 ms |
| Ray | NVDEC hardware | decoder prepare | 11 | 124.142 ms | 0.001 ms | 1,365.408 ms | 1,365.408 ms |
| Ray | NVDEC hardware | decoder queue | 11 | 222.587 ms | 0.197 ms | 1,116.780 ms | 1,116.780 ms |
| Ray | NVDEC hardware | decode and convert | 11 | 8.698 ms | 5.760 ms | 28.367 ms | 28.367 ms |

The NVDEC averages are dominated by one cold initialization. On immediate
rewatch, the observed decoder prepare maximum was 0.001 ms, decoder queue
maximum was 0.011 ms, and decode maximum was 6.285 ms. The 1,365.408 ms
prepare and 1,116.780 ms queue waits overlap and must not be added.

This produces two concrete follow-up items:

1. Add or select the reviewed AMD hardware decode path. Automatic decode on
   the Radeon host is currently software.
2. Keep or prewarm the NVIDIA decoder when an immediate rewatch is likely.
   Cold NVDEC initialization caused the largest measured first-frame delay.

## Harness defects found and resolved

### Terminal session exhaustion

CECWorkstation2's installed backend logged:

```text
too many terminal sessions open here (32); close one before opening another
```

Exited terminal sessions remain in the host table until the one-hour idle
reaper runs. Repeated one-shot remote bootstrap calls therefore exhausted the
32-session cap even though their routes had been torn down.

The harness now opens one terminal bootstrap per endpoint. That bootstrap
starts a sealed worker from the stable runtime. Later control, probe, stop,
result, and collection operations use existing Files routes and local sandbox
node control. The worker has a 900-second idle lease, writes a SHA-256-sealed
result for every request, stops after collection, and removes its own process
record.

This avoids the immediate test blocker. The product's terminal host should
still release exited sessions promptly instead of counting them against the
cap for one hour.

### Capture authorization

The first real route was correctly refused because the disposable identities
had no owner, fleet, or share relationship. The pair now uses mutual temporary
display-consume grants and revokes both in cleanup.

### Route and result cleanup

Earlier failed cuts exposed four issues that are now covered:

- a terminal route is unwatched and disconnected on every exit path;
- a setup retry is allowed only before any command is sent;
- a large result is downloaded through Files and checked against a short
  terminal envelope;
- empty pre-media traces are preserved without being misreported as invalid
  profiler output.

Cleanup array handling now also works when there are zero or one errors.

## Evidence

| Evidence | Size | SHA-256 |
| --- | ---: | --- |
| `C:\t\ams-sandbox-results\pair-crossgpu-share1\sandbox-pair-session.json` | 102,469 bytes | `9BA6795B6F6CF24E8747701CD21D7B2C669F7A6AA5B71281A9F30E91D2E7E17C` |
| `C:\t\ams-sandbox-results\pair-crossgpu-share1\pair-profile-summary.txt` | 14,916 bytes | `DD71CB03CF70CFAD59F2CFC2BE3BD75C965735DDCE9C10A52D9DA59E245E63B2` |
| CEC artifact ZIP | 57,073 bytes | `4BBA763F30371B3ED37FCD87727AD29672717B10A70C0427C2F080AF938376A2` |
| Ray artifact ZIP | 57,768 bytes | `25248A8AACAA49197CBEDD0A47BE00B1909089DD1803483816766CECA39077ED` |

After collection, an exact Files read for
`AppData\Local\AllMyStuffSandboxRuntime\sandbox-remote-worker.json` returned
file-not-found on both hosts. No local destination file was created. That is
the independent proof that both workers removed their records after exit.

## Validation limits

The following claims remain unproven:

- motion frame pacing at 30 or 60 fps;
- smearing, tearing, flicker, and damage recovery during motion;
- glass-to-glass latency;
- AMD hardware decode;
- codec switching among H.264, HEVC, and AV1;
- decoder recovery after packet loss or forced route failure;
- GUI WebView transfer, canvas upload, browser paint, and remote input;
- TURN-relayed performance;
- macOS, Linux, ARM, or non-Windows remote orchestration.

The next useful run is a 30-second moving-pattern soak in both directions,
followed by an immediate rewatch. It should preserve separate cold and warm
decoder measurements and score frame order, palette motion, green bands,
black rows, static skips, recovery, and latency drift.
