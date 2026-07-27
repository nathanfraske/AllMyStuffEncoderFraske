export interface RouteHandle {
  routeId: string;
  generation: number;
}

export interface RouteConnectIntent {
  routeId: string;
  revision: number;
}

export interface RouteConnectStart {
  intent: RouteConnectIntent;
  displaced: RouteHandle | null;
}

export interface RouteInvalidation {
  tracked: boolean;
  wasPending: boolean;
  handle: RouteHandle | null;
}

export type RouteHandleSettlement =
  | { accepted: true; stale: null }
  | { accepted: false; stale: RouteHandle };

export type RouteSnapshotObservation = "present" | "terminal" | "absent";
export type RouteSnapshotDisposition = "present" | "pending" | "absent";

interface RouteEntry {
  revision: number;
  pending: boolean;
  handle: RouteHandle | null;
  observed: boolean;
}

/**
 * Tracks one local intent revision separately from the backend generation.
 * The revision rejects late JavaScript completions. The generation is kept
 * intact for the backend's compare-and-disconnect guard.
 */
export class RouteHandleState {
  private readonly entries = new Map<string, RouteEntry>();

  begin(routeId: string): RouteConnectStart {
    const prior = this.entries.get(routeId);
    const revision = (prior?.revision ?? 0) + 1;
    this.entries.set(routeId, {
      revision,
      pending: true,
      handle: null,
      observed: false,
    });
    return {
      intent: { routeId, revision },
      displaced: prior?.handle ?? null,
    };
  }

  accept(intent: RouteConnectIntent, handle: RouteHandle): boolean {
    const current = this.entries.get(intent.routeId);
    if (
      !current ||
      current.revision !== intent.revision ||
      !current.pending ||
      handle.routeId !== intent.routeId
    ) {
      return false;
    }
    current.pending = false;
    current.handle = handle;
    return true;
  }

  settle(intent: RouteConnectIntent, handle: RouteHandle): RouteHandleSettlement {
    return this.accept(intent, handle)
      ? { accepted: true, stale: null }
      : { accepted: false, stale: handle };
  }

  fail(intent: RouteConnectIntent): boolean {
    const current = this.entries.get(intent.routeId);
    if (!current || current.revision !== intent.revision || !current.pending) return false;
    current.pending = false;
    current.handle = null;
    current.observed = false;
    return true;
  }

  invalidate(routeId: string): RouteInvalidation {
    const prior = this.entries.get(routeId);
    if (!prior) return { tracked: false, wasPending: false, handle: null };
    this.entries.set(routeId, {
      revision: prior.revision + 1,
      pending: false,
      handle: null,
      observed: false,
    });
    return {
      tracked: true,
      wasPending: prior.pending,
      handle: prior.handle,
    };
  }

  isCurrent(intent: RouteConnectIntent): boolean {
    const current = this.entries.get(intent.routeId);
    return current?.revision === intent.revision;
  }

  currentHandle(routeId: string): RouteHandle | null {
    return this.entries.get(routeId)?.handle ?? null;
  }

  isPending(routeId: string): boolean {
    return this.entries.get(routeId)?.pending === true;
  }

  /**
   * Fold one authoritative backend observation into the local intent state.
   *
   * An in-flight command wins over an absent or terminal snapshot because the
   * snapshot may describe the predecessor that the command is replacing. An
   * accepted handle also survives absence until one backend snapshot has
   * actually reported that route. Once observed, later absence is
   * authoritative and retires the local generation.
   */
  reconcileSnapshot(
    routeId: string,
    observation: RouteSnapshotObservation,
  ): RouteSnapshotDisposition {
    const current = this.entries.get(routeId);

    if (observation === "present") {
      if (current?.pending) {
        // The event stream can deliver the route snapshot before the
        // short-lived command socket delivers its response. Remember the
        // positive observation, but keep the command pending until its exact
        // handle settles.
        current.observed = true;
        return "pending";
      }
      if (current?.handle) current.observed = true;
      return "present";
    }

    if (current?.pending) return "pending";
    if (observation === "absent" && current?.handle && !current.observed) {
      return "pending";
    }

    if (current && (current.handle || current.observed)) {
      this.entries.set(routeId, {
        revision: current.revision + 1,
        pending: false,
        handle: null,
        observed: false,
      });
    }
    return "absent";
  }

  hasPendingIntent(routeId: string): boolean {
    const current = this.entries.get(routeId);
    return current?.pending === true || (!!current?.handle && !current.observed);
  }

  canAddress(routeId: string, active: boolean): boolean {
    const entry = this.entries.get(routeId);
    if (entry?.pending) return false;
    return entry?.handle ? true : active;
  }
}
