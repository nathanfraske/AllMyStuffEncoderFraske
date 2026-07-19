# The video pipeline — how to interact with it

_Fork-internal. This is the single operational reference for working on
this fork's encoders, decoders, and video path: the shape of the
pipeline, how a codec/rung/posture is chosen, every operator dial, the
three seams you extend, and the log lines you grep. It is **not** part of
the upstream PR (see [`README.md`](README.md) for why)._

## The one rule that makes all of this safe

**Everything the pipeline needs lives behind the node backend
(`node/src/**`).** Tuning encoders, adding a codec, adding a feedback
signal, or adding an experimental feature must never touch the shared
crates (`crates/allmystuff-protocol`, `-session`, `-mobile-core`) or the
GUI. Those are Chris's surface; he develops there freely while we work
here. The boundary is held by three design choices, each detailed below:

1. **The codec is carried key-to-key, not on the wire** — the decoder
   *sniffs* it, so adding AV1 changes no message type.
2. **New tuning signals ride an opaque `ext` bag**, not new typed
   protocol fields — so a new feedback metric or tune knob is a
   backend-only struct change.
3. **Experimental features gate on `labs.rs`**, whose one runtime toggle
   already exists in the GUI — so no feature ever needs a new control.

If a pipeline change seems to require editing a `crates/` file or a
Svelte component, stop: it almost certainly belongs behind one of those
three seams instead. The only wire touchpoints that already exist are the
opaque `ext` field on `RouteControl::VideoFeedback`/`Tune` (commit
`8e1308c`) and the `labs_set` control op — both backend-owned.

## Shape of the path

```
 HOST                                                     VIEWER
 ────                                                     ──────
 capture ─▶ (BGRA→NV12) ─▶ ENCODE ─▶ pace/split ─▶┐   ┌▶ depace ─▶ SNIFF ─▶ DECODE ─▶ NV12→RGBA ─▶ present
 DXGI dup     D3D11 VP     ladder    link-fit     │   │  reorder    codec     ladder    AVX2+NT SIMD   (GUI)
 or GDI       (zero-copy)  §Encode   §Pacer       │   │  ring       §Sniff    §Decode
                                                  └───┘
                                       CHANNEL_MEDIA / CHANNEL_CONTROL
                                       = the ICE-negotiated STUN/TURN datapath
                                         (NOT signaling — see §Boundary)

           closed loop:  viewer measures arrival ──ext──▶ host AIMD reconfigures bitrate  §Loop
```

Capture and the BGRA→NV12 conversion feed a GPU texture straight into the
encoder only when the experimental zero-copy lane is explicitly enabled
(`ALLMYSTUFF_GPU_LANE=1`); production defaults to CPU-DXGI capture feeding
the vendor hardware encoder through a CPU NV12 buffer. Studio·Lossless stays
selectable but fails soft to lossy Studio until the fork replaces the current
H.264-only media-track framing with a gated HEVC data-plane contract.
Everything from ENCODE rightward is what this guide covers.

## Encode — the ladder and how a rung is picked

`video.rs run_gpu_lane` opens the best encoder available on the host's
GPU, in this order, falling to the next on failure:

| Rung | Module | When | Notes |
|---|---|---|---|
| **NVENC** | `nvenc.rs` | NVIDIA GPU | H.264 + HEVC; the only rung with **lossless** (HEVC constQP-0, transquant bypass). Presets measured-set: game P2, lossless P3, studio P5. P6/P7 banned (hidden 16-frame lookahead). |
| **AMF** | `amf.rs` | AMD GPU (RDNA) | AVC today (`GpuCodec::Amf` arm); vendor-gated (refuses non-`0x1002`). NVENC-parity levers: GDR game mode, in-place bitrate, presets. **No lossless** (AMF has no transquant bypass). |
| **MF** | `mediafoundation.rs` | any GPU MFT | vendor-neutral hardware encode; the AMD host used this before the AMF rung. Deeper pipelining → ring retirement depth 4. |
| **software** | `openh264` | no GPU encode | the floor. |

The rung is selected by capability, then the **posture** sets its knobs:

| Posture | Intent | Encoder config |
|---|---|---|
| **Balanced** | default remote desktop | CBR-ish, moderate preset |
| **Game** | WAN gaming, lowest latency | **GDR intra-refresh** (no full IDR stalls); the only posture with closed-loop bitrate on by default |
| **Studio** | high-fidelity creative | higher preset, higher bitrate |
| **Studio·Lossless** | byte-exact | NVENC HEVC constQP-0 → NVDEC/D3D11VA, proven byte-exact both decode rungs |

Posture arrives as `Tune.mode` (a `Posture` enum in the backend, carried
through the `ext`-bearing `RouteControl::Tune`). `ALLMYSTUFF_GAME_MODE`
forces Game locally.

