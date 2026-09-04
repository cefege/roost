// Hard authentication deadline shared by long-lived browser Sync and worker
// WebSockets. Re-arms across the platform timer maximum, so a deadline further
// out than ~24.8 days is never silently dropped.
//
// Pure timing behind an injectable clock, pinned by
// apps/coord/tests/ws-auth-deadline.test.ts.

const MAX_TIMER_DELAY_MS = 2_147_483_647;

export interface WsDeadlineClock {
  now(): number;
  setTimeout(callback: () => void, delayMs: number): Timer;
  clearTimeout(timer: Timer): void;
  maxDelayMs?: number;
}

export interface WsAuthDeadlineTimer {
  current: Timer | null;
}

export interface WsAuthDeadlineSocket {
  close(code?: number, reason?: string): void;
}

export const realWsDeadlineClock: WsDeadlineClock = {
  now: Date.now,
  setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
  clearTimeout: (timer) => clearTimeout(timer),
};

export function scheduleWsAuthDeadline(
  ws: WsAuthDeadlineSocket,
  deadlineMs: number,
  clock: WsDeadlineClock = realWsDeadlineClock,
): WsAuthDeadlineTimer {
  const handle: WsAuthDeadlineTimer = { current: null };
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
