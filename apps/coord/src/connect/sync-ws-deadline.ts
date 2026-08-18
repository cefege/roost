// Reauth deadline for the browser Sync WebSocket. A public-surface socket
// authenticated by a Cloudflare Access assertion carries a hard expiry; this
// arms close(4003) at that instant and RE-ARMS across the platform timer
// maximum, so a deadline further out than ~24.8 days is never silently dropped.
//
// Split out of sync-ws-handler.ts: pure timing behind an injectable clock,
// pinned by apps/coord/tests/sync-ws-deadline.test.ts.

import type { ServerWebSocket } from "bun";
import type { SyncWsData } from "./sync-ws-handler.ts";

const MAX_TIMER_DELAY_MS = 2_147_483_647;

export interface SyncDeadlineClock {
  now(): number;
  setTimeout(callback: () => void, delayMs: number): Timer;
  clearTimeout(timer: Timer): void;
  maxDelayMs?: number;
}
export interface SyncDeadlineTimer {
  current: Timer | null;
}


export const realDeadlineClock: SyncDeadlineClock = {
  now: Date.now,
  setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
  clearTimeout: (timer) => clearTimeout(timer),
};

export function scheduleDeadline(
  ws: Pick<ServerWebSocket<SyncWsData>, "close">,
  deadlineMs: number,
  clock: SyncDeadlineClock = realDeadlineClock,
): SyncDeadlineTimer {
  const handle: SyncDeadlineTimer = { current: null };
  const arm = (): void => {
    handle.current = clock.setTimeout(() => {
      const remaining = deadlineMs - clock.now();
      if (remaining <= 0) {
        handle.current = null;
        ws.close(4003, "reauth required");
        return;
      }
      arm();
    }, Math.min(
      Math.max(0, deadlineMs - clock.now()),
      clock.maxDelayMs ?? MAX_TIMER_DELAY_MS,
    ));
  };
  arm();
  return handle;
}