## Decode — the rungs, the ladder, and the sniff

The decoder does **not** learn the codec from the wire. It **sniffs**
each access unit (`video_decode.rs sniff_codec`):

- H.264 / HEVC — Annex-B start codes (`00 00 01`), NAL type disambiguates.
- **AV1 — no start codes.** Falls through to `sniff_av1_obu` (leb128
  OBU-aware, detects a leading sequence-header OBU). This is the
  structural reason adding AV1 needs no wire change — see
  [`AV1-SEAMS.md`](AV1-SEAMS.md).

Sniffed codec → a rung ladder (each pins with an env dial):

| Codec | Ladder | Pin dial |
|---|---|---|
| H.264 | openh264 (sw) — GPU rungs as added | — |
| HEVC | NVDEC (4.24 ms @1440p) → D3D11VA (5.76 ms) → *(no sw floor yet)* | `ALLMYSTUFF_HEVC_DECODER` |
| AV1 | `Av1Rung`: NvdecAv1 → D3d11vaAv1 (both **stubs** today) | `ALLMYSTUFF_AV1_DECODER` |

D3D11VA (`d3d11va.rs`) drives `ID3D11VideoDecoder` on any Windows GPU —
this is what makes lossless Studio decode cross-vendor. Field truth: HEVC
has exactly **one** DXVA slice format (the 10-byte short entry reported as
`ConfigBitstreamRaw=1`); there is no HEVC "long" format in any SDK.

The decoded NV12 → RGBA step is the AVX2 + non-temporal-store SIMD kernel
in `nvdec.rs` (`nv12_to_rgba`, byte-exact vs the scalar reference).

## Pacer and the closed loop

**Pacer** (`mesh.rs send_video_paced`): a frame is split at slice
boundaries (`split_annexb_paced`) and metered onto the datapath by a
**link-fitted drain model** so a keyframe burst doesn't head-of-line
block. AV1 has no slice boundaries → the AU rides as one chunk until the
`obu_split` seam is filled (documented in `split_annexb_paced`).
`ALLMYSTUFF_PACE_DRAIN_MBPS` overrides the fitted drain rate;
`ALLMYSTUFF_PACED_SLices` toggles sub-frame paced slices.

**Closed loop** (`§Encode` ⇄ `§Decode`, entirely on the datapath):

1. Viewer measures per-chunk arrival (`note_video_arrival` — a chunk-train
   bandwidth estimate) plus a one-way-delay trend.
2. It packs those into `video::PipelineFeedback` → `.to_ext()` → the
   opaque `ext` on `RouteControl::VideoFeedback`.
3. Host parses `PipelineFeedback::from_ext`, runs **AIMD** against the
   estimate, and reconfigures NVENC bitrate **in place** (no stream
   restart).

The loop is gated by `ALLMYSTUFF_RATE_ADAPT`: default **GameOnly** (only
the Game posture rides it, because AIMD is unambiguously beneficial only
there); `off`/`0` disables; `all`/`1` enables for every lossy posture.
This is the "reserve auto-changers unless 100% beneficial for the mode"
rule, in code.

## The operator dials

Pipeline-scoped environment dials (claim/pairing and logging dials are
elsewhere). Unset = the shipped default; every dial is fail-soft.

**Selection / codec**
| Dial | Effect |
|---|---|
| `ALLMYSTUFF_NVENC` / `ALLMYSTUFF_AMF` | enable/skip the direct SDK rung (`NVENC=1` is required for normal lossy postures; Studio·Lossless is an explicit exception, while `NVENC=0` disables it too) |
| `ALLMYSTUFF_NVENC_PRESET` | preset override (P1–P5; P6/P7 refused) |
| `ALLMYSTUFF_HEVC_DECODER` / `ALLMYSTUFF_AV1_DECODER` | pin a decode rung (`nvdec`/`d3d11va`) |
| `ALLMYSTUFF_VIDEO_ENCODE_ADAPTER` | choose the encode GPU |
| `ALLMYSTUFF_GPU_LANE` | `1` enables the experimental zero-copy capture→encode lane (including quarantined HEVC experiments); unset/`0` uses CPU-DXGI + hardware H.264 encode |

**Posture / rate**
| Dial | Effect |
|---|---|
| `ALLMYSTUFF_GAME_MODE` | force Game posture (GDR) |
| `ALLMYSTUFF_RATE_ADAPT` | closed-loop bitrate: unset=GameOnly, `off`, `all` |
| `ALLMYSTUFF_VIDEO_BITRATE` / `_FPS` / `_MAX_EDGE` | backend caps (the real ceiling — the GUI sliders must not clamp below these) |
| `ALLMYSTUFF_PACE_DRAIN_MBPS` / `ALLMYSTUFF_PACED_SLICES` | pacer drain rate / sub-frame slices |
| `ALLMYSTUFF_RING_RETIRE` | encoder ring-retirement depth (AMD-tearing A/B) |

