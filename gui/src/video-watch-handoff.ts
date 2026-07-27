export const VIDEO_WATCH_HANDOFF_MS = 80;

type TimerHandle = ReturnType<typeof setTimeout>;
type Schedule = (callback: () => void, delayMs: number) => TimerHandle;
type Cancel = (handle: TimerHandle) => void;
type Release = () => void;

interface PendingRelease {
  timer: TimerHandle;
  release: Release;
}

/**
 * Gives a same-route replacement one short local window to register before
 * its predecessor is released. The backend already makes watcher claims
 * atomic, so this preserves the live decoder without changing peer traffic.
 */
export class VideoWatchHandoff {
  private readonly delayMs: number;
  private readonly schedule: Schedule;
  private readonly cancel: Cancel;
  private readonly pending = new Map<string, Map<number, PendingRelease>>();

  constructor(
    delayMs = VIDEO_WATCH_HANDOFF_MS,
    schedule: Schedule = (callback, delay) => setTimeout(callback, delay),
    cancel: Cancel = (handle) => clearTimeout(handle),
  ) {
    this.delayMs = delayMs;
    this.schedule = schedule;
    this.cancel = cancel;
  }

  defer(routeId: string, token: number, release: Release): void {
    if (!Number.isSafeInteger(token) || token <= 0) return;
    let route = this.pending.get(routeId);
    if (!route) {
      route = new Map();
      this.pending.set(routeId, route);
    }
    if (route.has(token)) return;

    const timer = this.schedule(() => {
      const current = this.pending.get(routeId);
      const pending = current?.get(token);
      if (!pending) return;
      current?.delete(token);
      if (current?.size === 0) this.pending.delete(routeId);
      pending.release();
    }, this.delayMs);
    route.set(token, { timer, release });
  }

  /** Call only after the replacement watcher has registered successfully. */
  complete(routeId: string): number {
    const route = this.pending.get(routeId);
    if (!route) return 0;
    this.pending.delete(routeId);
    for (const pending of route.values()) {
      this.cancel(pending.timer);
      pending.release();
    }
    return route.size;
  }

  pendingCount(routeId: string): number {
    return this.pending.get(routeId)?.size ?? 0;
  }
}

export const videoWatchHandoff = new VideoWatchHandoff();
