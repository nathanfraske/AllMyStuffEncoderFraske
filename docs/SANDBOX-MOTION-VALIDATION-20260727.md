# Sandbox Motion Validation, 2026-07-27

## Verdict

The bounded motion slice completed on the isolated CECWorkstation2 and Ray
Workstation sandbox stacks.

CECWorkstation2 successfully viewed Ray's moving 1920 by 1080 pattern for four
30-second phases. The run delivered 6,388 native decoded frames at an average
53.215 fps. Immediate rewatch reduced average route first-frame time from
718.295 ms to 37.456 ms.

The run also found two live issues:

1. CECWorkstation2 used OpenH264 software decode. Automatic decode attempted
   NVDEC, could not load `nvcuda.dll` on the AMD system, and had no AMD or
   D3D11VA hardware path to select.
2. Ray could not view CECWorkstation2. The CEC source selected its AMD hardware
   encoder, then DXGI duplication returned access denied. The screenshot
   fallback reused an invalid display handle through two logged topology
   changes and never recovered.

The motion proxy also found a split purple target in every completed phase.
The orange target stayed a single component at its expected width. This is a
repeatable candidate for the reported smearing, but the current report stores
only maxima. It does not yet show how many frames contained the split target.

No production AllMyStuff process was replaced or restarted. Both leased motion
sources stopped normally, both sandbox stacks stopped, and cleanup reported
zero errors.

## Test slice

The completed slice was:

- one sealed AllMyStuff 0.2.49 and MyOwnMesh 0.3.2 bundle on each host;
- separate sandbox identity, state, sockets, backend, sidecar, logs, and
  profiler on each host;
- one short-lived test network and exact bilateral display-consume grants;
- one full-screen moving checker pattern on each source;
- a 240 by 140 orange rectangle and a 240 by 140 purple rectangle moving in
  opposite directions;
- native H.264 delivery;
- two cycles in each direction;
- one 30-second initial route and one immediate 30-second rewatch per cycle;
- 1 Hz CPU, GPU, and VRAM telemetry on both endpoints;
- up to 100,000 process-local pipeline events per endpoint;
- guarded cleanup even when one direction failed.

Application files, worker requests, results, and profiles traveled through the
existing authenticated Files data route. Video traveled through the normal
authenticated media route. Signaling exchanged only the metadata needed by the
existing product to find the peer and negotiate ICE. The harness did not add a
signaling message or send application or media payload through signaling.

The probe recorded `exact_route_activation_and_media_delivery`. Peer inventory
was deliberately skipped for the probe itself, so this report does not claim a
fresh direct-versus-TURN classification for this run.

## Verified inputs

| Field | Value |
| --- | --- |
| Branch | `codex/sandbox-harness-20260726` |
| Bundle source base | `ae935d9f1202f198bf0b7a084a4e39f57e367b60` |
| Bundle source state | Dirty prototype, fully listed in the sealed manifest |
| Bundle | `C:\t\ams-sandbox-bundles\interbox-motion-dev5-20260727` |
| AllMyStuff | `allmystuff-serve 0.2.49` |
| MyOwnMesh | `myownmesh 0.3.2` |
| Backend SHA-256 | `310888E9256292A9BC2FF78A75C25E0CB96B82C8A9833EDE31E039B6F6E07C16` |
| MyOwnMesh SHA-256 | `E1F6DD92F5A17AF4A94D24824C2A216542765DA1078571367313B442C301CFF0` |
| Video probe SHA-256 | `FB1CEAE2C9D95627B2C54C58E817A76B288375391041D21F3992BC9D3544A64D` |
| Motion source SHA-256 | `4BBF16004CB623D7D2602ACBB18720E51CA96B769C872DB1EBFE64502700FCD6` |
| Pair runner SHA-256 | `13E58C1F49B6AD531F915A963BA5578F17D87F0D1D5B6C4807A4C33E345E1B92` |
| Leased worker SHA-256 | `3B1A4B6430CCDB170A785EE689D0D2FE6A819FFDC9800D431D15953E9E5DAA75` |

