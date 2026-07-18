// Backoff + queue-cap tuning knobs for CoordLink.ts. Extracted to keep
// CoordLink.ts under the 400-line cap; imported back by the factory.

export const BACKOFF_INITIAL_MS = 500;
export const BACKOFF_MAX_MS = 30_000;
export const BACKOFF_MULTIPLIER = 2;
// Auth-rejection escalation: when a dial fails without ws.onopen firing
// (upgrade rejected — typically JWT aud mismatch from a stale binary), the
// 30s backoff cap produces ~2,880 retries/day. After this many consecutive
// non-open failures, cap backoff at AUTH_REJECT_BACKOFF_CAP_MS instead,
// cutting the noise 10×. Reset on any successful open.
export const AUTH_REJECT_THRESHOLD = 3;
export const AUTH_REJECT_BACKOFF_CAP_MS = 5 * 60_000;
export const PENDING_CAP = 1024;
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
