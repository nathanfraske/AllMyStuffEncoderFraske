import assert from "node:assert/strict";
import test from "node:test";
import { RouteHandleState, type RouteHandle } from "../src/route-handle-state.ts";

const handle = (generation: number): RouteHandle => ({
  routeId: "route:screen->viewer",
  generation,
});

test("a stale A completion cannot overwrite accepted B", () => {
  const state = new RouteHandleState();
  const a = state.begin(handle(1).routeId).intent;
  const invalidated = state.invalidate(a.routeId);
  assert.equal(invalidated.wasPending, true);

  const b = state.begin(a.routeId).intent;
  assert.equal(state.accept(b, handle(2)), true);
  assert.deepEqual(state.settle(a, handle(1)), {
    accepted: false,
    stale: handle(1),
  });
  assert.deepEqual(state.currentHandle(a.routeId), handle(2));
});

test("disconnect captures A generation before B is installed", () => {
  const state = new RouteHandleState();
  const a = state.begin(handle(11).routeId).intent;
  assert.equal(state.accept(a, handle(11)), true);

  const closeA = state.invalidate(a.routeId);
  const b = state.begin(a.routeId).intent;
  assert.equal(state.accept(b, handle(12)), true);

  assert.deepEqual(closeA.handle, handle(11));
  assert.deepEqual(state.currentHandle(a.routeId), handle(12));
});

test("invalidating a pending connect defers close until its handle arrives", () => {
  const state = new RouteHandleState();
  const a = state.begin(handle(20).routeId).intent;
  const close = state.invalidate(a.routeId);

  assert.equal(close.tracked, true);
  assert.equal(close.wasPending, true);
  assert.equal(close.handle, null);
  assert.equal(state.accept(a, handle(20)), false);
  assert.equal(state.currentHandle(a.routeId), null);
});

test("only the current failure can reconcile local state", () => {
  const state = new RouteHandleState();
  const a = state.begin(handle(30).routeId).intent;
  const b = state.begin(handle(30).routeId).intent;

  assert.equal(state.fail(a), false);
  assert.equal(state.isPending(b.routeId), true);
  assert.equal(state.fail(b), true);
  assert.equal(state.isPending(b.routeId), false);
});

test("route commands wait for a matching handle or observed active route", () => {
  const state = new RouteHandleState();
  const pending = state.begin(handle(40).routeId).intent;

  assert.equal(state.canAddress(pending.routeId, false), false);
  assert.equal(state.canAddress(pending.routeId, true), false);
  assert.equal(state.accept(pending, handle(40)), true);
  assert.equal(state.canAddress(pending.routeId, false), true);

  const observed = new RouteHandleState();
  assert.equal(observed.canAddress(pending.routeId, true), true);
});
