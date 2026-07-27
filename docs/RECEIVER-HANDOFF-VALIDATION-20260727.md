# Receiver Handoff Validation, 2026-07-27

## Verdict

The receiver handoff slice is implemented and passed the matched motion test on
both available decoder paths.

When a GUI viewer for the same route replaces another viewer, the replacement
now registers before the old watcher is released. This keeps the route-local
decoder and its dependency state alive through the handoff.

Across four rewatch samples per viewer:

| Viewer | Decoder | Cold mean | Handoff mean | Time saved | Reduction | Speedup |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| CECWorkstation2 | OpenH264 software | 49.015 ms | 16.569 ms | 32.447 ms | 66.20% | 2.96x |
| Ray Workstation | NVIDIA NVDEC hardware | 69.067 ms | 17.712 ms | 51.355 ms | 74.36% | 3.90x |

The measured value is rewatch registration to the first decoded frame returned
by the native viewer probe. It is not glass-to-glass latency and does not
include WebView paint.

Steady delivery did not regress. Mean rewatch delivery increased by 0.32
percent on CEC and 0.35 percent on Ray. Every one of the 4,592 rewatch frames
contained both calibrated motion targets. There were no timestamp regressions,
green-dominant samples, or nearly black rows.

## Implemented slice

The product change is deliberately small:

1. Viewer shutdown defers its local `video_unwatch` by 80 ms.
2. A same-route replacement calls the existing `video_watch`.
3. Only after registration succeeds does it release the predecessor token.
4. If no replacement arrives, the timer releases the old token normally.
5. The backend's existing current-token rule prevents a late predecessor
   release from stopping the replacement decoder.

The GUI now has opt-in diagnostics for:

- registration to first native presentation;
- predecessor count at a successful handoff;
- poll, dispatch, ready wait, presentation wait, and presentation busy average,
  p95, and maximum;
- canvas paint busy and paint interval average, p95, and maximum;
- effect start to first canvas paint.

The diagnostics are compiled into this development build but remain opt-in.

The production source files changed by this slice are:

```text
gui/src/tauri.ts
gui/src/ui/Console.svelte
gui/src/video-timing.ts
gui/src/video-watch-handoff.ts
```

`node/src/mesh.rs` received one test only. The production backend behavior was
already make-before-break safe.

The probe and sandbox files changed only to reproduce both watcher orders,
collect timings, and make the synthetic motion source reliable.

## Boundary check

This slice does not change:

- the signaling client or signaling payloads;
- peer control messages or their wire format;
- route negotiation;
- ICE, STUN, or TURN behavior;
- MyOwnMesh;
- media packet formats;
- encoder or decoder bitstreams.

The product change reorders two existing local Tauri IPC calls,
`video_watch` and `video_unwatch`. No application or media data is added to the
signaling plane. The sandbox sent video through the existing authenticated
media route and used exact, temporary bilateral screen grants.

## Verified inputs

| Field | Value |
| --- | --- |
| Branch | `codex/sandbox-harness-20260726` |
| Media implementation commit | `2c58240d4a43c4f5bba91bcb7cc9a9bbd442de0f` |
| Final harness commit | `717b64cedb4b30c3a84ec0fef7f161ee229b266d` |
| Source state at bundle creation | clean |
| AllMyStuff | `allmystuff-serve 0.2.49` |
| MyOwnMesh | `myownmesh 0.3.2` |
| Resolution | 1920 by 1080 |
| Requested cadence | 60 fps |
| Codec and delivery | native H.264 |
| Repetitions | 4 cycles per mode and viewer |
| Phase duration | 5 seconds |

The two viewers were:

| Host | Processor | GPU | Decoder observed |
| --- | --- | --- | --- |
| CECWorkstation2 | Intel Core Ultra 9 285K | AMD Radeon RX 9060 XT | OpenH264 software |
| Ray Workstation (`DESKTOP-S8N3B0M`) | AMD Ryzen 9 3900X | NVIDIA GeForce RTX 3080 | NVIDIA NVDEC hardware |

For the CEC viewer direction, Ray selected `NVIDIA H.264 Encoder MFT`.
For the Ray viewer direction, CEC selected `AMDh264Encoder`. Both encoders used
Media Foundation at 1920 by 1080, 19.9 Mbps, and a requested 60 fps.

The AMD host does not have an AMD or D3D11VA hardware decoder in this build.
Automatic H.264 decode tried NVDEC, could not load `nvcuda.dll`, and fell back
to OpenH264. This slice improves handoff on that existing fallback. It does not
claim AMD hardware decode.

## Test protocol

Each cycle contained:

1. a fresh route watch and a five-second motion sample;
2. one viewer replacement;
3. a second five-second motion sample.

The cold baseline released the old watcher, waited 150 ms, registered a new
watcher, and requested the existing media-route refresh.

The handoff case registered the successor first, then released the predecessor.
It did not request a refresh because the decoder and dependency chain remained
live.

