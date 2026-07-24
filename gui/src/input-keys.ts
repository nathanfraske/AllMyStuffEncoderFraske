import type { InputAction } from "./types";

const MODIFIER_KEYS = new Set(["Shift", "Control", "Alt", "Meta", "AltGraph"]);

export type RemoteKeyAction = Extract<InputAction, { kind: "key" }>;
type SendKey = (routeId: string, action: RemoteKeyAction) => Promise<boolean>;

interface HeldKey {
  id: string;
  routeId: string;
  key: string;
  code?: string;
  downAccepted: Promise<boolean>;
  releasePending: boolean;
}

interface KeyAuthority {
  transition(routeId: string | null, action: RemoteKeyAction, repeat?: boolean): void;
  releaseAll(): number;
  heldCount(): number;
}

export interface RemoteKeyTracker {
  onKey(routeId: string | null, action: RemoteKeyAction): void;
  releaseAll(): number;
  heldCount(): number;
}

export interface KeyForwarder {
  /**
   * Forward one key transition. A keydown captures `routeId`; its keyup uses
   * that captured route even if the UI has switched routes in the meantime.
   */
  onKey(routeId: string | null, event: KeyboardEvent, down: boolean): void;
  /** Queue lifts for everything still held or awaiting a lift. */
  releaseAll(): number;
  /** Includes releases awaiting their local enqueue acknowledgement. */
  heldCount(): number;
}

function makeKeyAuthority(send: SendKey): KeyAuthority {
  const active = new Map<string, HeldKey>();
  const authorities = new Set<HeldKey>();
  const transitionTails = new Map<string, Promise<void>>();

  const schedule = (id: string, transition: () => Promise<boolean>): Promise<boolean> => {
    const prior = transitionTails.get(id) ?? Promise.resolve();
    const submitted = prior.then(transition).catch(() => false);
    const tail = submitted.then(
      () => undefined,
      () => undefined,
    );
    transitionTails.set(id, tail);
    void tail.then(() => {
      if (transitionTails.get(id) === tail) transitionTails.delete(id);
    });
    return submitted;
  };

  const retire = (entry: HeldKey) => {
    authorities.delete(entry);
    if (active.get(entry.id) === entry) active.delete(entry.id);
  };

  const release = (entry: HeldKey) => {
    if (entry.releasePending) return;
    entry.releasePending = true;
    if (active.get(entry.id) === entry) active.delete(entry.id);

    void schedule(entry.id, async () => {
      const downAccepted = await entry.downAccepted;
      if (!downAccepted) {
        retire(entry);
        return true;
      }
      const accepted = await send(entry.routeId, {
        kind: "key",
        key: entry.key,
        code: entry.code,
        down: false,
      }).catch(() => false);
      if (accepted) retire(entry);
      else {
        entry.releasePending = false;
        if (!active.has(entry.id)) active.set(entry.id, entry);
      }
      return accepted;
    });
  };

  return {
    transition(routeId, action, repeat = false) {
      const id = action.code || action.key;
      const prior = active.get(id);

      if (repeat) {
        if (action.down && prior && !MODIFIER_KEYS.has(prior.key)) {
          void schedule(prior.id, () =>
            send(prior.routeId, {
              kind: "key",
              key: prior.key,
              code: prior.code,
              down: true,
            }),
          );
        }
        return;
      }

      if (!action.down) {
        if (prior) release(prior);
        // macOS webviews can swallow non-modifier keyups while Command
        // remains held. When Command lifts, explicitly lift those keys too.
        if (action.key === "Meta") {
          for (const held of [...authorities]) {
            if (!MODIFIER_KEYS.has(held.key)) release(held);
          }
        }
        return;
      }
      if (!routeId) return;

      // A second non-repeat press cannot transfer the first press's authority
      // to another route. Treat the malformed duplicate as cleanup only.
      if (prior) {
        release(prior);
        return;
      }

      const entry: HeldKey = {
        id,
        routeId,
        key: action.key,
        code: action.code,
        downAccepted: Promise.resolve(false),
        releasePending: false,
      };
      active.set(id, entry);
      authorities.add(entry);
      entry.downAccepted = schedule(entry.id, () =>
        send(routeId, {
          kind: "key",
          key: entry.key,
          code: entry.code,
          down: true,
        }).catch(() => false),
      );
      void entry.downAccepted.then((accepted) => {
        if (!accepted && !entry.releasePending) retire(entry);
      });
    },

    releaseAll() {
      const pending = [...authorities].reverse();
      for (const entry of pending) release(entry);
      return pending.length;
    },

    heldCount: () => authorities.size,
  };
}

/**
 * Tracks programmatic key actions, such as the on-screen keyboard, using the
 * same route ownership and acknowledgement rules as physical keys.
 */
export function makeRemoteKeyTracker(send: SendKey): RemoteKeyTracker {
  const authority = makeKeyAuthority(send);
  return {
    onKey: (routeId, action) => authority.transition(routeId, action),
    releaseAll: authority.releaseAll,
    heldCount: authority.heldCount,
  };
}

/**
 * Tracks physical keyboard authority until the matching release has been
 * accepted by the local input FIFO.
 */
export function makeKeyForwarder(send: SendKey): KeyForwarder {
  const authority = makeKeyAuthority(send);
  return {
    onKey(routeId, event, down) {
      authority.transition(
        routeId,
        {
          kind: "key",
          key: event.key,
          code: event.code || undefined,
          down,
        },
        event.repeat,
      );
    },
    releaseAll: authority.releaseAll,
    heldCount: authority.heldCount,
  };
}
