//! What a refusal does, and the four ways a caller might try to get past one:
//! by reconnecting, by asking again, by moving the clock, and by filling the
//! process table until a live limit would have to be evicted.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod rate_limit_support;

use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use roost_coord::middleware::rate_limit::{
    DEFAULT_TOKENS_PER_WINDOW, RATE_LIMIT_WINDOW, RateLimitCaller, RateLimitRefusalReason,
    RateLimiter,
};
use rate_limit_support::{admitted, caller};

#[test]
fn a_reconnecting_caller_finds_the_budget_it_left_and_another_caller_finds_its_own() {
    let limiter = RateLimiter::new();
    let window_start = Instant::now();

    assert_eq!(
        admitted(
            &limiter,
            "PairCreate",
            &caller(9),
            window_start,
            DEFAULT_TOKENS_PER_WINDOW
        ),
        DEFAULT_TOKENS_PER_WINDOW
    );

    // The same caller, arriving on a second connection ten seconds later. A key
    // that followed the connection would hand this a whole fresh budget, and a
    // limit a client can reset by reconnecting never fires.
    let reconnected = RateLimitCaller::from_client_ip("198.51.100.9");
    let refusal = limiter
        .admit_at(
            "PairCreate",
            &reconnected,
            window_start + Duration::from_secs(10),
        )
        .expect("a reconnect does not buy a fresh budget");
    assert_eq!(refusal.reason, RateLimitRefusalReason::Budget);
    assert_eq!(
        refusal.retry_after_seconds, 50,
        "the window it left had 50 seconds left, not a new 60"
    );

    // A different caller on the same process is a different budget: the limiter
    // is per process and per caller, never a global pool one caller drains.
    assert_eq!(
        admitted(
            &limiter,
            "PairCreate",
            &caller(10),
            window_start + Duration::from_secs(10),
            DEFAULT_TOKENS_PER_WINDOW
        ),
        DEFAULT_TOKENS_PER_WINDOW
    );
}

#[test]
fn only_the_first_refusal_of_a_window_is_reported_for_logging() {
    let limiter = RateLimiter::new();
    let blocked = caller(12);
    let window_start = Instant::now();
    assert_eq!(
        admitted(
            &limiter,
            "DevicesRevoke",
            &blocked,
            window_start,
            DEFAULT_TOKENS_PER_WINDOW
        ),
        DEFAULT_TOKENS_PER_WINDOW
    );

    for second in 0..5 {
        let refusal = limiter
            .admit_at(
                "DevicesRevoke",
                &blocked,
                window_start + Duration::from_secs(second),
            )
            .expect("refused");
        assert_eq!(
            refusal.first_in_window,
            second == 0,
            "a client retrying in a loop is one rate_limited line, not one per \
             request"
        );
    }

    // The next window reports again: the log is per window, not per caller.
    let next_window = window_start + RATE_LIMIT_WINDOW;
    assert_eq!(
        admitted(
            &limiter,
            "DevicesRevoke",
            &blocked,
            next_window,
            DEFAULT_TOKENS_PER_WINDOW
        ),
        DEFAULT_TOKENS_PER_WINDOW
    );
    assert!(
        limiter
            .admit_at("DevicesRevoke", &blocked, next_window)
            .expect("refused")
            .first_in_window
    );
}

#[test]
fn a_clock_that_steps_backwards_cannot_hand_out_a_fresh_budget() {
    // The window opens an hour from the current instant, so the step at the end
    // is a rewind of the clock rather than an arithmetic trick.
    let now = Instant::now();
    let window_start = now + Duration::from_secs(3_600);
    let limiter = RateLimiter::new();
    let browser = caller(13);
    assert_eq!(
        admitted(
            &limiter,
            "WorkspacesCreate",
            &browser,
            window_start,
            DEFAULT_TOKENS_PER_WINDOW
        ),
        DEFAULT_TOKENS_PER_WINDOW
    );

    let half_way = window_start + RATE_LIMIT_WINDOW / 2;
    assert_eq!(
        limiter
            .admit_at("WorkspacesCreate", &browser, half_way)
            .expect("spent")
            .retry_after_seconds,
        30
    );

    // The clock steps back an hour. The window is measured on the monotonic
    // clock, so the budget is still spent and the wait is still the window that
    // is left; a wall-clock limiter would hand the caller the hour of skew as a
    // fresh budget, or pin the window shut for the hour of skew.
    let refusal = limiter
        .admit_at("WorkspacesCreate", &browser, now)
        .expect("a rewound clock does not refill a window");
    assert_eq!(refusal.reason, RateLimitRefusalReason::Budget);
    assert_eq!(
        refusal.retry_after_seconds, 60,
        "the backoff never exceeds the window the budget is spent over"
    );
    assert!(!refusal.first_in_window, "the window already reported");
}

#[test]
fn a_clock_that_leaps_forward_refills_exactly_one_window() {
    let limiter = RateLimiter::new();
    let browser = caller(14);
    let window_start = Instant::now();
    assert_eq!(
        admitted(
            &limiter,
            "WorkspacesDelete",
            &browser,
            window_start,
            DEFAULT_TOKENS_PER_WINDOW
        ),
        DEFAULT_TOKENS_PER_WINDOW
    );

    // Ten windows elapsed in one step. The window that was closed is over, and
    // the one that opens now is one window: a leap is not ten refills.
    let after_leap = window_start + Duration::from_secs(600);
    assert_eq!(
        admitted(
            &limiter,
            "WorkspacesDelete",
            &browser,
            after_leap,
            DEFAULT_TOKENS_PER_WINDOW
        ),
        DEFAULT_TOKENS_PER_WINDOW
    );
    assert!(
        limiter
            .admit_at("WorkspacesDelete", &browser, after_leap)
            .is_some()
    );
    assert_eq!(
        limiter.bucket_count(),
        1,
        "the window that elapsed is replaced, not accumulated"
    );
}

#[test]
fn a_full_bucket_table_fails_closed_and_recovers_when_its_windows_elapse() {
    let limiter = RateLimiter::with_max_buckets(NonZeroUsize::new(3).unwrap());
    let window_start = Instant::now();

    for octet in 1..=3 {
        assert!(
            limiter
                .admit_at("WorkspacesCreate", &caller(octet), window_start)
                .is_none()
        );
    }
    assert_eq!(limiter.bucket_count(), 3);

    let churner = caller(4);
    let refusal = limiter
        .admit_at("WorkspacesCreate", &churner, window_start)
        .expect("a full table refuses a caller it has never seen");
    assert_eq!(refusal.reason, RateLimitRefusalReason::AtCapacity);
    assert!(refusal.first_in_window);

    let again = limiter
        .admit_at("WorkspacesCreate", &churner, window_start)
        .expect("still full");
    assert!(
        !again.first_in_window,
        "a churning caller is one capacity report a window, not one per request"
    );
    assert_eq!(
        limiter.bucket_count(),
        3,
        "failing open by evicting a live limit would hand this caller a fresh \
         budget on every attempt"
    );

    // Every window has elapsed, so the table prunes and the churner is served.
    assert!(
        limiter
            .admit_at(
                "WorkspacesCreate",
                &churner,
                window_start + RATE_LIMIT_WINDOW
            )
            .is_none()
    );
    assert_eq!(limiter.bucket_count(), 1);
}