The source pattern contained two 240 by 140 rectangles moving in opposite
directions over a checked background. Source status had to prove that position
and paint counts advanced before a probe could be accepted.

Ray's source completed its full 300.008-second lease at frame 18,904. The
corrected CEC source remained live through both NVIDIA probes and was stopped
explicitly at 359.748 seconds and frame 23,021.

## Rewatch results

### Rewatch to first decoded frame

| Viewer | Mode | Cycle 1 | Cycle 2 | Cycle 3 | Cycle 4 | Mean | Median | Range |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| CEC, OpenH264 | Cold | 38.062 ms | 51.601 ms | 49.707 ms | 56.692 ms | 49.015 ms | 50.654 ms | 38.062 to 56.692 ms |
| CEC, OpenH264 | Handoff | 13.963 ms | 15.600 ms | 19.838 ms | 16.873 ms | 16.569 ms | 16.236 ms | 13.963 to 19.838 ms |
| Ray, NVDEC | Cold | 118.994 ms | 51.359 ms | 59.377 ms | 46.537 ms | 69.067 ms | 55.368 ms | 46.537 to 118.994 ms |
| Ray, NVDEC | Handoff | 25.635 ms | 12.844 ms | 14.401 ms | 17.968 ms | 17.712 ms | 16.185 ms | 12.844 to 25.635 ms |

The four-sample median improved by 67.95 percent on CEC and 70.77 percent on
Ray. Reporting both mean and median keeps Ray's 118.994 ms cold sample visible
instead of hiding it in one aggregate.

Initial route startup was not the target of this change. Its four-sample mean
was 665.196 ms in the CEC cold run and 652.230 ms in the CEC handoff run. On
Ray it was 338.674 ms and 322.063 ms, respectively. Those are separate cold
route starts, not same-route handoffs.

### Steady delivery and pacing

| Viewer | Metric | Cold | Handoff | Difference |
| --- | --- | ---: | ---: | ---: |
| CEC, OpenH264 | Mean FPS | 55.498 | 55.673 | +0.32% |
| CEC, OpenH264 | Mean arrival p95 | 28.283 ms | 26.934 ms | -4.77% |
| CEC, OpenH264 | Mean poll p95 | 14.382 ms | 10.648 ms | -25.96% |
| Ray, NVDEC | Mean FPS | 57.224 | 57.423 | +0.35% |
| Ray, NVDEC | Mean arrival p95 | 20.652 ms | 20.381 ms | -1.31% |
| Ray, NVDEC | Mean poll p95 | 16.292 ms | 13.003 ms | -20.19% |

These pacing differences are observations from four runs. No release threshold
or population-level claim is inferred from them.

## Decoder lifecycle proof

The backend logs independently confirm the intended mechanism.

| Viewer | Mode | Viewer watches | Decoder starts | Successor decoder restarts |
| --- | --- | ---: | ---: | ---: |
| CEC, OpenH264 | Cold | 8 | 8 | 4 |
| CEC, OpenH264 | Handoff | 8 | 4 | 0 |
| Ray, NVDEC | Cold | 8 | 8 | 4 |
| Ray, NVDEC | Handoff | 8 | 4 | 0 |

Each cycle still starts one decoder for its initial route. The handoff removes
only the four redundant decoder starts caused by the four viewer replacements.

The Ray trace provides a stage-level cross-check. Rewatch windows were
segmented by the cumulative NVDEC frame counters logged at each phase:

```text
cold:    295-598, 903-1204, 1506-1807, 2107-2409
handoff: 2711-3003, 3313-3603, 3913-4204, 4515-4805
```

The trace's monotonic timestamps advance continuously across each listed
range and show the 68.4-second gap between the cold and handoff runs.

| Ray rewatch stage | Cold samples | Cold average | Cold p95 | Cold maximum | Handoff samples | Handoff average | Handoff p95 | Handoff maximum |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Decoder prepare | 1,172 | 0.0491 ms | 0.0008 ms | 56.4283 ms | 1,167 | 0.00045 ms | 0.0008 ms | 0.0022 ms |
| Decoder queue wait | 1,176 | 0.1376 ms | 0.0138 ms | 55.7453 ms | 1,167 | 0.0092 ms | 0.0128 ms | 0.0213 ms |
| Decode and convert | 1,172 | 5.5586 ms | 8.2394 ms | 17.7848 ms | 1,167 | 5.3640 ms | 7.9697 ms | 10.4594 ms |
| Viewer queue wait | 1,137 | 1.9646 ms | 10.2534 ms | 16.6701 ms | 1,146 | 0.2834 ms | 0.6193 ms | 7.7197 ms |

Decoder-prepare maximum fell from 56.4283 ms to 0.0022 ms in the rewatch
windows. Decoder-queue maximum fell from 55.7453 ms to 0.0213 ms. The steady
decode-and-convert p95 changed from 8.2394 ms to 7.9697 ms.

These are process-local stage durations. They cannot be added to source stages
or called end-to-end latency.

