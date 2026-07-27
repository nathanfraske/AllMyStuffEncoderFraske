import assert from "node:assert/strict";
import test from "node:test";
import { summarizeTiming, TimingWindow } from "../src/video-timing.ts";

test("timing summary reports exact average, nearest-rank p95, and maximum", () => {
  const summary = summarizeTiming(Array.from({ length: 20 }, (_, index) => index + 1));
  assert.deepEqual(summary, {
    count: 20,
    avg: 10.5,
    p95: 19,
    max: 20,
  });
});

test("timing summary ignores invalid samples", () => {
  assert.deepEqual(summarizeTiming([Number.NaN, -1, Number.POSITIVE_INFINITY]), {
    count: 0,
    avg: 0,
    p95: 0,
    max: 0,
  });
});

test("taking a timing window resets it", () => {
  const timing = new TimingWindow();
  timing.add(3);
  timing.add(5);
  assert.deepEqual(timing.take(), { count: 2, avg: 4, p95: 5, max: 5 });
  assert.deepEqual(timing.take(), { count: 0, avg: 0, p95: 0, max: 0 });
});
