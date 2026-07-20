#!/usr/bin/env python3
"""Summarize bounded AllMyStuff video-profiler JSONL traces.

The trace contains process-local monotonic durations. This tool intentionally
does not subtract timestamps from different files/processes: cross-box clocks
are not assumed synchronized and RTP timestamps are labels, not wall clocks.
"""

from __future__ import annotations

import argparse
import json
import math
import pathlib
import sys
from collections import defaultdict
from dataclasses import dataclass, field


STAGE_KINDS = {
    "capture_wait": "cadence",
    "capture_age": "gauge",
    "convert_busy": "busy",
    "encoder_queue_wait": "queue_wait",
    "encode_busy": "busy",
    "encoder_output_delivery": "delivery",
    "outbound_route_queue_wait": "queue_wait",
    "outbound_pace_wait": "pace_wait",
    "outbound_serialize_busy": "busy",
    "outbound_pipe_wait": "queue_wait",
    "outbound_pipe_connect_wait": "io_wait",
    "outbound_pipe_write": "io_wait",
    "inbound_pipe_wait_read": "cadence",
    "inbound_parse_busy": "busy",
    "inbound_dispatch_backpressure": "queue_wait",
    "inbound_dispatch_wait": "queue_wait",
    "decoder_queue_wait": "queue_wait",
    "decoder_prepare_busy": "busy",
    "decoder_coalesce_wait": "queue_wait",
    "decode_busy": "busy",
    "frame_delivery": "delivery",
    "viewer_queue_wait": "queue_wait",
    "viewer_poll_cadence": "cadence",
    "viewer_poll_lock_wait": "queue_wait",
    "viewer_batch_busy": "busy",
    "viewer_ipc_write": "io_wait",
}

# These are valuable standalone measurements but overlap another interval or
# describe an age/cadence rather than sequential work. They must never be added
# to a per-frame local stage sum.
NON_ADDITIVE_STAGES = {
    "capture_wait",
    "capture_age",
    "inbound_pipe_wait_read",
    "encoder_output_delivery",
    "frame_delivery",
    "viewer_poll_cadence",
}


@dataclass
class Series:
    values_ns: list[int] = field(default_factory=list)

    def add(self, value: int) -> None:
        if value >= 0:
            self.values_ns.append(value)

    def percentile(self, percent: int) -> int:
        if not self.values_ns:
            return 0
        ordered = sorted(self.values_ns)
        index = max(0, math.ceil(len(ordered) * percent / 100) - 1)
        return ordered[index]


def ms(value_ns: int | float) -> float:
    return float(value_ns) / 1_000_000.0


def load(path: pathlib.Path):
    series: dict[tuple[int, str, str, str], Series] = defaultdict(Series)
    frame_totals: dict[tuple[int, int], int] = defaultdict(int)
    frame_routes: dict[tuple[int, int], set[str]] = defaultdict(set)
    frame_stages: dict[tuple[int, int], set[str]] = defaultdict(set)
    malformed = 0
    with path.open("r", encoding="utf-8") as source:
        for line_number, line in enumerate(source, 1):
            if not line.strip():
                continue
            try:
                event = json.loads(line)
                pid = int(event["pid"])
                route = str(event["route"])
                stage = str(event["stage"])
                kind = str(event.get("kind") or STAGE_KINDS.get(stage, "unknown"))
                duration = int(event["duration_ns"])
                frame_id = int(event.get("frame_id", 0))
            except (KeyError, TypeError, ValueError, json.JSONDecodeError):
                malformed += 1
                continue
            series[(pid, route, kind, stage)].add(duration)
            if frame_id and stage not in NON_ADDITIVE_STAGES:
                frame_key = (pid, frame_id)
                frame_totals[frame_key] += max(0, duration)
                frame_routes[frame_key].add(route)
                frame_stages[frame_key].add(stage)
    return series, frame_totals, frame_routes, frame_stages, malformed


def print_summary(path: pathlib.Path, top_frames: int) -> int:
    series, frame_totals, frame_routes, frame_stages, malformed = load(path)
    if not series:
        print(f"{path}: no valid profiler events", file=sys.stderr)
        return 1

    print(f"\n{path}")
    print(
        f"{'pid':>7}  {'kind':<10}  {'n':>7}  {'avg ms':>10}  {'p50':>10}  "
        f"{'p95':>10}  {'p99':>10}  {'max':>10}  route / stage"
    )
    for (pid, route, kind, stage), values in sorted(series.items()):
        samples = values.values_ns
        average = sum(samples) / len(samples)
        print(
            f"{pid:7d}  {kind:<10}  {len(samples):7d}  {ms(average):10.3f}  "
            f"{ms(values.percentile(50)):10.3f}  {ms(values.percentile(95)):10.3f}  "
            f"{ms(values.percentile(99)):10.3f}  {ms(max(samples)):10.3f}  "
            f"{route} / {stage}"
        )

    if top_frames:
        print(
            "\nLargest selected non-overlapping local stage sums "
            "(not end-to-end or cross-process/network latency):"
        )
        for (pid, frame_id), total in sorted(
            frame_totals.items(), key=lambda item: item[1], reverse=True
        )[:top_frames]:
            key = (pid, frame_id)
            routes = ",".join(sorted(frame_routes[key]))
            stages = ",".join(sorted(frame_stages[key]))
            print(
                f"  {ms(total):10.3f} ms  pid={pid} frame={frame_id} "
                f"routes={routes} stages={stages}"
            )

    if malformed:
        print(f"warning: skipped {malformed} malformed lines", file=sys.stderr)
    return 0


def main() -> int:
    # Windows PowerShell can expose a legacy CP-1252 console even though route
    # labels are UTF-8 (for example, the source-to-sink arrow).  Make the CLI
    # deterministic when it is run interactively, piped, or captured by CI.
    for stream in (sys.stdout, sys.stderr):
        reconfigure = getattr(stream, "reconfigure", None)
        if reconfigure is not None:
            reconfigure(encoding="utf-8", errors="backslashreplace")

    parser = argparse.ArgumentParser(
        description="Summarize local AllMyStuff video stage-profiler JSONL traces."
    )
    parser.add_argument("trace", nargs="+", type=pathlib.Path)
    parser.add_argument(
        "--top-frames",
        type=int,
        default=10,
        help="show N largest sums sharing a process-local frame id (default: 10)",
    )
    args = parser.parse_args()
    status = 0
    for path in args.trace:
        try:
            status |= print_summary(path, max(0, args.top_frames))
        except OSError as error:
            print(f"{path}: {error}", file=sys.stderr)
            status = 1
    return status


if __name__ == "__main__":
    raise SystemExit(main())
