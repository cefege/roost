//! The per-route, per-client request budget: 100 requests a minute for every
//! listed RPC, 600 for `PairPoll` alone.
//!
//! Reached as `core.services.rate_limit`. The bucket table is process state
//! rather than listener state for the reason `apps/coord/src/middleware/
//! rate-limit.ts` gives: the budget is spent by a client, and a client that
//! reconnects must not get a fresh one.
//!
//! `new()` takes nothing and must keep taking nothing. The route list and the
//! window are v2's constants; anything operator-configured is read at call time
//! from `core.services.boot`.

/// The request budget one coordinator process enforces.
#[derive(Debug, Default)]
pub struct RateLimiter;

impl RateLimiter {
    /// A limiter with no bucket spent yet.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}
