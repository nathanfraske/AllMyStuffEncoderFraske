import assert from "node:assert/strict";
import test from "node:test";
import {
  makeRemoteButtonTracker,
  type RemoteMouseButtonAction,
} from "../src/remote-button-tracker.ts";

test("release uses the route that received the press exactly once", () => {
  const sent: Array<{ routeId: string; action: RemoteMouseButtonAction }> = [];
  const buttons = makeRemoteButtonTracker((routeId, action) => sent.push({ routeId, action }));

  assert.equal(buttons.press("route-a", 0), true);
  assert.equal(buttons.release(0), true);
  assert.equal(buttons.release(0), false);
  assert.deepEqual(sent, [
    { routeId: "route-a", action: { kind: "mouse_button", button: 0, down: true } },
    { routeId: "route-a", action: { kind: "mouse_button", button: 0, down: false } },
  ]);
});

test("duplicate press is suppressed and route switch lifts the old route first", () => {
  const sent: Array<{ routeId: string; action: RemoteMouseButtonAction }> = [];
  const buttons = makeRemoteButtonTracker((routeId, action) => sent.push({ routeId, action }));

  assert.equal(buttons.press("route-a", 1), true);
  assert.equal(buttons.press("route-a", 1), false);
  assert.equal(buttons.press("route-b", 1), true);
  assert.equal(buttons.release(1), true);

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

test("cancel, blur, and teardown cleanup is idempotent", () => {
  const sent: Array<{ routeId: string; action: RemoteMouseButtonAction }> = [];
  const buttons = makeRemoteButtonTracker((routeId, action) => sent.push({ routeId, action }));

  buttons.press("route-a", 0);
  buttons.press("route-b", 2);
  assert.equal(buttons.releaseAll(), 2);
  assert.equal(buttons.releaseAll(), 0);
  assert.equal(buttons.heldCount(), 0);
  assert.deepEqual(
    sent.filter(({ action }) => !action.down).map(({ routeId }) => routeId),
    ["route-a", "route-b"],
  );
});
