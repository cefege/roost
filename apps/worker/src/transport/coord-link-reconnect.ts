// Reconnect ladder for coord-link.ts: the exponential-backoff dial timer plus
// the dial/open counters the backoff cap is derived from. Extracted from
// coord-link.ts as pure code motion to keep both files under the 400-line cap.
//
// A worker is a daemon, so nothing here ever gives up — the ceiling only makes
// a wedged worker observable. Every counter reset below is load-bearing for
// exactly one scenario, named at the reset.

import { signal } from "@roost/shared/diag";
import { log } from "@roost/shared/log";
import {
  BACKOFF_INITIAL_MS, BACKOFF_MULTIPLIER,
  AUTH_REJECT_THRESHOLD, AUTH_REJECT_BACKOFF_CAP_MS,
  AUTH_REJECT_THRESHOLD_AFTER_OPEN, backoffCapMs,
} from "./coord-link-constants.ts";
import type { CoordLinkReconnect, CoordLinkReconnectHooks } from "./coord-link-types.ts";

// Consecutive dial failures since the last successful open. Reset to 0
// in ws.onopen; a worker is a daemon so we NEVER stop reconnecting —
// crossing the ceiling only fires an observability signal (once, then
// cooldown-gated) so a wedged worker is visible in `roost doctor`.
const RECONNECT_GIVE_UP_AFTER = 10;
let _reconnectFailures = 0;

export function createCoordLinkReconnect(hooks: CoordLinkReconnectHooks): CoordLinkReconnect {
  let backoffMs = BACKOFF_INITIAL_MS;
  // Monotonic dial counter. Stamped onto every `connecting` state
  // transition so consumers of state().attempt (telemetry, health UI)
  // can distinguish a wedged worker (attempt: 47) from a healthy one
  // (attempt: 1). Reset to 0 once a stream successfully opens; we
  // pre-increment so the first attempt reports attempt: 1.
  let dialAttempt = 0;
  // Consecutive dials that failed without ws.onopen firing. Not necessarily
  // auth: a 401 upgrade, a handshake timeout and a proxy 502 look identical
  // from the close event. backoffCapMs() decides when a streak escalates.
  let _authRejectCount = 0;
  let _didOpen = false;
  // Persists across dials so callers can reconcile only after a true reopen,
  // not race normal startup with a duplicate snapshot request.
  let hasOpened = false;
  // Pending backoff dial. Held so relocate()/dispose() can cancel it — an
  // uncancelled timer means a second concurrent socket.
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;

  function beginDial(): number {
    dialAttempt += 1;
    _didOpen = false;
    return dialAttempt;
  }

  function noteOpen(): boolean {
    _reconnectFailures = 0;
    _didOpen = true;
    const reconnected = hasOpened;
    hasOpened = true;
    _authRejectCount = 0;
    return reconnected;
  }

  function noteDialClosed(): void {
    // A dial that never fired ws.onopen is not necessarily an auth
    // rejection — a 401 upgrade, a handshake timeout and a proxy 502 are
    // indistinguishable here. Just count the streak; scheduleReconnect
    // decides what it means (see backoffCapMs).
    if (!_didOpen) _authRejectCount++;
  }

  /** Reset dial counters only once the session is demonstrably useful:
   * >=1 frame received AND >=STABLE_SESSION_MS uptime — distinguishes a
   * healthy long session from a flap. The caller owns the uptime test. */
  function noteStableSession(): void {
    backoffMs = BACKOFF_INITIAL_MS;
    dialAttempt = 0;
  }

  function resetForRedial(): void {
    backoffMs = BACKOFF_INITIAL_MS;
    // A worker auth-rejected by the source would otherwise carry a 5-minute
    // backoff cap into the healthy target and sit offline for minutes.
    dialAttempt = 0;
    _authRejectCount = 0;
  }

  function cancelPendingDial(): void {
    if (reconnectTimer !== null) { clearTimeout(reconnectTimer); reconnectTimer = null; }
  }

  function scheduleReconnect(): void {
    if (hooks.isDisposed()) return;
    // A worker is a daemon: we retry forever. Crossing the ceiling only
    // makes the wedged-worker anomaly visible (cooldown-gated to once/10s);
    // retry behavior is unchanged.
    _reconnectFailures += 1;
    if (_reconnectFailures >= RECONNECT_GIVE_UP_AFTER) {
      signal("reconnect.give_up", { failures: _reconnectFailures, action: "keep_retrying", cooldownKey: "coordlink" });
    }
    // Escalate backoff for sustained non-open streaks (e.g. stale binary with
    // wrong JWT aud, which never opens at all). A link that HAS opened in this
    // process needs a far longer streak, so a transient stall — a throttled
    // worker, a proxy 502 — costs one 30s cap instead of 5 minutes.
    const _cap = backoffCapMs(_authRejectCount, hasOpened);
    const _threshold = hasOpened ? AUTH_REJECT_THRESHOLD_AFTER_OPEN : AUTH_REJECT_THRESHOLD;
    if (_cap === AUTH_REJECT_BACKOFF_CAP_MS && _authRejectCount === _threshold) {
      log.warn("coord-link", "reconnect_backoff_escalated", {
        count: _authRejectCount, backoffMs: _cap, had_opened: hasOpened,
      });
    }
    const nextDialAtMs = Date.now() + backoffMs;
    hooks.setState({ kind: "reconnecting", nextDialAtMs, backoffMs });
    const d = backoffMs;
    backoffMs = Math.min(backoffMs * BACKOFF_MULTIPLIER, _cap);
    reconnectTimer = setTimeout(() => {
      reconnectTimer = null;
      if (!hooks.isDisposed()) hooks.dial();
    }, d);
  }

  return {
    scheduleReconnect, cancelPendingDial, beginDial, noteOpen,
    noteDialClosed, noteStableSession, resetForRedial,
  };
}