## Motion and corruption checks

| Viewer and mode | Rewatch frames | Both targets | Missing orange | Missing purple | Timestamp regressions | Max green ratio | Max black-row ratio |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| CEC cold | 1,126 | 1,126 | 0 | 0 | 0 | 0 | 0 |
| CEC handoff | 1,130 | 1,130 | 0 | 0 | 0 | 0 | 0 |
| Ray cold | 1,166 | 1,166 | 0 | 0 | 0 | 0 | 0 |
| Ray handoff | 1,170 | 1,170 | 0 | 0 | 0 | 0 | 0 |

Ray's NVDEC status remained clean. The logs contain no concealed, corrupt,
unsettled, API-error, or queue-overflow count above zero in the measured runs.

This test detects missing calibrated targets, gross green output, black rows,
timestamp reversal, and large pacing stalls. It is not a subjective smear
score.

## Harness finding

The first CEC motion attempt recorded a 600-second lease but the topmost source
form exited normally after 3.589 seconds. A second attempt exited after 2.591
seconds. The form had an Escape-key close handler in addition to the sealed
stop request and lease.

The harness now removes that uncontrolled key path. Only the sealed stop file,
lease expiry, or operating-system window teardown can end the source. After
that change, the same CEC source remained live for 359.748 seconds and stopped
only when the harness sent its sealed stop request.

This proves the corrected behavior. It does not prove which external key or
window event triggered the earlier closes.

One local throwaway file-transfer helper also exited once with Windows status
`0xC0000374` during an earlier staging attempt. Both remote runtimes had started
successfully, and exact runtime records allowed safe recovery. That event did
not occur in either accepted media A/B pair and is tracked as a sandbox
transport defect, not a media result.

## Local verification

The implementation passed:

```text
GUI unit tests: 41 passed, 0 failed
pnpm check: 0 errors, 0 warnings
pnpm build: passed
node library: 354 passed, 0 failed, 1 ignored interactive display test
video_prod_probe: 12 passed, 0 failed
native_make_before_break_preserves_the_decoder_epoch: passed
PowerShell parser checks: passed
git diff --check: passed
```

The final sealed bundle was built from a clean source tree. Both corrected
sandbox instances stopped and collected successfully. Their cleanup error
lists are empty.

## Evidence

| Evidence | Bytes | SHA-256 |
| --- | ---: | --- |
| CEC cold result | 470,087 | `9476D69A3FDDC30D8E3A7165EE7C745E0CBFF4F0801ECA63FE52D66A26965BD5` |
| CEC handoff result | 469,904 | `FF9760E4E207DB93CC2A28D4DC13CA6F51E2DF0B15E337EBED7A4E68224F9E33` |
| Ray cold result | 469,999 | `B82FA165B72BF919AB63A67A0E4CB7FB07D135106562CB9241752141B1FD190F` |
| Ray handoff result | 473,488 | `55FD1BC2E591105BFC8B9DB7749305BB555937ACAD66C26B884CAA2ADE497FCD` |
| Corrected pair session | 12,530 | `62B15F6106CAFAE244D9CC44EB3DE036737377A845EC622C5BA3DB451F18C711` |
| Final bundle manifest | 4,685 | `108DD9CD40B54F1E9225A3820B2C35EDBD52170476FAF96BC85E7C2A1664EE09` |
| Final CEC artifact ZIP | 562,618 | `7B6140ED9C3405247E8C0D449D76D11F65EC1686DCFC80010D16B11E25BA6A0C` |
| Final Ray artifact ZIP | 4,465,335 | `390E7B4AE0C2348B45666ECC54159DCF10324DE1C9B14DC4C5A9BE17A3F7D996` |

Primary result paths:

```text
C:\t\ams-sandbox-results\CECWorkstation2\ab-mcold-cec-0727
C:\t\ams-sandbox-results\CECWorkstation2\ab-mwarm-cec-0727
C:\t\ams-sandbox-results\DESKTOP-S8N3B0M\ab2-mcold-ray-0727
C:\t\ams-sandbox-results\DESKTOP-S8N3B0M\ab2-mwarm-ray-0727
C:\t\ams-sandbox-results\.x\recv-ab2-0727
```

## Remaining validation

The slice is ready for an interactive GUI motion soak. That run should use the
new opt-in first-presentation and first-paint logs to measure decoded frame to
actual WebView paint and should include a static desktop handoff. A static
source may not produce a new frame immediately, so its behavior must be
measured before deciding whether a bounded post-handoff refresh is useful.

The current proof does not cover:

- subjective smear or motion-quality acceptance;
- WebView paint or glass-to-glass latency;
- WAN or TURN behavior;
- AMD hardware decode;
- HEVC or AV1;
- forced packet loss or route loss;
- macOS, Linux, or non-Windows presentation.

Those are separate slices. None is required to accept the measured
make-before-break improvement for continuous native H.264 motion on these two
Windows decoder paths.
