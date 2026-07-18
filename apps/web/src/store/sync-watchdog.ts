// Browser-side stale-link watchdog for the Sync WebSocket. Mirrors the
// worker↔coord watchdog (CoordLink.ts:248-265, CoordLink-constants.ts). When
// the coord process dies behind tailscale-serve the browser-side TCP stays
// ESTABLISHED and ws.onclose/onerror never fire — the reconnect loop sits
// `await closed` forever, no auto-reload, terminal frozen until a manual page
// refresh. This watchdog force-closes the socket after sustained silence so
// onclose fires → the loop re-dials with sinceEventId → backfill → recovery.
//
// Only acts while the tab is foregrounded: Chrome's background-tab throttle is
// already handled by the visibilitychange → _abortSyncForVisibility path
// (sync-bootstrap.ts). A hidden tab relies on that path, not this watchdog.

import { isPageVisible } from "../lib/pageVisible.ts";

export const SYNC_STALE_TIMEOUT_MS = 90_000;   // 3 missed 30s keepalives
export const SYNC_STALE_CHECK_MS = 15_000;

export interface StaleWatchdogOpts {
  staleMs?: number;
  checkMs?: number;
  isVisible?: () => boolean;   // default isPageVisible
  onStale?: () => void;        // fired BEFORE close (caller sets abort reason)
}

/** Starts a setInterval that force-closes `ws` when no WS message has arrived
 *  for `staleMs` AND the tab is visible. The visibility handler
 *  (sync-bootstrap.ts::_abortSyncForVisibility) owns background→foreground
 *  recovery; this watchdog owns foreground half-open stalls. Returns stop(). */
export function startStaleWatchdog(ws: WebSocket, opts: StaleWatchdogOpts = {}): () => void {
  const staleMs = opts.staleMs ?? SYNC_STALE_TIMEOUT_MS;
  const checkMs = opts.checkMs ?? SYNC_STALE_CHECK_MS;
  const isVisible = opts.isVisible ?? isPageVisible;
  let lastMsgAt = Date.now();
  const onMessage = (): void => { lastMsgAt = Date.now(); };
  ws.addEventListener("message", onMessage);
  const timer = setInterval(() => {
    if (Date.now() - lastMsgAt < staleMs) return;
    if (!isVisible()) return;            // hidden → visibility handler owns it
    opts.onStale?.();
    try { ws.close(); } catch { /* already closing */ }
  }, checkMs);
  return () => { clearInterval(timer); ws.removeEventListener("message", onMessage); };
}
