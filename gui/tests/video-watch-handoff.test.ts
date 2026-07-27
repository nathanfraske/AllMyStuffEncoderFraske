import assert from "node:assert/strict";
import test from "node:test";
import { VideoWatchHandoff } from "../src/video-watch-handoff.ts";

function fakeTimers() {
  let next = 1;
  const jobs = new Map<number, () => void>();
  const cancelled: number[] = [];
  return {
    schedule(callback: () => void): ReturnType<typeof setTimeout> {
      const id = next++;
      jobs.set(id, callback);
      return id as unknown as ReturnType<typeof setTimeout>;
    },
    cancel(handle: ReturnType<typeof setTimeout>): void {
      const id = handle as unknown as number;
      cancelled.push(id);
      jobs.delete(id);
    },
    runAll(): void {
      const callbacks = [...jobs.values()];
      jobs.clear();
      for (const callback of callbacks) callback();
    },
    cancelled,
  };
}

test("ordinary close releases only after the handoff window", () => {
  const timers = fakeTimers();
  const released: number[] = [];
  const handoff = new VideoWatchHandoff(
    80,
    (callback) => timers.schedule(callback),
    (handle) => timers.cancel(handle),
  );

  handoff.defer("route:a", 11, () => released.push(11));
  assert.equal(handoff.pendingCount("route:a"), 1);
  assert.deepEqual(released, []);

  timers.runAll();
  assert.deepEqual(released, [11]);
  assert.equal(handoff.pendingCount("route:a"), 0);
});

test("successful same-route registration retires the predecessor after replacement", () => {
  const timers = fakeTimers();
  const released: number[] = [];
  const handoff = new VideoWatchHandoff(
    80,
    (callback) => timers.schedule(callback),
    (handle) => timers.cancel(handle),
  );

  handoff.defer("route:a", 12, () => released.push(12));
  assert.equal(handoff.complete("route:a"), 1);
  assert.deepEqual(released, [12]);
  assert.equal(timers.cancelled.length, 1);

  timers.runAll();
  assert.deepEqual(released, [12], "cancelled retirement must not fire twice");
});

test("a replacement cannot consume another route's pending release", () => {
  const timers = fakeTimers();
  const released: string[] = [];
  const handoff = new VideoWatchHandoff(
    80,
    (callback) => timers.schedule(callback),
    (handle) => timers.cancel(handle),
  );

  handoff.defer("route:a", 21, () => released.push("a"));
  handoff.defer("route:b", 22, () => released.push("b"));
  assert.equal(handoff.complete("route:b"), 1);
  assert.deepEqual(released, ["b"]);
  assert.equal(handoff.pendingCount("route:a"), 1);

  timers.runAll();
  assert.deepEqual(released, ["b", "a"]);
});
