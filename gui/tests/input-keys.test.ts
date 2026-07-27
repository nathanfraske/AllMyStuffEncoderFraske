import assert from "node:assert/strict";
import test from "node:test";
import { makeRemoteKeyTracker, type RemoteKeyAction } from "../src/input-keys.ts";

const settle = () => new Promise<void>((resolve) => setImmediate(resolve));

function deferred() {
  let resolve: (accepted: boolean) => void = () => {};
  const promise = new Promise<boolean>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

test("keyup uses the exact route that accepted keydown", async () => {
  const sent: Array<{ routeId: string; action: RemoteKeyAction }> = [];
  const keys = makeRemoteKeyTracker(async (routeId, action) => {
    sent.push({ routeId, action });
    return true;
  });

  keys.onKey("route-a", { kind: "key", key: "x", code: "KeyX", down: true });
  keys.onKey("route-b", { kind: "key", key: "x", code: "KeyX", down: false });
  await settle();

  assert.deepEqual(sent, [
    { routeId: "route-a", action: { kind: "key", key: "x", code: "KeyX", down: true } },
    { routeId: "route-a", action: { kind: "key", key: "x", code: "KeyX", down: false } },
  ]);
});

test("keyup waits for keydown enqueue acknowledgement and retains authority", async () => {
  let acknowledgeDown: (accepted: boolean) => void = () => {};
  let acknowledgeUp: (accepted: boolean) => void = () => {};
  const sent: RemoteKeyAction[] = [];
  const keys = makeRemoteKeyTracker((_routeId, action) => {
    sent.push(action);
    return new Promise<boolean>((resolve) => {
      if (action.down) acknowledgeDown = resolve;
      else acknowledgeUp = resolve;
    });
  });

  keys.onKey("route-a", { kind: "key", key: "x", code: "KeyX", down: true });
  keys.onKey(null, { kind: "key", key: "x", code: "KeyX", down: false });
  await settle();
  assert.equal(sent.length, 1);
  assert.equal(keys.heldCount(), 1);

  acknowledgeDown(true);
  await settle();
  assert.equal(sent.length, 2);
  assert.equal(keys.heldCount(), 1);

  acknowledgeUp(true);
  await settle();
  assert.equal(keys.heldCount(), 0);
});

test("failed keyup acknowledgement remains retryable", async () => {
  let releaseAttempts = 0;
  const sent: RemoteKeyAction[] = [];
  const keys = makeRemoteKeyTracker(async (_routeId, action) => {
    sent.push(action);
    if (action.down) return true;
    releaseAttempts += 1;
    return releaseAttempts > 1;
  });

  keys.onKey("route-a", { kind: "key", key: "x", code: "KeyX", down: true });
  await settle();
  keys.onKey(null, { kind: "key", key: "x", code: "KeyX", down: false });
  await settle();
  assert.equal(keys.heldCount(), 1);

  assert.equal(keys.releaseAll(), 1);
  await settle();
  assert.equal(keys.heldCount(), 0);
  assert.deepEqual(sent.map((action) => action.down), [true, false, false]);
});

test("rapid re-press queues down, up, down and keeps successor held", async () => {
  const gates = [deferred(), deferred(), deferred()];
  const sent: RemoteKeyAction[] = [];
  const keys = makeRemoteKeyTracker((_routeId, action) => {
    sent.push(action);
    return gates[sent.length - 1]!.promise;
  });

  keys.onKey("route-a", { kind: "key", key: "x", code: "KeyX", down: true });
  keys.onKey(null, { kind: "key", key: "x", code: "KeyX", down: false });
  keys.onKey("route-a", { kind: "key", key: "x", code: "KeyX", down: true });
  await settle();
  assert.deepEqual(sent.map((action) => action.down), [true]);

  gates[0]!.resolve(true);
  await settle();
  assert.deepEqual(sent.map((action) => action.down), [true, false]);

  gates[1]!.resolve(true);
  await settle();
  assert.deepEqual(sent.map((action) => action.down), [true, false, true]);

  gates[2]!.resolve(true);
  await settle();
  assert.equal(keys.heldCount(), 1);
});