The source pattern handshake was checked before media began. Each source
returned after frame 10 while it was still running, with measured elapsed time
of 140 ms on CEC and 157 ms on Ray. At the final status check, each had
advanced to frame 12,360 with matching paint counts and a validated moving
position.

Both sources later stopped without force:

| Source | Final frame | Final paints | Elapsed | Update rate |
| --- | ---: | ---: | ---: | ---: |
| CECWorkstation2 | 14,365 | 14,367 | 227.892 s | 63.034 Hz |
| Ray Workstation | 17,424 | 17,425 | 275.748 s | 63.188 Hz |

The three focused classifier tests passed in a release build:

```text
motion_palette_clean_targets_are_single_components: ok
motion_palette_exposes_split_and_trailing_regions_without_a_policy_threshold: ok
motion_palette_calibrates_the_measured_decode_transform: ok
```

## Completed direction

CECWorkstation2 viewed Ray Workstation.

Ray selected `NVIDIA H.264 Encoder MFT` through Media Foundation at 1920 by
1080, 19.9 Mbps, and a requested 60 fps. CEC requested native automatic decode,
failed to load NVDEC because `nvcuda.dll` was unavailable, and selected
OpenH264 software.

### Frame pacing

| Cycle | Phase | Frames | FPS | First frame | Arrival p50 | Arrival p95 | Arrival p99 | Arrival max | Source p95 | Source p99 | Source max |
| ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | Initial | 1,615 | 53.808 | 805.991 ms | 16.306 ms | 34.478 ms | 50.144 ms | 68.040 ms | 28.256 ms | 40.022 ms | 74.378 ms |
| 1 | Rewatch | 1,583 | 52.751 | 37.292 ms | 16.732 ms | 35.987 ms | 51.946 ms | 73.506 ms | 32.033 ms | 43.766 ms | 71.745 ms |
| 2 | Initial | 1,603 | 53.421 | 630.599 ms | 16.667 ms | 34.474 ms | 50.478 ms | 72.907 ms | 28.622 ms | 43.689 ms | 87.611 ms |
| 2 | Rewatch | 1,587 | 52.879 | 37.619 ms | 16.650 ms | 35.693 ms | 55.848 ms | 73.335 ms | 31.223 ms | 45.667 ms | 61.977 ms |

Across the four phases:

- average delivered rate was 53.215 fps, with a 52.751 to 53.808 fps range;
- average arrival p95 was 35.158 ms;
- average arrival p99 was 52.104 ms;
- the largest arrival interval was 73.506 ms;
- average source p95 was 30.034 ms;
- average source p99 was 43.286 ms;
- the largest source timestamp interval was 87.611 ms;
- average lag-drift slope was 0.292 ms per second;
- average net lag growth was 16.516 ms per phase;
- all four phases had zero timestamp regressions.

The p50 interval stayed close to one requested 60 fps frame interval. The p95
and p99 tails did not. Source timestamps already contained most of that tail,
so the result does not support assigning all pacing variation to the network or
viewer. The process-local trace also cannot measure glass-to-glass latency.

### Process-local stage profile

These are independent process-local measurements. They must not be added
across hosts and called end-to-end latency.

| Host role | Stage | Samples | Average | p50 | p95 | p99 | Maximum |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Ray source | Convert | 6,759 | 4.437 ms | 4.386 ms | 4.893 ms | 5.084 ms | 6.003 ms |
| Ray source | Encode | 6,759 | 8.410 ms | 8.611 ms | 10.292 ms | 10.403 ms | 18.683 ms |
| Ray source | Encoder queue wait | 6,759 | 0.010 ms | 0.009 ms | 0.012 ms | 0.018 ms | 0.573 ms |
| Ray source | Outbound route queue | 6,756 | 0.020 ms | 0.019 ms | 0.025 ms | 0.034 ms | 0.162 ms |
| Ray source | Outbound pipe write | 6,756 | 0.034 ms | 0.032 ms | 0.046 ms | 0.062 ms | 0.529 ms |
| CEC viewer | Decode and convert | 6,737 | 4.930 ms | 4.729 ms | 6.321 ms | 7.152 ms | 41.197 ms |
| CEC viewer | Decoder queue wait | 6,737 | 0.047 ms | 0.016 ms | 0.043 ms | 0.180 ms | 41.193 ms |
| CEC viewer | Viewer queue wait | 6,388 | 1.866 ms | 0.211 ms | 10.764 ms | 15.796 ms | 28.223 ms |
| CEC viewer | Viewer IPC write | 6,388 | 3.343 ms | 3.237 ms | 3.857 ms | 4.935 ms | 11.850 ms |

