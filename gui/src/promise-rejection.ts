export type RejectionEventLike = {
  reason: unknown;
  preventDefault(): void;
};

/**
 * Keep an asynchronous command failure from replacing the production UI.
 * Development builds retain the full-screen diagnostic because a developer can
 * act on it immediately. Production records the rejection and leaves the app
 * usable while the command-level handler reports recoverable failures.
 */
export function handleUnhandledRejection(
  event: RejectionEventLike,
  development: boolean,
  showFatal: (label: string, reason: unknown) => void,
  report: (label: string, reason: unknown) => void = console.error,
): void {
  event.preventDefault();
  report("Unhandled promise rejection", event.reason);
  if (development) showFatal("Unhandled promise rejection", event.reason);
}
