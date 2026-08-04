// Browser-side stale-link watchdog for the Sync WebSocket. Mirrors the
// worker↔coord watchdog (CoordLink.ts:248-265, CoordLink-constants.ts). When
// the coord process dies behind tailscale-serve the browser-side TCP stays
// ESTABLISHED and ws.onclose/onerror never fire — the reconnect loop sits
// `await closed` forever, no auto-reload, terminal frozen until a manual page
// refresh. This watchdog force-closes the socket after sustained silence so
// onclose fires → the loop re-dials with sinceEventId → backfill → recovery.
//
// Only acts while the tab is foregrounded: a hidden tab's setInterval is
// throttled and unreliable. The hidden→visible transition is owned by
// shouldRedialOnRefocus below, which the refocus handler (sync-bootstrap.ts)
// consults instead of re-dialing unconditionally.

import { isPageVisible } from "../lib/pageVisible.ts";

export const SYNC_STALE_TIMEOUT_MS = 90_000;   // 3 missed 30s keepalives
export const SYNC_STALE_CHECK_MS = 15_000;

/** Idle budget a refocused tab tolerates before re-dialing. 1.5× coord's 30s
 *  Sync keepalive (sync-ws-handler.ts): one dropped keepalive must not force a
 *  re-dial, a suspended or half-open socket must. */
export const SYNC_REFOCUS_STALE_MS = 45_000;
/** Pure decision for the refocus handler — tested in sync-watchdog.test.ts. */
export function shouldRedialOnRefocus(idleMs: number): boolean {
  return idleMs > SYNC_REFOCUS_STALE_MS;
}

export interface StaleWatchdogOpts {
  staleMs?: number;
  checkMs?: number;
  isVisible?: () => boolean;   // default isPageVisible
  onStale?: () => void;        // fired BEFORE close (caller sets abort reason)
}

export interface StaleWatchdog {
  stop(): void;
  idleMs(): number;
}

/** Starts a foreground stale-link watchdog with generation-local liveness. */
export function startStaleWatchdog(
  ws: WebSocket,
  opts: StaleWatchdogOpts = {},
): StaleWatchdog {
  const staleMs = opts.staleMs ?? SYNC_STALE_TIMEOUT_MS;
  const checkMs = opts.checkMs ?? SYNC_STALE_CHECK_MS;
  const isVisible = opts.isVisible ?? isPageVisible;
  let lastMsgAt = Date.now();
  const idleMs = (): number => Date.now() - lastMsgAt;
  const onMessage = (): void => { lastMsgAt = Date.now(); };
  ws.addEventListener("message", onMessage);
  const timer = setInterval(() => {
    if (idleMs() < staleMs) return;
    if (!isVisible()) return;
    opts.onStale?.();
    try { ws.close(); } catch { /* already closing */ }
  }, checkMs);
  return {
    stop(): void {
      clearInterval(timer);
      ws.removeEventListener("message", onMessage);
    },
    idleMs,
  };
}
