import assert from "node:assert/strict";
import test from "node:test";
import { makeInputDispatcher, type InputEnqueue } from "../src/input-dispatch.ts";
import type { InputAction } from "../src/types.ts";

function deferred<T>() {
  let resolve: (value: T) => void = () => {};
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

test("discrete submissions are route-scoped FIFO", async () => {
  const first = deferred<boolean>();
  const calls: InputAction[] = [];
  const enqueue: InputEnqueue = (_routeId, action) => {
    calls.push(action);
    return calls.length === 1 ? first.promise : Promise.resolve(true);
  };
  const input = makeInputDispatcher(enqueue);

  const down = input.send("route-a", { kind: "key", key: "a", down: true });
  const up = input.send("route-a", { kind: "key", key: "a", down: false });
  await Promise.resolve();
  assert.deepEqual(calls.map((action) => action.kind), ["key"]);

  first.resolve(true);
  assert.equal(await down, true);
  assert.equal(await up, true);
  assert.deepEqual(
    calls.map((action) => action.kind === "key" && action.down),
    [true, false],
  );
});

test("pointer motion coalesces without blocking a discrete release", async () => {
  const firstMotion = deferred<boolean>();
  const calls: Array<{ action: InputAction; ordered: boolean }> = [];
  const enqueue: InputEnqueue = (_routeId, action, ordered) => {
    calls.push({ action, ordered });
    if (action.kind === "mouse_move" && calls.length === 1) return firstMotion.promise;
    return Promise.resolve(true);
  };
  const input = makeInputDispatcher(enqueue);

  void input.send("route-a", { kind: "mouse_move", x: 0.1, y: 0.1 });
  void input.send("route-a", { kind: "mouse_move", x: 0.2, y: 0.2 });
  void input.send("route-a", { kind: "mouse_move", x: 0.3, y: 0.3 });
  await input.send("route-a", { kind: "mouse_button", button: 0, down: false });

  assert.equal(calls.length, 2);
  assert.equal(calls[0]?.action.kind, "mouse_move");
  assert.equal(calls[1]?.action.kind, "mouse_button");

  firstMotion.resolve(true);
  await new Promise<void>((resolve) => setImmediate(resolve));
  assert.equal(calls.length, 3);
  assert.deepEqual(calls[2]?.action, { kind: "mouse_move", x: 0.3, y: 0.3 });
});

test("ordered cursor re-seat shares the discrete FIFO", async () => {
  const first = deferred<boolean>();
  const calls: InputAction[] = [];
  const input = makeInputDispatcher((_routeId, action) => {
    calls.push(action);
    return calls.length === 1 ? first.promise : Promise.resolve(true);
  });

  const move = input.send("route-a", { kind: "mouse_move", x: 0.4, y: 0.5 }, true);
  const down = input.send("route-a", { kind: "mouse_button", button: 0, down: true });
  await Promise.resolve();
  assert.equal(calls.length, 1);

  first.resolve(true);
  await move;
  await down;
  assert.deepEqual(calls.map((action) => action.kind), ["mouse_move", "mouse_button"]);
});