**Scheduling honesty** (T3.x)
| Dial | Effect |
|---|---|
| `ALLMYSTUFF_MMCSS` | join the MMCSS "Games"/"Pro Audio" class |
| `ALLMYSTUFF_GPU_SCHED` | raise the D3DKMT GPU scheduling class |
| `ALLMYSTUFF_GPU_HEARTBEAT[_MS/_PX]` | clock-keeper (256×256@2ms) that holds GPU boost flat |

**Experimental** (see §The labs gate)
| Dial | Effect |
|---|---|
| `ALLMYSTUFF_EXPERIMENTAL` | master Labs tier (outer wall) |
| `ALLMYSTUFF_X_<FEATURE>` | per-feature dial inside an on tier |

**Observability**
| Dial | Effect |
|---|---|
| `ALLMYSTUFF_VIDEO_STATS` | per-frame encode stats to the log |
| `ALLMYSTUFF_TELEMETRY[_SECS]` | periodic telemetry rollup |
| `ALLMYSTUFF_HEVC_DUMP` | dump the HEVC bitstream for offline inspection |

## The three seams you extend (never the wire)

**1. A new tuning signal → `PipelineFeedback` / `PipelineHints`.** Add a
field to the backend struct in `video.rs`, pack it in `to_ext()`, read it
in `from_ext()`. It rides the opaque `ext` bag verbatim through
session/protocol — zero churn there. *Never* add a typed field to
`RouteControl`.

**2. A new experimental feature → `labs.rs`.** Add a `Feature` variant, a
`slot()` atomic, an `env_spec()` (`ALLMYSTUFF_X_*`) row, and a `set_feature`
wire-name arm. Then at the call site: `if labs::on(Feature::Foo) { new }
else { /* today's shipped path, untouched */ }`. The GUI toggle already
drives the tier — no new control. All 11 planned dials are already
present as variants, so most features are just filling the branch. See
[`EXPERIMENTAL-ARC-PLAN-2026-07.md`](EXPERIMENTAL-ARC-PLAN-2026-07.md).

**3. A new codec → the sniff + a rung.** Teach `sniff_codec`, add an
`AuCodec` arm + a rung struct, add encode constants. The codec is carried
by the sniff, so no wire change. AV1 is fully stubbed as the worked
example — [`AV1-SEAMS.md`](AV1-SEAMS.md) maps every stub to what fills it.

## Observability — log lines to grep

The serve log lives at `C:\ProgramData\AllMyStuff\logs\allmystuff-serve.log`
(machine-wide, rotating `.prev`; the path is announced in-log at boot).
Useful greps:

- `labs state:` — the Experimental tier/feature state (a Labs session is
  self-describing).
- `ALLMYSTUFF_RATE_ADAPT` / AIMD reconfigure lines — closed-loop bitrate
  decisions and the per-layer bandwidth riding each route.
- frame-health / GDR wave lines — loss reports name the frame; the wave
  heals with a shaped refresh.
- encode-rung open lines — which rung (NVENC/AMF/MF/sw) actually opened
  and its posture preset.

## The boundary, restated (what NOT to touch)

- **Signaling** carries *zero* media/data. All media and control ride the
  ICE-negotiated STUN/TURN transport via the app channels
  `CHANNEL_CONTROL` / `CHANNEL_MEDIA`. Do not move pipeline data onto the
  signaling plane. (`send_control` → `ChannelSendTo(CHANNEL_CONTROL)` is
  the datapath, not signaling.)
- **`crates/**` and `gui/**`** — Chris's surface. The pipeline never needs
  them; the three seams above are why.
- **Versioning** is Chris's call. Prototype builds carry his last release
  version; never bump it or claim a version without his sign-off.

## Where the rest lives

- [`AV1-SEAMS.md`](AV1-SEAMS.md) — the AV1 stub map + implement order.
- [`EXPERIMENTAL-ARC-PLAN-2026-07.md`](EXPERIMENTAL-ARC-PLAN-2026-07.md) — the Labs arc, per-feature.
- [`SMOOTHNESS-IDEAS-2026-07.md`](SMOOTHNESS-IDEAS-2026-07.md) — the graded idea bank (where this heads next).
- [`ENCODER-PASS-2026-07.md`](ENCODER-PASS-2026-07.md) — the profiling report (before/after).
- `../INTEGRATION-REPORT-2026-07.md` + `../TESTER-KIT-2026-07.md` — the **PR-facing** review dossier (these *do* go upstream).
