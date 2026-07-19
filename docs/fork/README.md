# `docs/fork/` — fork-internal pipeline docs (NOT for the upstream PR)

Everything in this directory documents **how to work on this fork's video
pipeline** — the encoders, decoders, and datapath. It is internal to
nathanfraske's fork and is deliberately **kept out of the pull request to
the upstream (Chris's) repo**, so the PR stays code-focused and Chris's
own docs tree is untouched.

## What's here (fork-only)

| Doc | What it is |
|---|---|
| [`PIPELINE.md`](PIPELINE.md) | **Start here.** The single operational reference: the encode/decode ladders, postures, the sniff, the pacer + closed loop, every operator dial, the three seams you extend, and the log lines. |
| [`AV1-SEAMS.md`](AV1-SEAMS.md) | The AV1 stub map — every seam and what fills it, in implement order. |
| [`EXPERIMENTAL-ARC-PLAN-2026-07.md`](EXPERIMENTAL-ARC-PLAN-2026-07.md) | The Labs (Experimental) feature arc, per-feature, with the gate reconciled to shipped code. |
| [`SMOOTHNESS-IDEAS-2026-07.md`](SMOOTHNESS-IDEAS-2026-07.md) | The graded smoothness/pacing/latency idea bank — where the pipeline heads next. |
| [`ENCODER-PASS-2026-07.md`](ENCODER-PASS-2026-07.md) | The encoder-pass profiling report — fixes and before/after numbers. |

## What is NOT here (goes with the PR)

The **review dossier** stays at `docs/` root because it is written *for*
Chris to review the PR:

- `../INTEGRATION-REPORT-2026-07.md` — diffs, pipeline map, blast radius.
- `../TESTER-KIT-2026-07.md` — how to verify the change.

And `../../HANDOFF.md` (repo root) is the fork engineering handoff — also
fork-internal, also excluded from the PR, but kept at the root by
convention as the next-agent entry point.

## How these stay out of the PR

A GitHub PR is a whole-branch diff, so "on the fork but not in the PR"
means the files must be absent from the branch the PR is opened from. The
mechanism (also written up in `../TESTER-KIT-2026-07.md`): open the PR
from a curated branch that strips the fork-only paths.

```sh
# from fork main, build the upstream PR branch
git switch -c for-upstream main
git rm -r docs/fork
git rm HANDOFF.md
git commit -m "chore: strip fork-internal docs for the upstream PR"
gh pr create --repo <upstream>/<repo> --base main \
  --head nathanfraske:for-upstream
# docs/fork/ and HANDOFF.md remain on fork main; the PR never carries them.
```

`.gitattributes` also marks these paths `export-ignore` so `git archive`
tarballs omit them — a secondary signal, not the PR mechanism itself.

To move a doc across the line later, just `git mv` it in or out of this
directory and update the curated-branch `git rm` list to match.
