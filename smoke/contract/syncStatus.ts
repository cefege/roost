// Type surface of the page's Sync redial state, as
// `window.__smoke.syncRedialStatus()` returns it. Type-only.

/** Is there a Sync socket, and is it carrying traffic? */
export type SyncLinkLiveness = "none" | "dialing" | "open";

export interface SyncRedialStatus {
  /** Consecutive failed dials since this tab last received a Sync frame. */
  readonly failures: number;
  /** Delay the pending redial waits — capped, never unbounded. */
  readonly nextDelayMs: number;
  /** True only while a hidden document sleeps instead of redialing. */
  readonly hiddenParked: boolean;
  /** Whether this tab currently has an open socket, a dial in flight, or none. */
  readonly liveness: SyncLinkLiveness;
}
