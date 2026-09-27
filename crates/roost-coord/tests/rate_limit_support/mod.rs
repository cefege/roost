//! The caller and the request loop the rate-limit tests share.
//!
//! Used by `rate_limit_budget.rs` (what a window spends) and
//! `rate_limit_refusals.rs` (what a refusal does). Nothing here reads a clock
//! of its own: every call is told the instant, which is the seam
//! `RateLimiter::admit_at` exists to provide.

use std::time::Instant;

use roost_coord::middleware::rate_limit::{RateLimitCaller, RateLimiter};

/// A caller on the documentation range, standing in for one browser or worker.
#[must_use]
pub fn caller(last_octet: u8) -> RateLimitCaller {
    RateLimitCaller::from_client_ip(format!("198.51.100.{last_octet}"))
}

/// How many of `attempts` requests arriving at the same instant were admitted.
pub fn admitted(
    limiter: &RateLimiter,
    method: &str,
    caller: &RateLimitCaller,
    now: Instant,
    attempts: u32,
) -> u32 {
    let mut count = 0;
    for _ in 0..attempts {
        if limiter.admit_at(method, caller, now).is_none() {
            count += 1;
        }
    }
    count
}
