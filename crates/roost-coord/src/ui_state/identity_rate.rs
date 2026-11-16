//! The seam the new-tab-identity budget is admitted through.
//!
//! The budget is `UI_STATE_NEW_IDENTITIES_PER_WINDOW` per fingerprint per
//! window. The UI owner takes the limiter as a trait with an allow-all value,
//! rather than counting in a second place: two limiters over one budget is a
//! budget nobody can reason about. Called by `state_owner`, which accepts a
//! shared limiter through `with_identity_rate_limiter`.

/// One fixed-window budget decision.
///
/// `capacity` and `window_ms` are passed per call rather than captured, because
/// the coordinator raises the new-identity budget for a deployment that reports
/// from many tabs and must not need a second limiter type to say so.
pub trait IdentityRateLimiter: Send + Sync {
    /// Whether one identity introduction may proceed, recorded against
    /// `scope` (the fingerprint) in `group`.
    fn consume(
        &self,
        scope: &str,
        group: &str,
        capacity: usize,
        window_ms: i64,
        now_ms: i64,
    ) -> bool;
}

/// The limiter a coordinator runs with unless one is attached through
/// `with_identity_rate_limiter`.
///
/// It admits everything, and that is a real difference from a budgeted
/// limiter: a paired device may introduce unbounded distinct tab ids. The
/// cardinality caps in `state_owner` still bound how many of those are
/// RETAINED, which is the memory-shaped half of the problem; the identity
/// window is the half this seam hands to a shared limiter rather than
/// reimplementing.
#[derive(Debug, Clone, Copy, Default)]
pub struct AllowEveryIdentityRateLimiter;

impl IdentityRateLimiter for AllowEveryIdentityRateLimiter {
    fn consume(
        &self,
        _scope: &str,
        _group: &str,
        _capacity: usize,
        _window_ms: i64,
        _now_ms: i64,
    ) -> bool {
        true
    }
}
