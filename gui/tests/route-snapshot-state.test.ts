import assert from "node:assert/strict";
import test from "node:test";
import { RouteHandleState, type RouteHandle } from "../src/route-handle-state.ts";
import {
  planConsoleRouteRecovery,
  reconcileAuthoritativeRouteSnapshot,
  type SnapshotRouteEntry,
} from "../src/route-snapshot-state.ts";
import type { MediaKind, Route } from "../src/types.ts";

const route = (id: string, media: MediaKind): Route => ({
  id,
  from: `${id}:from`,
  to: `${id}:to`,
  media,
});

const handle = (routeId: string, generation: number): RouteHandle => ({
  routeId,
  generation,
});

const active = (value: Route): SnapshotRouteEntry => ({
  route: value,
  state: { state: "active" },
});

test("a fresh empty snapshot removes previously observed video, input, and clipboard routes", () => {
  const handles = new RouteHandleState();
  const routes = [
    route("route:video", "display"),
    route("route:input", "input"),
    route("route:clipboard", "clipboard"),
  ];

  for (const [index, value] of routes.entries()) {
    const intent = handles.begin(value.id).intent;
    assert.equal(handles.accept(intent, handle(value.id, index + 1)), true);
  }
  reconcileAuthoritativeRouteSnapshot(routes, routes.map(active), handles);

  const reconciled = reconcileAuthoritativeRouteSnapshot(routes, [], handles);
  assert.deepEqual(reconciled.routes, []);
  assert.deepEqual(
    reconciled.losses.map((loss) => [loss.routeId, loss.cause]),
    routes.map((value) => [value.id, "absent"]),
  );

  assert.deepEqual(
    planConsoleRouteRecovery(
      [
        { lane: "video", routeId: routes[0].id, desired: true },
        { lane: "input", routeId: routes[1].id, desired: true },
        { lane: "clipboard", routeId: routes[2].id, desired: true },
      ],
      reconciled.losses,
    ).map((recovery) => [recovery.lane, recovery.action]),
    [
      ["video", "reconnect"],
      ["input", "reconnect"],
      ["clipboard", "reconnect"],
    ],
  );
});

test("absence preserves an in-flight or not-yet-observed desired route", () => {
  const handles = new RouteHandleState();
  const value = route("route:pending-video", "display");
  const intent = handles.begin(value.id).intent;

  let reconciled = reconcileAuthoritativeRouteSnapshot([value], [], handles);
  assert.deepEqual(reconciled.routes, [value]);
  assert.deepEqual(reconciled.losses, []);

  assert.equal(handles.accept(intent, handle(value.id, 9)), true);
  reconciled = reconcileAuthoritativeRouteSnapshot([value], [], handles);
  assert.deepEqual(reconciled.routes, [value]);
  assert.deepEqual(reconciled.losses, []);
});

test("a positive snapshot delivered before the command response still confirms that handle", () => {
  const handles = new RouteHandleState();
  const value = route("route:event-first", "display");
  const intent = handles.begin(value.id).intent;

  reconcileAuthoritativeRouteSnapshot([value], [active(value)], handles);
  assert.equal(handles.accept(intent, handle(value.id, 10)), true);

  const reconciled = reconcileAuthoritativeRouteSnapshot([value], [], handles);
  assert.deepEqual(reconciled.routes, []);
  assert.deepEqual(reconciled.losses, [{ routeId: value.id, cause: "absent" }]);
});

test("rejected and torn-down video, input, and clipboard routes clear instead of reconnecting", () => {
  const handles = new RouteHandleState();
  const routes = [
    route("route:video-terminal", "display"),
    route("route:input-terminal", "input"),
    route("route:clipboard-terminal", "clipboard"),
  ];
  const terminal: SnapshotRouteEntry[] = [
    { route: routes[0], state: { state: "rejected", reason: "denied" } },
    { route: routes[1], state: { state: "torn_down" } },
    { route: routes[2], state: { state: "rejected", reason: "expired" } },
  ];
  for (const [index, value] of routes.entries()) {
    const intent = handles.begin(value.id).intent;
    assert.equal(handles.accept(intent, handle(value.id, index + 20)), true);
  }

  const reconciled = reconcileAuthoritativeRouteSnapshot(routes, terminal, handles);
  assert.deepEqual(reconciled.routes, []);
  assert.deepEqual(
    planConsoleRouteRecovery(
      [
        { lane: "video", routeId: routes[0].id, desired: true },
        { lane: "input", routeId: routes[1].id, desired: true },
        { lane: "clipboard", routeId: routes[2].id, desired: true },
      ],
      reconciled.losses,
    ).map((recovery) => [recovery.lane, recovery.action, recovery.loss.cause]),
    [
      ["video", "clear", "rejected"],
      ["input", "clear", "torn_down"],
      ["clipboard", "clear", "rejected"],
    ],
  );
});

test("a predecessor terminal snapshot cannot erase a replacement command still in flight", () => {
  const handles = new RouteHandleState();
  const value = route("route:same-monitor", "display");
  const replacement = handles.begin(value.id).intent;
  const reconciled = reconcileAuthoritativeRouteSnapshot(
    [value],
    [{ route: value, state: { state: "torn_down" } }],
    handles,
  );

  assert.deepEqual(reconciled.routes, [value]);
  assert.deepEqual(reconciled.losses, []);
  assert.equal(handles.isCurrent(replacement), true);
});
