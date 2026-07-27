export interface TimingSummary {
  count: number;
  avg: number;
  p95: number;
  max: number;
}

const EMPTY_TIMING: TimingSummary = {
  count: 0,
  avg: 0,
  p95: 0,
  max: 0,
};

/** Summarize one bounded diagnostics interval with an exact nearest-rank p95. */
export function summarizeTiming(samples: readonly number[]): TimingSummary {
  const finite = samples.filter((value) => Number.isFinite(value) && value >= 0);
  if (finite.length === 0) return { ...EMPTY_TIMING };

  const sorted = [...finite].sort((a, b) => a - b);
  const total = sorted.reduce((sum, value) => sum + value, 0);
  const p95Index = Math.max(0, Math.ceil(sorted.length * 0.95) - 1);
  return {
    count: sorted.length,
    avg: total / sorted.length,
    p95: sorted[p95Index],
    max: sorted[sorted.length - 1],
  };
}

/** Exact interval timings for opt-in diagnostics. `take` also resets the window. */
export class TimingWindow {
  private samples: number[] = [];

  add(value: number): void {
    if (Number.isFinite(value) && value >= 0) this.samples.push(value);
  }

  take(): TimingSummary {
    const summary = summarizeTiming(this.samples);
    this.samples = [];
    return summary;
  }

  reset(): void {
    this.samples = [];
  }
}
