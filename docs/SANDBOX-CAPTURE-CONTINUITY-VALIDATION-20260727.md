# Windows Capture Continuity Validation, 2026-07-27

## Verdict

The capture-continuity slice fixes the reproduced Windows source failure.
Before the change, the AMD source selected its hardware encoder but delivered
zero frames after DXGI returned access denied and the desktop screenshot path
repeatedly reused an invalid handle. With this slice, the same two sandbox
hosts completed both directions, both route starts, and both immediate
rewatches.

The recovered AMD-to-NVIDIA direction delivered 6,707 frames across four
30-second phases at an average 55.709 fps. All 6,707 sampled frames were
unique, and there were no timestamp regressions. The AMD source used
`AMDh264Encoder` through Media Foundation. The NVIDIA viewer used NVDEC.

This is a continuity fix, not a proven encoder speedup. In the direction that
already worked before the change, source conversion and encoding time were
effectively unchanged. Delivery rate increased by 5.19 percent and pacing
tails improved in this run, but initial route startup was 15.34 percent
slower. More repetitions are required before assigning those differences to
the code change.

The exact-display GDI fallback was proven separately on both hosts with real
pixel captures. It was not entered during the final bilateral soak because
DXGI opened successfully on that run. The report keeps those two claims
separate.

## Scope

The implementation changes only the Windows video capture path and its
development probes:

1. A stale `HMONITOR` is no longer reused after topology churn. The requested
   output is reacquired by its stable `\\.\DISPLAYn` name. An explicit
   secondary display is never silently replaced with the primary display.
2. A generic invalid desktop capture handle is treated separately from a
   stale monitor handle. The route retries with bounded 250 ms backoff instead
   of spinning at full rate.
3. A reusable exact-display GDI session now opens
   `CreateDCW("DISPLAY", "\\.\DISPLAYn")`. Its display DC, memory DC, and
   top-down DIB remain allocated during the short fallback spell.
4. The fallback reports a healthy capture state only after it obtains and
   encodes a real frame. A failed grab no longer becomes a fabricated black
   frame.
5. The degraded fallback remains bounded. The route retries DXGI after five
   seconds instead of staying permanently on one-shot capture.
6. The motion runner now checks the source immediately after the direction it
   served. A source that exits before its declared lease can no longer make
   desktop content look like valid synthetic motion evidence.

No signaling implementation, signaling payload, session protocol, route
protocol, ICE negotiation, STUN behavior, or TURN behavior changed. The source
diff contains no signaling or MyOwnMesh file. The sandbox used the existing
authenticated media and Files data routes.

## Verified build input

| Field | Value |
| --- | --- |
| Branch | `codex/sandbox-harness-20260726` |
| Media source commit | `5dc85f3608b8e2a08b353e144b5c227a4252199b` |
| Source status at bundle creation | Clean |
| Bundle | `C:\t\ams-sandbox-bundles\capture-continuity-gdi-final-20260727` |
| AllMyStuff | `allmystuff-serve 0.2.49` |
| MyOwnMesh | `myownmesh 0.3.2` |
| Bundle files | 17 |
| Bundle rehash | 17 matched, 0 missing, 0 mismatched |
| Bundle manifest SHA-256 | `F4F96022ED31193F2728E7B260454E861006CFAC5ECBAB6C3DBA2DE05FA99C32` |

The two endpoints were:

| Host | Processor | Graphics used by the video path | Role coverage |
| --- | --- | --- | --- |
| CECWorkstation2 | Intel Core Ultra 9 285K, 24 cores and 24 threads | AMD Radeon RX 9060 XT | AMD Media Foundation encode, OpenH264 decode |
| Ray Workstation (`DESKTOP-S8N3B0M`) | AMD Ryzen 9 3900X, 12 cores and 24 threads | NVIDIA GeForce RTX 3080 | NVIDIA Media Foundation encode, NVDEC decode |

Both hosts reported Windows 11 build 26200. The sandbox stacks had separate
identities, state, sockets, logs, and binaries. Production installs were not
replaced.

## Before and after

The comparable direction is CECWorkstation2 viewing Ray. It used the same
native H.264 protocol, two cycles, a 30-second initial route, and a 30-second
immediate rewatch.

