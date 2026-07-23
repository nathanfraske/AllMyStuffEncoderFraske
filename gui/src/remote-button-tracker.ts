export interface RemoteMouseButtonAction {
  kind: "mouse_button";
  button: number;
  down: boolean;
}

export interface RemoteButtonTracker {
  press(routeId: string, button: number): boolean;
  release(button: number): boolean;
  releaseAll(): number;
  heldCount(): number;
}

/**
 * Owns the route identity for every remote mouse press. Releases never consult
 * current UI state, so a blur, route switch, pointer cancellation, or teardown
 * still sends one matching lift to the route that received the press.
 */
export function makeRemoteButtonTracker(
  send: (routeId: string, action: RemoteMouseButtonAction) => void,
): RemoteButtonTracker {
  const held = new Map<number, string>();

  const release = (button: number): boolean => {
    const routeId = held.get(button);
    if (routeId === undefined) return false;
    held.delete(button);
    send(routeId, { kind: "mouse_button", button, down: false });
    return true;
  };

  return {
    press(routeId, button) {
      const priorRoute = held.get(button);
      if (priorRoute === routeId) return false;
      if (priorRoute !== undefined) release(button);
      held.set(button, routeId);
      send(routeId, { kind: "mouse_button", button, down: true });
      return true;
    },

    release,

    releaseAll() {
      const pending = [...held.entries()];
      held.clear();
      for (const [button, routeId] of pending) {
        send(routeId, { kind: "mouse_button", button, down: false });
      }
      return pending.length;
    },

    heldCount: () => held.size,
  };
}
