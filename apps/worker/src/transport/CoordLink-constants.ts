// Backoff + queue-cap tuning knobs for CoordLink.ts. Extracted to keep
// CoordLink.ts under the 400-line cap; imported back by the factory.

export const BACKOFF_INITIAL_MS = 500;
export const BACKOFF_MAX_MS = 30_000;
export const BACKOFF_MULTIPLIER = 2;
// Auth-rejection escalation: when a dial fails without ws.onopen firing, the
// 30s backoff cap produces ~2,880 retries/day. After this many consecutive
// non-open failures, cap backoff at AUTH_REJECT_BACKOFF_CAP_MS instead,
// cutting the noise 10×. Reset on any successful open.
export const AUTH_REJECT_THRESHOLD = 3;
export const AUTH_REJECT_BACKOFF_CAP_MS = 5 * 60_000;
// A dial that never fires ws.onopen is NOT necessarily an auth rejection: coord
// answers a bad JWT with an HTTP 401 on the upgrade, which Bun's client
// WebSocket reports exactly like a timeout or a tailscale-serve 502. So a
// worker throttled by its own cgroup used to escalate to the 5-min cap after 3
// dials and stay invisible for minutes (2026-08-01, ovh1). A worker that has
// never opened in this process is the real stale-binary case and still
// escalates after 3; once a link has opened, only a long streak escalates, so
// a transient stall costs one 30s cap instead of 5 minutes.
export const AUTH_REJECT_THRESHOLD_AFTER_OPEN = 60;

/** Reconnect backoff ceiling. Pure so the escalation rule is unit-testable
 *  without driving 60 real dials. */
export function backoffCapMs(nonOpenStreak: number, hasOpened: boolean): number {
  const threshold = hasOpened ? AUTH_REJECT_THRESHOLD_AFTER_OPEN : AUTH_REJECT_THRESHOLD;
  return nonOpenStreak >= threshold ? AUTH_REJECT_BACKOFF_CAP_MS : BACKOFF_MAX_MS;
}
export const PENDING_CAP = 1024;
// Pending frames are encoded before admission so the byte cap is exact rather
// than an estimate over mutable proto objects.
export const PENDING_BYTES_CAP = 8 * 1024 * 1024;
// Leave headroom below the pending cap for native WebSocket buffering. Bun
// exposes the browser-compatible bufferedAmount counter but no drain event, so
// CoordLink samples it on a short bounded timer while work is waiting.
export const WS_BUFFERED_HIGH_WATER_BYTES = 4 * 1024 * 1024;
export const WS_DRAIN_RETRY_MS = 4;
// Raw PTY bytes feed coordinator-only metadata scanners. Cells normally go
// first, but an old metadata frame is promoted so it cannot starve forever.
export const RAW_METADATA_MAX_AGE_MS = 100;
// Minimum stream uptime before the dial counters reset to 0. A
// helloAck-then-immediate-drop pattern would otherwise cycle
// attempt:1 forever, hiding a coord flap pathology from telemetry.
export const STABLE_SESSION_MS = 30_000;
// D-4b unacked cap. Bounds worker memory if coord is down or wedged
// for hours (claude streaming bursts ~10 ev/s = 36k entries/hour).
// On overflow, evict oldest with log.error — coord's next snapshot
// reconciliation on reconnect catches the gap.
export const UNACKED_CAP = 8192;
// Stale-link watchdog. Coord pings every 30s (coord worker-conn.ts keepalive),
// so a healthy open link never goes >30s without a downstream frame. When the
// coord process dies behind tailscale serve, the worker-side TCP stays
// ESTABLISHED and ws.send keeps "succeeding" into a black hole — onerror/
// onclose never fire (2026-07-11: 7h zombie link; every spawn failed with
// [failed_precondition] worker not connected). Force-close + re-dial after
// 3 missed pings. Same half-open-through-tailscale class as install.ts
// BOOT_RPC_TIMEOUT_MS.
export const STALE_LINK_TIMEOUT_MS = 90_000;
export const STALE_CHECK_INTERVAL_MS = 15_000;

// Downstream terminal-control admission. Input and viewport hold independent
// budgets so a viewport RPC parked on a keeper resize can never consume the
// slots PTY input needs; a viewport flood cannot starve typing and vice versa.
// Over-cap admission fails closed with a provable pre-write rejection rather
// than a silent drop, so the coordinator learns immediately instead of waiting
// out its deadline and calling a definite refusal "unknown".
export const INPUT_REQUEST_INFLIGHT_CAP = 256;
export const VIEWPORT_REQUEST_INFLIGHT_CAP = 64;
// Ceiling on the coordinator-supplied relative budget, and the budget used
// when a peer supplies none, so one downstream request can never park an
// admission slot indefinitely.
export const TERMINAL_REQUEST_BUDGET_CAP_MS = 30_000;