The selected source-side stage sum was 12.920 ms per frame. Encode accounted
for 65.1 percent, conversion for 34.3 percent, and the measured local output
and queue work for 0.6 percent.

The selected viewer-side stage sum was 10.209 ms per frame. Software decode
accounted for 48.3 percent, viewer IPC write for 32.7 percent, viewer queue wait
for 18.3 percent, and the remaining measured local work for 0.7 percent.

The largest selected source-side frame sum was 23.414 ms. The largest selected
viewer decoder-side frame sum was 45.940 ms. These maxima occurred in local
traces and do not identify network transit time.

### Resource telemetry

The 122 samples per endpoint cover the four completed 30-second phases.
Process CPU uses the harness's Windows convention where 100 percent is one
logical core.

| Host role | Process CPU avg/max | Total CPU avg/max | GPU 3D avg/max | GPU encode avg/max | GPU decode avg/max | VRAM avg/max |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| CEC viewer | 45.66% / 72% | 13.08% / 20% | 2.00% / 2% | 0% / 0% | 0% / 0% | 1,343.2 / 1,401 MB |
| Ray source | 74.11% / 110% | 7.14% / 15% | 16.82% / 19% | 30.45% / 34% | 0% / 0% | 905.9 / 907 MB |

CEC's zero GPU decode counter agrees with the explicit OpenH264 fallback.
Ray's GPU encode counter and backend log agree that hardware encode was active.

## Motion and content result

The source rectangles are each 240 pixels wide on a 1,920-pixel display. Their
expected horizontal span ratio is therefore 0.125.

| Phase | Both targets present | Orange max components | Orange max span | Purple max components | Purple max span | Purple max row-origin spread |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Cycle 1 initial | 1,613 / 1,615 | 1 | 0.125000 | 2 | 0.593750 | 0.472441 |
| Cycle 1 rewatch | 1,583 / 1,583 | 1 | 0.125000 | 2 | 0.593750 | 0.472441 |
| Cycle 2 initial | 1,603 / 1,603 | 1 | 0.125000 | 2 | 0.593750 | 0.472441 |
| Cycle 2 rewatch | 1,586 / 1,587 | 1 | 0.125000 | 2 | 0.585938 | 0.472441 |

Both targets were found in 6,385 of 6,388 frames. The maximum green-dominant
sample ratio was zero, and the maximum nearly-black-row ratio was zero.

The orange result matches the single 240-pixel source rectangle. The purple
maximum covers about 59 percent of the decoded frame and has two disconnected
components. A clean synthetic target produces one component in the unit test,
while the injected split and trailing patterns produce the metrics seen here.

This is evidence of a decoded split or trailing region under the calibrated
palette proxy. It is not yet a production smearing rate. The report stores the
maximum but not the number of affected frames, and it does not retain every
RGBA frame for independent review.

## Reverse-direction blocker

Ray began a native route to CEC. CEC enumerated `AMDh264Encoder` and selected
the AMD Media Foundation hardware encoder at 1920 by 1080, 19.9 Mbps, and a
requested 60 fps.

Capture then failed before one frame reached the viewer:

```text
DXGI duplication unavailable: DuplicateOutput returned access denied
fallback to per-frame screenshots
screen grab failed: the handle is invalid
```

The CEC log contains:

- 599 invalid-handle screen-grab failures over 11.419 seconds;
- an average 52.46 failed grabs per second during that interval;
- four standalone access-denied errors, plus the initial DXGI access-denied
  result;
- two display-topology changes while the failed capture loop was active;
- no recovered capture status and no decoded frame before the probe failed.

The same source failure occurred in the earlier `motion-smoke3` run, which
recorded 552 invalid-handle failures and two topology changes. This is a
reproduced capture-lifecycle defect, not an encoder-selection failure.