| Metric, four-phase mean | Before | After | Difference |
| --- | ---: | ---: | ---: |
| Delivered frame rate | 53.215 fps | 55.976 fps | +5.19% |
| Initial first frame | 718.295 ms | 828.495 ms | +15.34% |
| Rewatch first frame | 37.456 ms | 39.718 ms | +6.04% |
| Arrival interval p95 | 35.158 ms | 27.304 ms | -22.34% |
| Arrival interval p99 | 52.104 ms | 32.673 ms | -37.29% |
| Source interval p95 | 30.034 ms | 26.780 ms | -10.83% |
| Source interval p99 | 43.286 ms | 28.978 ms | -33.05% |
| Timestamp regressions | 0 | 0 | no change |

The before run delivered 6,388 frames in this direction. The after run
delivered 6,735.

The reverse direction is the corrected failure:

| Metric | Before | After |
| --- | ---: | ---: |
| Completed phases | 0 | 4 |
| Delivered frames | 0 | 6,707 |
| Mean delivered rate | 0 fps | 55.709 fps |
| Unique sampled frames | 0 | 6,707 |
| Timestamp regressions | 0 | 0 |
| Source encoder | AMD hardware encoder opened, then capture failed | AMD Media Foundation hardware |
| Viewer decoder | no frames to decode | NVIDIA NVDEC hardware |

Immediately before the final fix, a focused failed run logged 555 invalid
desktop-handle errors from 18:05:51.899 UTC through 18:06:03.386 UTC. That is
48.32 failed grabs per second for 11.487 seconds, with zero frames delivered.
The final bilateral soak contains no invalid-handle capture loop.

## Bilateral frame pacing

CECWorkstation2 viewed Ray:

| Cycle | Phase | Frames | FPS | First frame | Arrival p95 | Arrival p99 | Arrival max | Source p95 | Source p99 | Source max |
| ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | Initial | 1,687 | 56.093 | 988.019 ms | 27.284 ms | 32.127 ms | 47.174 ms | 26.066 ms | 28.311 ms | 40.556 ms |
| 1 | Rewatch | 1,686 | 56.011 | 43.107 ms | 27.112 ms | 32.516 ms | 42.303 ms | 26.688 ms | 29.088 ms | 41.100 ms |
| 2 | Initial | 1,679 | 55.826 | 668.971 ms | 27.357 ms | 33.279 ms | 50.560 ms | 27.000 ms | 29.233 ms | 40.267 ms |
| 2 | Rewatch | 1,683 | 55.976 | 36.330 ms | 27.461 ms | 32.771 ms | 56.787 ms | 27.367 ms | 29.278 ms | 52.511 ms |

Ray viewed CECWorkstation2:

| Cycle | Phase | Frames | FPS | First frame | Arrival p95 | Arrival p99 | Arrival max | Source p95 | Source p99 | Source max |
| ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | Initial | 1,655 | 55.000 | 415.097 ms | 23.017 ms | 27.291 ms | 33.594 ms | 32.811 ms | 35.522 ms | 84.222 ms |
| 1 | Rewatch | 1,677 | 55.717 | 78.700 ms | 23.006 ms | 27.066 ms | 60.923 ms | 31.522 ms | 35.345 ms | 42.067 ms |
| 2 | Initial | 1,680 | 55.810 | 338.778 ms | 23.137 ms | 27.068 ms | 38.034 ms | 31.666 ms | 35.455 ms | 59.912 ms |
| 2 | Rewatch | 1,695 | 56.309 | 130.020 ms | 22.529 ms | 26.473 ms | 33.848 ms | 21.033 ms | 34.889 ms | 66.156 ms |

All eight phases had zero timestamp regressions.

## Process-local profile

These measurements are local stage durations. They are not glass-to-glass
latency, and values from different processes must not be added and labeled as
network latency.

