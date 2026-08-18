// Browser-side liveness policy for the Sync WebSocket: the stale-link watchdog
// plus the redial/park decisions the reconnect loop asks for. Mirrors the
// worker↔coord watchdog (`apps/worker/src/transport/coord-link.ts`'s
// stale-link watchdog in `dial()`, tuned by `coord-link-constants.ts`). When
// the coord process dies behind tailscale-serve the browser-side TCP stays
// ESTABLISHED and ws.onclose/onerror never fire — the reconnect loop sits
// `await closed` forever, no auto-reload, terminal frozen until a manual page
// refresh. This watchdog force-closes the socket after sustained silence so
// onclose fires → the loop re-dials with sinceEventId → backfill → recovery.
//
// Only acts while the tab is foregrounded: a hidden tab's setInterval is
// throttled and unreliable. The hidden→visible transition is owned by
// shouldRedialOnRefocus below, which the page-lifecycle wake (sync.ts) consults
// instead of re-dialing unconditionally.
//
// Every duration here is measured on performance.now(): a tab resumed after a
// freeze/suspend often comes back to a corrected wall clock, and a Date.now()
// jump would either hide a dead socket or fake a stale one.

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

// ─── redial policy ────────────────────────────────────────────────────────────
// A visible document with no live socket ALWAYS has a redial scheduled. Only the
// DELAY is bounded; the attempt count never is. An attempt cap can only be
// cleared by a reload, which is the failure this policy exists to remove.

export const SYNC_REDIAL_BASE_MS = 1_000;
export const SYNC_REDIAL_MAX_MS = 30_000;
/** Consecutive failures at which the delay saturates: 1s 2s 4s 8s 16s 30s. */
export const SYNC_REDIAL_SATURATION_FAILURES = 6;
/** Consecutive failures a HIDDEN document tolerates before it sleeps instead of
 *  dialing on a throttled timer (~60s of accumulated backoff, the budget the
 *  retired permanent park used). Any page-lifecycle resume wakes it at once. */
export const SYNC_HIDDEN_PARK_FAILURES = 8;

/** Capped backoff for the Nth consecutive failed dial. */
export function nextRedialDelayMs(failures: number): number {
  const steps = Math.min(Math.max(failures - 1, 0), SYNC_REDIAL_SATURATION_FAILURES);
  return Math.min(SYNC_REDIAL_BASE_MS * 2 ** steps, SYNC_REDIAL_MAX_MS);
}

/** May the redial loop sleep instead of dialing? Only a hidden document, and
 *  only past the failure budget above: nobody is watching it, and its throttled
 *  timers make a dial-per-30s pointless. A visible document never parks. */
export function shouldParkRedial(failures: number, visible = isPageVisible()): boolean {
  return !visible && failures >= SYNC_HIDDEN_PARK_FAILURES;
}

/** Is there a Sync socket, and is it carrying traffic? */
export type SyncLinkLiveness = "none" | "dialing" | "open";

/** Resume decision: close only a socket that is OPEN and has gone silent past
 *  the refocus budget. A dial already in flight IS the redial — closing it would
 *  discard that generation and immediately start a second one. */
export function shouldCloseStaleLinkOnResume(
  liveness: SyncLinkLiveness,
  idleMs: number,
): boolean {
  return liveness === "open" && shouldRedialOnRefocus(idleMs);
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
  let lastMsgAt = performance.now();
  const idleMs = (): number => performance.now() - lastMsgAt;
  const onMessage = (): void => { lastMsgAt = performance.now(); };
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
