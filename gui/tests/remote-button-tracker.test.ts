import assert from "node:assert/strict";
import test from "node:test";
import {
  makeRemoteButtonTracker,
  type RemoteMouseButtonAction,
} from "../src/remote-button-tracker.ts";

const settle = () => new Promise<void>((resolve) => setImmediate(resolve));

function deferred() {
  let resolve: (accepted: boolean) => void = () => {};
  const promise = new Promise<boolean>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

test("release uses the route that received the press exactly once", async () => {
  const sent: Array<{ routeId: string; action: RemoteMouseButtonAction }> = [];
  const buttons = makeRemoteButtonTracker(async (routeId, action) => {
    sent.push({ routeId, action });
    return true;
  });

  assert.equal(buttons.press("route-a", 0), true);
  assert.equal(buttons.release(0), true);
  assert.equal(buttons.release(0), false);
  await settle();
  assert.deepEqual(sent, [
    { routeId: "route-a", action: { kind: "mouse_button", button: 0, down: true } },
    { routeId: "route-a", action: { kind: "mouse_button", button: 0, down: false } },
  ]);
});

test("authority remains held until release enqueue acknowledgement", async () => {
  let acknowledgeRelease: (accepted: boolean) => void = () => {};
  const buttons = makeRemoteButtonTracker((_routeId, action) => {
    if (action.down) return Promise.resolve(true);
    return new Promise<boolean>((resolve) => {
      acknowledgeRelease = resolve;
    });
  });

  buttons.press("route-a", 0);
  await settle();
  buttons.release(0);
  await settle();
  assert.equal(buttons.heldCount(), 1);

  acknowledgeRelease(true);
  await settle();
  assert.equal(buttons.heldCount(), 0);
});

test("failed release acknowledgement retains authority for retry", async () => {
  let releaseAttempts = 0;
  const sent: RemoteMouseButtonAction[] = [];
  const buttons = makeRemoteButtonTracker(async (_routeId, action) => {
    sent.push(action);
    if (action.down) return true;
    releaseAttempts += 1;
    return releaseAttempts > 1;
  });

  buttons.press("route-a", 0);
  await settle();
  assert.equal(buttons.release(0), true);
  await settle();
  assert.equal(buttons.heldCount(), 1);
  assert.equal(buttons.release(0), true);
  await settle();
  assert.equal(buttons.heldCount(), 0);
  assert.deepEqual(sent.map((action) => action.down), [true, false, false]);
});

test("route switch lifts the old route and tracks the new route independently", async () => {
  const sent: Array<{ routeId: string; action: RemoteMouseButtonAction }> = [];
  const buttons = makeRemoteButtonTracker(async (routeId, action) => {
    sent.push({ routeId, action });
    return true;
  });

  assert.equal(buttons.press("route-a", 1), true);
  assert.equal(buttons.press("route-a", 1), false);
  assert.equal(buttons.press("route-b", 1), true);
  assert.equal(buttons.release(1), true);
  await settle();

  assert.deepEqual(
    sent.map(({ routeId, action }) => [routeId, action.down]),
    [
      ["route-a", true],
      ["route-a", false],
      ["route-b", true],
      ["route-b", false],
    ],
  );
});

test("cancel, blur, and teardown cleanup is idempotent", async () => {
  const sent: Array<{ routeId: string; action: RemoteMouseButtonAction }> = [];
  const buttons = makeRemoteButtonTracker(async (routeId, action) => {
    sent.push({ routeId, action });
    return true;
  });

  buttons.press("route-a", 0);
  buttons.press("route-b", 2);
  assert.equal(buttons.releaseAll(), 2);
  assert.equal(buttons.releaseAll(), 2);
  await settle();
  assert.equal(buttons.releaseAll(), 0);
  assert.equal(buttons.heldCount(), 0);
  assert.deepEqual(
    sent.filter(({ action }) => !action.down).map(({ routeId }) => routeId),
    ["route-a", "route-b"],
  );
});

test("rapid re-press queues down, up, down and keeps successor held", async () => {
  const gates = [deferred(), deferred(), deferred()];
  const sent: RemoteMouseButtonAction[] = [];
  const buttons = makeRemoteButtonTracker((_routeId, action) => {
    sent.push(action);
    return gates[sent.length - 1]!.promise;
  });

  buttons.press("route-a", 0);
  buttons.release(0);
  buttons.press("route-a", 0);
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
  assert.equal(buttons.heldCount(), 1);
});
