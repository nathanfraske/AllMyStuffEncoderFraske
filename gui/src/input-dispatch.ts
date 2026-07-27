import type { InputAction } from "./types";

export type InputEnqueue = (
  routeId: string,
  action: InputAction,
  ordered: boolean,
) => Promise<boolean>;

export interface InputDispatcher {
  send(routeId: string, action: InputAction, ordered?: boolean): Promise<boolean>;
}

function isPointerMotion(action: InputAction): boolean {
  return action.kind === "mouse_move" || action.kind === "mouse_move_rel";
}

/**
 * Serializes discrete local IPC submissions per route. Pointer motion has an
 * independent one-in-flight, one-latest lane, so stale cursor samples are
 * coalesced and never sit ahead of a key or button release.
 */
export function makeInputDispatcher(enqueue: InputEnqueue): InputDispatcher {
  const discreteTails = new Map<string, Promise<void>>();
  const motion = new Map<
    string,
    {
      inFlight: boolean;
      pending: InputAction | null;
    }
  >();

  const sendDiscrete = (
    routeId: string,
    action: InputAction,
    ordered: boolean,
  ): Promise<boolean> => {
    const prior = discreteTails.get(routeId) ?? Promise.resolve();
    const submitted = prior.then(() => enqueue(routeId, action, ordered));
    const tail = submitted.then(
      () => undefined,
      () => undefined,
    );
    discreteTails.set(routeId, tail);
    void tail.then(() => {
      if (discreteTails.get(routeId) === tail) discreteTails.delete(routeId);
    });
    return submitted;
  };

  const pumpMotion = (routeId: string, state: { inFlight: boolean; pending: InputAction | null }) => {
    const action = state.pending;
    if (!action) {
      state.inFlight = false;
      motion.delete(routeId);
      return;
    }
    state.pending = null;
    state.inFlight = true;
    void enqueue(routeId, action, false)
      .catch(() => false)
      .then(() => pumpMotion(routeId, state));
  };

  return {
    send(routeId, action, ordered = false) {
      if (ordered || !isPointerMotion(action)) {
        return sendDiscrete(routeId, action, ordered);
      }

      let state = motion.get(routeId);
      if (!state) {
        state = { inFlight: false, pending: null };
        motion.set(routeId, state);
      }
      state.pending = action;
      if (!state.inFlight) pumpMotion(routeId, state);
      // Motion is deliberately lossy. Acceptance into the latest-only local
      // slot is its acknowledgement; held-state transitions never use it.
      return Promise.resolve(true);
    },
  };
}