| Host role | Stage | Samples | Average | p95 | p99 | Maximum |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Ray NVIDIA source | Convert | 6,779 | 4.435 ms | 4.888 ms | 5.069 ms | 6.873 ms |
| Ray NVIDIA source | Encode | 6,779 | 8.413 ms | 10.263 ms | 10.399 ms | 31.413 ms |
| Ray NVIDIA source | Encoder queue | 6,779 | 0.010 ms | 0.012 ms | 0.017 ms | 2.338 ms |
| Ray NVIDIA source | Route queue | 6,775 | 0.020 ms | 0.025 ms | 0.033 ms | 0.174 ms |
| Ray NVIDIA source | Pipe write | 6,775 | 0.033 ms | 0.045 ms | 0.059 ms | 0.578 ms |
| CEC OpenH264 viewer | Decode and convert | 6,755 | 4.962 ms | 6.098 ms | 7.060 ms | 15.332 ms |
| CEC OpenH264 viewer | Viewer queue | 6,735 | 0.297 ms | 0.442 ms | 4.258 ms | 13.913 ms |
| CEC OpenH264 viewer | Viewer IPC write | 6,735 | 3.381 ms | 3.836 ms | 4.544 ms | 6.376 ms |
| CEC AMD source | Convert | 2,706 | 1.961 ms | 2.205 ms | 2.758 ms | 3.853 ms |
| CEC AMD source | Encode | 2,706 | 6.480 ms | 8.575 ms | 9.928 ms | 14.085 ms |
| CEC AMD source | Encoder queue | 2,706 | 0.012 ms | 0.016 ms | 0.019 ms | 2.135 ms |
| CEC AMD source | Route queue | 2,705 | 0.023 ms | 0.029 ms | 0.042 ms | 1.031 ms |
| CEC AMD source | Pipe write | 2,704 | 0.055 ms | 0.121 ms | 0.162 ms | 0.665 ms |
| Ray NVDEC viewer | Decode and convert | 3,946 | 8.259 ms | 11.116 ms | 12.650 ms | 52.511 ms |
| Ray NVDEC viewer | Viewer queue | 3,727 | 5.553 ms | 14.043 ms | 16.156 ms | 19.373 ms |
| Ray NVDEC viewer | Viewer IPC write | 3,727 | 2.627 ms | 3.312 ms | 3.692 ms | 4.775 ms |

The largest selected CEC viewer frame sum was 23.629 ms. The largest selected
Ray viewer frame sum was 111.421 ms, at a route transition. Ray's
`decoder_prepare` maximum was 93.425 ms and its decoder queue maximum was
98.728 ms at the transition. Steady-state p95 values were much lower.

The valid same-direction before and after source profile is nearly identical:

| Ray source stage | Before average | After average |
| --- | ---: | ---: |
| Convert | 4.437 ms | 4.435 ms |
| Encode | 8.410 ms | 8.413 ms |
| Encoder queue | 0.010 ms | 0.010 ms |
| Route queue | 0.020 ms | 0.020 ms |
| Pipe write | 0.034 ms | 0.033 ms |

This is why the pacing improvement is recorded as an observation rather than
claimed as a capture-code speedup.

## Exact-display GDI proof

The same GDI calls used by the product fallback were run directly on both test
hosts. Every requested output returned a 1920 by 1080 frame with non-black
pixels and thousands of distinct RGB colors.

| Host | Display | Pixels | Non-black pixels | Unique RGB colors | Result |
| --- | --- | ---: | ---: | ---: | --- |
| CECWorkstation2 | `\\.\DISPLAY15` | 2,073,600 | 2,061,203 | 165,527 | success |
| CECWorkstation2 | `\\.\DISPLAY16` | 2,073,600 | 2,073,528 | 70,788 | success |
| CECWorkstation2 | `\\.\DISPLAY17` | 2,073,600 | 2,073,529 | 7,360 | success |
| Ray Workstation | `\\.\DISPLAY1` | 2,073,600 | 1,835,395 | 184,171 | success |

The CEC report SHA-256 is
`FBCF6F4D7557D39F647BDEF732DF0826A041F494E36BD9E81F4F4FB5C30AECE9`.
The Ray report SHA-256 is
`AE466D612FE372411B5B3E0359544B634E374B535BA8E6662A6A5557DDB0095C`.

The final bilateral run opened DXGI successfully on both sources. It therefore
proves normal capture and route continuity, not a live transition through the
new GDI fallback. A forced-DXGI-failure hook would be needed for a repeatable
end-to-end fallback test.

## Motion and visual result

Ray's synthetic source remained alive while it served the measured direction,
so its CEC viewer result is valid input. Across 6,735 frames:

