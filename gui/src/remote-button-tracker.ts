export interface RemoteMouseButtonAction {
  kind: "mouse_button";
  button: number;
  down: boolean;
}

type SendButton = (routeId: string, action: RemoteMouseButtonAction) => Promise<boolean>;

interface HeldButton {
  routeId: string;
  button: number;
  downAccepted: Promise<boolean>;
  releasePending: boolean;
}

export interface RemoteButtonTracker {
  press(routeId: string, button: number): boolean;
  release(button: number): boolean;
  releaseAll(): number;
  heldCount(): number;
}

/**
 * Owns the route identity for every remote mouse press. A release keeps its
 * authority until the local route FIFO acknowledges it, so a failed enqueue
 * can be retried without consulting current UI state.
 */
export function makeRemoteButtonTracker(send: SendButton): RemoteButtonTracker {
  const active = new Map<number, HeldButton>();
  const authorities = new Set<HeldButton>();
  const transitionTails = new Map<number, Promise<void>>();

  const schedule = (button: number, transition: () => Promise<boolean>): Promise<boolean> => {
    const prior = transitionTails.get(button) ?? Promise.resolve();
    const submitted = prior.then(transition).catch(() => false);
    const tail = submitted.then(
      () => undefined,
      () => undefined,
    );
    transitionTails.set(button, tail);
    void tail.then(() => {
      if (transitionTails.get(button) === tail) transitionTails.delete(button);
    });
    return submitted;
  };

  const retire = (entry: HeldButton) => {
    authorities.delete(entry);
    if (active.get(entry.button) === entry) active.delete(entry.button);
  };

  const releaseEntry = (entry: HeldButton) => {
    if (entry.releasePending) return;
    entry.releasePending = true;
    if (active.get(entry.button) === entry) active.delete(entry.button);

    void schedule(entry.button, async () => {
      const downAccepted = await entry.downAccepted;
      if (!downAccepted) {
        retire(entry);
        return true;
      }
      const accepted = await send(entry.routeId, {
        kind: "mouse_button",
        button: entry.button,
        down: false,
      }).catch(() => false);
      if (accepted) retire(entry);
      else {
        entry.releasePending = false;
        if (!active.has(entry.button)) active.set(entry.button, entry);
      }
      return accepted;
    });
  };

  const release = (button: number): boolean => {
    const entry = active.get(button);
    if (!entry) return false;
    releaseEntry(entry);
    return true;
  };

  return {
    press(routeId, button) {
      const prior = active.get(button);
      if (prior?.routeId === routeId) return false;
      if (prior) releaseEntry(prior);

      const entry: HeldButton = {
        routeId,
        button,
        downAccepted: Promise.resolve(false),
        releasePending: false,
      };
      active.set(button, entry);
      authorities.add(entry);
      entry.downAccepted = schedule(button, () =>
        send(routeId, {
          kind: "mouse_button",
          button,
          down: true,
        }).catch(() => false),
      );
      void entry.downAccepted.then((accepted) => {
        if (!accepted && !entry.releasePending) retire(entry);
      });
      return true;
    },

    release,

    releaseAll() {
      const pending = [...authorities];
      for (const entry of pending) releaseEntry(entry);
      return pending.length;
    },

    heldCount: () => authorities.size,
  };
}
