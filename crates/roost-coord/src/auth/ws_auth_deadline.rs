//! The hard authentication deadline a long-lived WebSocket closes at: `4003
//! reauth required` once the verified credential's deadline passes.
//!
//! Called by the Sync socket loop (`sync_ws::socket`), which races
//! [`sleep_until_reauth`] against its socket. The worker link arms no deadline:
//! v2 never schedules one there. Ports `apps/coord/src/auth/ws-auth-deadline.ts`.
//!
//! THE WAIT IS RE-CHECKED, NOT TRUSTED. v2 arms `min(deadline - now, 2^31 - 1)`
//! and re-arms on every fire, because a platform timer silently truncates a
//! longer delay. The port keeps the re-check for a second reason: the wait is
//! slept on the monotonic clock but the deadline is a wall-clock instant, and
//! only a re-read of the wall clock says whether the two still agree.

use std::time::Duration;

/// The close code a socket whose credential outlived its deadline closes with.
pub const REAUTH_CLOSE_CODE: u16 = 4003;

/// The close reason beside [`REAUTH_CLOSE_CODE`].
pub const REAUTH_CLOSE_REASON: &str = "reauth required";

/// The longest single wait before the deadline is re-read
/// (`ws-auth-deadline.ts:8`, the 32-bit platform timer maximum).
pub const MAX_TIMER_DELAY_MS: u64 = 2_147_483_647;

/// Whether a socket whose deadline is `deadline_ms` must close at `now_ms`.
///
/// Inclusive, as v2's `remaining <= 0` and the open-time `reauthAtMs <= now`
/// both are: a credential is not acceptable AT its deadline.
#[must_use]
pub const fn reauth_expired(deadline_ms: i64, now_ms: i64) -> bool {
    deadline_ms <= now_ms
}

/// How long to wait before re-reading the clock, or `None` once the deadline
/// has passed.
#[must_use]
pub fn reauth_wait_ms(deadline_ms: i64, now_ms: i64, max_delay_ms: u64) -> Option<u64> {
    if reauth_expired(deadline_ms, now_ms) {
        return None;
    }
    let remaining = u64::try_from(deadline_ms.saturating_sub(now_ms)).unwrap_or(u64::MAX);
    Some(remaining.min(max_delay_ms))
}

/// Resolve once `deadline_ms` has passed on the wall clock.
///
/// The caller races this against its socket and closes
/// [`REAUTH_CLOSE_CODE`] / [`REAUTH_CLOSE_REASON`] when it wins. Dropping the
/// future is the cancellation: an ordinary close leaves no timer behind.
pub async fn sleep_until_reauth(deadline_ms: i64) {
    sleep_until_reauth_on(
        deadline_ms,
        MAX_TIMER_DELAY_MS,
        &crate::rpc::service::now_ms,
    )
    .await;
}

/// [`sleep_until_reauth`] over an injected clock and timer maximum, so the
/// re-arm is testable under a paused runtime. The clock is `Sync` because the
/// future borrows it across an await, and a `Send` future may only hold a
/// shared borrow of a `Sync` value.
pub async fn sleep_until_reauth_on(
    deadline_ms: i64,
    max_delay_ms: u64,
    clock: &(dyn Fn() -> i64 + Sync),
) {
    while let Some(wait_ms) = reauth_wait_ms(deadline_ms, clock(), max_delay_ms) {
        tokio::time::sleep(Duration::from_millis(wait_ms)).await;
    }
}