- the orange target was present in 6,729 frames and was never split;
- orange p95 horizontal span was 0.125, its expected 240 of 1,920 width;
- the purple target was present in all 6,735 frames;
- every frame containing both targets classified purple as two components;
- purple p95 span was 0.546875;
- purple p95 row-origin spread was 0.425197;
- the maximum green-dominant sample ratio was zero;
- the maximum nearly-black-row ratio was zero.

The purple split remains reproducible and is still a candidate for the
reported trailing or smearing. No subjective release threshold has been set,
so this is a measured anomaly, not a pass or fail verdict.

CEC's synthetic source exited cleanly but early, at 68.940 seconds of a
900-second lease in the main run. A focused rerun reproduced the early close
at 2.676 seconds of a 300-second lease. Retained frames after that close showed
the real CEC desktop and a game at mixed 1920 by 1080 and 1280 by 720
resolutions. Those frames are valid proof of continued video delivery, but
they are not valid synthetic motion evidence.

The old runner accepted a clean early exit. The corrected runner now rejects
that condition and preserves the media report for diagnosis. It was checked
against both recorded states:

- 2.676 seconds of a 300-second lease was rejected;
- 150.002 seconds of a 150-second lease was accepted.

The reason the CEC form closes early is not proven by this slice. Product
video delivery continues after it closes.

## Local verification

The media source commit passed:

```text
cargo test --manifest-path node/Cargo.toml -j 16 --lib
353 passed, 0 failed, 1 ignored

cargo test --manifest-path node/Cargo.toml -j 16 --example video_prod_probe
12 passed, 0 failed

named_gdi_captures_real_primary_pixels
1 passed

cargo clippy --manifest-path node/Cargo.toml --all-targets
field-telemetry configuration, warnings denied: passed
```

Rust formatting passed for the touched Rust files. The touched PowerShell
files parse successfully. `git diff --check` reports no whitespace errors.

## Evidence

| Evidence | SHA-256 |
| --- | --- |
| `C:\t\ams-sandbox-results\pair-capgdi2-soak-0727\sandbox-pair-session.json` | `E0E6E340356FD5E3C75E1B08885B246CE2044B729BCC6A3504AAB0B285E4E73C` |
| `C:\t\ams-sandbox-results\pair-capgdi2-soak-0727\pair-profile-summary.txt` | `61E2235CE66C3F7DBD590ADA6C3B16ED100BF2B2F3130DC7161F70786D39E702` |
| `C:\t\ams-sandbox-results\pair-amdfb-smoke-0727\sandbox-pair-session.json` | `F661AD7BEB243BC858B99DA4079E1258550550A590D086344785FFCD8C2268A0` |
| `C:\t\ams-sandbox-results\pair-amdfb-smoke-0727\pair-profile-summary.txt` | `1F0B0A49B687EFF00B40BBF14E8B2ABFEBBF475561606F70DEE9B6440231AEB3` |
| `C:\t\ams-sandbox-results\gdi-capture-probe-cec-20260727.json` | `FBCF6F4D7557D39F647BDEF732DF0826A041F494E36BD9E81F4F4FB5C30AECE9` |
| `C:\t\ams-sandbox-results\gdi-capture-probe-ray-20260727.json` | `AE466D612FE372411B5B3E0359544B634E374B535BA8E6662A6A5557DDB0095C` |

Expanded endpoint evidence:

```text
C:\t\ams-sandbox-results\.x\capgdi2-soak-0727\first
C:\t\ams-sandbox-results\.x\capgdi2-soak-0727\second
C:\t\ams-sandbox-results\.x\amdfb-smoke-0727\first
C:\t\ams-sandbox-results\.x\amdfb-smoke-0727\second
```

## Remaining limits

This slice does not prove:

- a forced live DXGI-to-GDI-to-DXGI transition;
- the cause of the early CEC motion-window close;
- a subjective smearing acceptance threshold;
- glass-to-glass latency;
- direct versus TURN path selection for these runs;
- AMD hardware decode, which is not implemented in this build;
- HEVC, AV1, packet-loss, or forced route-loss recovery;
- macOS, Linux, or non-Windows capture behavior.

The next measured optimization target is the viewer side, not the encoder
side. Ray's NVDEC route-transition tail and viewer queue are larger than the
source queues, while both hardware encoders already keep their average
convert-plus-encode work below one 60 fps frame budget.
