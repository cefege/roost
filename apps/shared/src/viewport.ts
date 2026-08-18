// Shared viewport / SCD-policy timing. The worker (session-manager.ts
// viewportClaims) and coord (router.ts _viewersBySession) each track viewer
// claims independently and MUST agree on these two timings, or the
// two-sided withdraw hysteresis and TTL reaping desync (one side drops a
// viewer while the other holds it → PTY size ≠ SPA grid). Single source of
// truth so they can't drift. See feedback_viewport_scd_min_policy.

// Defer removing a withdrawn viewer this long so a refresh's re-claim
// cancels it → no SCD-min flap / scrollback re-serialize on a reload.
export const VIEWER_WITHDRAW_GRACE_MS = 800;

// Drop a viewer claim with no heartbeat for this long (dead browser:
// kill -9, WiFi drop, OS sleep). 4× the 30s SPA heartbeat = grace for
// missed beats.
export const VIEWER_CLAIM_TTL_MS = 120_000;

// How often each side sweeps for claims past VIEWER_CLAIM_TTL_MS above. The
// coordinator runs TWO reapers — connect/viewer-tracker.ts for presence and
// connect/cell-subscriptions.ts for the cell fanout — and their own comments
// require them to stay in lockstep: one dropping a claim the other still holds
// is exactly the desync this module exists to prevent.
export const VIEWER_REAP_INTERVAL_MS = 10_000;

// Liveness threshold for SCD-min weighting (A3). A claim not refreshed
// within ~2× the 30s SPA heartbeat is almost certainly a dead viewer and
// MUST NOT constrain the shared PTY size — otherwise a dead phone's tiny
// window clips every live desktop viewer for the full TTL (120s). The TTL
// above stays the hard backstop for actually removing the claim; this is
// the faster "stop letting it shrink everyone" cutoff. 70s = 2×heartbeat
// + 10s slack for one missed beat.
export const VIEWER_CLAIM_FRESH_MS = 70_000;
