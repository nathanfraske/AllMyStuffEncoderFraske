import type { RouteHandleState } from "./route-handle-state";
import type { Route, RouteLiveState } from "./types";

export interface SnapshotRouteEntry {
  route: Route;
  state: RouteLiveState;
  term_session?: string | null;
}

export type RouteLossCause = "absent" | "rejected" | "torn_down";

export interface RouteSnapshotLoss {
  routeId: string;
  cause: RouteLossCause;
  reason?: string;
}

export interface RouteSnapshotReconciliation {
  routes: Route[];
  states: Record<string, RouteLiveState>;
  sessions: Record<string, string>;
  losses: RouteSnapshotLoss[];
}

export type ConsoleRouteLane = "video" | "input" | "clipboard";

export interface ConsoleRouteBinding {
  lane: ConsoleRouteLane;
  routeId: string | null;
  desired: boolean;
}

export interface ConsoleRouteRecovery {
  lane: ConsoleRouteLane;
  routeId: string;
  action: "reconnect" | "clear";
  loss: RouteSnapshotLoss;
}

function terminalCause(state: RouteLiveState): RouteLossCause | null {
  if (state.state === "rejected") return "rejected";
  if (state.state === "torn_down") return "torn_down";
  return null;
}

/**
 * Reconcile the GUI catalog against a complete backend route snapshot.
 *
 * A route omitted from the snapshot is gone unless a local connect is still
 * pending or its returned handle has not appeared in a snapshot yet. Offered,
 * incoming, and active routes are all backend-backed and remain visible.
 */
export function reconcileAuthoritativeRouteSnapshot(
  currentRoutes: readonly Route[],
  snapshotRoutes: readonly SnapshotRouteEntry[],
  handles: RouteHandleState,
  interestedRouteIds: Iterable<string> = [],
): RouteSnapshotReconciliation {
  const routes = new Map(currentRoutes.map((route) => [route.id, route]));
  const snapshotIds = new Set<string>();
  const states: Record<string, RouteLiveState> = {};
  const sessions: Record<string, string> = {};
  const losses = new Map<string, RouteSnapshotLoss>();

  for (const live of snapshotRoutes) {
    const routeId = live.route.id;
    snapshotIds.add(routeId);
    const cause = terminalCause(live.state);
    const disposition = handles.reconcileSnapshot(
      routeId,
      cause ? "terminal" : "present",
    );

    if (disposition === "pending") {
      continue;
    }

    states[routeId] = live.state;
    if (live.term_session) sessions[routeId] = live.term_session;

    if (cause) {
      routes.delete(routeId);
      losses.set(routeId, {
        routeId,
        cause,
        reason: live.state.reason,
      });
    } else {
      routes.set(routeId, { ...live.route });
    }
  }

  const absenceCandidates = new Set<string>([
    ...routes.keys(),
    ...interestedRouteIds,
  ]);
  for (const routeId of absenceCandidates) {
    if (!routeId || snapshotIds.has(routeId)) continue;
    if (handles.reconcileSnapshot(routeId, "absent") === "pending") continue;
    routes.delete(routeId);
    losses.set(routeId, { routeId, cause: "absent" });
  }

  return {
    routes: [...routes.values()],
    states,
    sessions,
    losses: [...losses.values()],
  };
}

/**
 * Turn route loss into an explicit console outcome. Unexpected absence keeps
 * the user's on-state by reconnecting. A backend terminal state clears the
 * lane so rejected or torn-down routes never continue accepting local input.
 */
export function planConsoleRouteRecovery(
  bindings: readonly ConsoleRouteBinding[],
  losses: readonly RouteSnapshotLoss[],
): ConsoleRouteRecovery[] {
  const byRoute = new Map(losses.map((loss) => [loss.routeId, loss]));
  const planned: ConsoleRouteRecovery[] = [];

  for (const binding of bindings) {
    if (!binding.routeId) continue;
    const loss = byRoute.get(binding.routeId);
    if (!loss) continue;
    planned.push({
      lane: binding.lane,
      routeId: binding.routeId,
      action: loss.cause === "absent" && binding.desired ? "reconnect" : "clear",
      loss,
    });
  }

  return planned;
}