## Harness fixes exercised by this run

The first motion prototype used captured stdout and stderr pipes when the
leased worker invoked the bootstrap. The detached GUI source inherited those
handles, which kept the worker response open until the motion lease expired.

The worker now sends bootstrap stdout and stderr directly to its file-backed
worker logs and waits only for the bootstrap process. Both live MotionStart
calls returned while their sources were still running at frame 10. The
five-minute lease was not consumed.

The pair runner now records each direction immediately and supports
`ContinueOnProbeFailure`. The reverse capture failure therefore did not erase
the valid 120-second CEC-views-Ray result. Final status was
`complete_with_test_failures`, and cleanup still completed with zero errors.

Neither fix changes a signaling, route, media, terminal, Files, ICE, STUN, or
TURN wire format.

## Next test and implementation slice

The next slice should stay narrow:

1. Rebuild capture state when DXGI duplication returns access denied or when a
   screenshot monitor handle becomes invalid. Re-enumerate the requested
   monitor after a topology change and stop the old capture object before
   retrying.
2. Add bounded backoff and one explicit recovery transition so a failed capture
   loop cannot run at roughly 52 errors per second.
3. Extend the motion report with per-frame component counts and span
   distributions. Preserve a small bounded set of the worst RGBA frames so the
   palette result can be checked independently.
4. Repeat this exact two-cycle, two-direction, 30-second native protocol.
   Require both directions to finish, zero timestamp regressions, no
   unrecovered capture state, and complete guarded cleanup. Record pacing and
   motion distributions without setting a subjective smear threshold.
5. Use the successful reverse direction to compare Ray NVDEC against CEC
   OpenH264 on the same moving source. That isolates the decoder cost and
   first-frame behavior that this run could not measure.
6. After that baseline is complete, test AMD or D3D11VA hardware decode as a
   separate implementation slice. Then move to codec switching and forced
   route-loss recovery.

No p95, p99, or motion-quality acceptance limit is proposed here. The current
run supplies the first sustained baseline, but there is not yet enough
cross-direction data to set a defensible release threshold.

## Evidence

| Evidence | Size | SHA-256 |
| --- | ---: | --- |
| `C:\t\ams-sandbox-results\pair-motion-soak1\sandbox-pair-session.json` | 195,438 bytes | `FCEE5B92DCADE705ACCDF8F4928B6B7EB0721CAF4D9158E1413C21FDB24F59ED` |
| `C:\t\ams-sandbox-results\pair-motion-soak1\pair-profile-summary.txt` | 11,068 bytes | `24AB109B55519E276C04C8AFFFF49A156AFFE09DB902BA4BCB99A278F38ED739` |
| CEC artifact ZIP | 881,416 bytes | `7C4426A062AAB05016399C67899A58F8967C7FC0DF4EB9D54DA7CF912E8D0FDF` |
| Ray artifact ZIP | 766,454 bytes | `E3C89BA2F2E2006E753B440428B1EB015C613EF16A9E665FA97094819C724C8D` |
| Bundle manifest | 5,172 bytes | `7ED74969C216C14AF59D1C3133F20419A66E1CFDA7D18F79245E9528489CCBD4` |

Expanded endpoint evidence:

```text
C:\t\ams-sandbox-results\CECWorkstation2\motion-soak1\cec-motion-soak1-motion-soak1-aa5c8377-5403-4485-b448-ced81f6d2506
C:\t\ams-sandbox-results\DESKTOP-S8N3B0M\motion-soak1\ray-motion-soak1-motion-soak1-5e389a99-5031-4d33-8b26-0bc771bc0cbb
```

## Limits

This run does not prove:

- a subjective smearing rate or visual acceptance threshold;
- glass-to-glass latency;
- the selected direct or TURN candidate for this specific probe;
- AMD hardware decode;
- Ray NVDEC motion performance, because the reverse source never produced a
  frame;
- HEVC or AV1 behavior;
- packet-loss or route-loss recovery;
- GUI canvas upload, browser paint, or input latency;
- macOS, Linux, ARM, or non-Windows orchestration.
