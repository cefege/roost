//! What a caller's window spends, and what it may not spend on top: the two
//! rates, the reads that spend nothing, and the rpc whose budget is its own.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod rate_limit_support;

use std::time::{Duration, Instant};

use roost_coord::middleware::rate_limit::{
    DEFAULT_TOKENS_PER_WINDOW, PAIR_POLL_TOKENS_PER_WINDOW, RATE_LIMIT_WINDOW, RateLimitBucket,
    RateLimitRefusalReason, RateLimiter, bucket_for_method,
};
use rate_limit_support::{admitted, caller};

#[test]
fn the_hundredth_mutation_of_a_window_is_admitted_and_the_hundred_and_first_is_refused() {
    let limiter = RateLimiter::new();
    let office = caller(7);
    let window_start = Instant::now();

    assert_eq!(
        admitted(
            &limiter,
            "AuthRedeemBrowser",
            &office,
            window_start,
            DEFAULT_TOKENS_PER_WINDOW
        ),
        DEFAULT_TOKENS_PER_WINDOW,
        "the whole window is spendable"
    );

    let refusal = limiter
        .admit_at("AuthRedeemBrowser", &office, window_start)
        .expect("one past the window is refused");
    assert_eq!(refusal.reason, RateLimitRefusalReason::Budget);
    assert_eq!(refusal.bucket, RateLimitBucket::Sensitive);
    assert_eq!(
        refusal.retry_after_seconds, 60,
        "the whole 60 s window is left, and the caller is told so"
    );
    assert!(refusal.first_in_window);

    // The next window is a fresh budget, not a continuation of this one.
    assert!(
        limiter
            .admit_at(
                "AuthRedeemBrowser",
                &office,
                window_start + RATE_LIMIT_WINDOW
            )
            .is_none()
    );
}

#[test]
fn pair_poll_gets_its_own_six_hundred_request_budget() {
    let limiter = RateLimiter::new();
    let waiting_device = caller(60);
    let window_start = Instant::now();

    assert_eq!(
        admitted(
            &limiter,
            "PairPoll",
            &waiting_device,
            window_start,
            PAIR_POLL_TOKENS_PER_WINDOW
        ),
        PAIR_POLL_TOKENS_PER_WINDOW,
        "a device polling for its own approval is expected to poll often"
    );
    let refusal = limiter
        .admit_at("PairPoll", &waiting_device, window_start)
        .expect("601 polls a minute is refused");
    assert_eq!(refusal.bucket, RateLimitBucket::PairPoll);

    // The polling budget belongs to PairPoll alone: the confirm that ends the
    // pairing is an ordinary mutation and keeps the ordinary rate.
    assert_eq!(
        admitted(
            &limiter,
            "PairConfirm",
            &waiting_device,
            window_start,
            DEFAULT_TOKENS_PER_WINDOW
        ),
        DEFAULT_TOKENS_PER_WINDOW
    );
    assert!(
        limiter
            .admit_at("PairConfirm", &waiting_device, window_start)
            .is_some()
    );
}

#[test]
fn a_refused_request_spends_nothing_and_does_not_push_the_window_out() {
    let limiter = RateLimiter::new();
    let scraper = caller(11);
    let window_start = Instant::now();

    assert_eq!(
        admitted(
            &limiter,
            "AuthMintBootstrap",
            &scraper,
            window_start,
            DEFAULT_TOKENS_PER_WINDOW
        ),
        DEFAULT_TOKENS_PER_WINDOW
    );

    // Hammer the closed window. If a refusal spent budget, moved the reset, or
    // reset it, a caller could sit here and probe for the boundary.
    for second in 1..=50_u64 {
        let refusal = limiter
            .admit_at(
                "AuthMintBootstrap",
                &scraper,
                window_start + Duration::from_secs(second),
            )
            .expect("the window is still closed");
        assert_eq!(
            refusal.retry_after_seconds,
            60 - second,
            "the window still ends when it was opened to end"
        );
    }

    // Exactly one window after the first request, the budget is whole again.
    assert_eq!(
        admitted(
            &limiter,
            "AuthMintBootstrap",
            &scraper,
            window_start + RATE_LIMIT_WINDOW,
            DEFAULT_TOKENS_PER_WINDOW
        ),
        DEFAULT_TOKENS_PER_WINDOW,
        "50 refusals bought no extra time and no extra tokens"
    );
}

#[test]
fn a_read_neither_opens_a_window_nor_spends_a_mutations_budget() {
    let limiter = RateLimiter::new();
    let browser = caller(15);
    let window_start = Instant::now();

    for _ in 0..1_000 {
        assert!(
            limiter
                .admit_at("WorkspacesList", &browser, window_start)
                .is_none()
        );
        assert!(
            limiter
                .admit_at("UiListStates", &browser, window_start)
                .is_none()
        );
    }
    assert_eq!(
        limiter.bucket_count(),
        0,
        "a read that opened a window would let SPA bootstrap traffic fill the \
         process table and push real callers into the capacity refusal"
    );

    assert_eq!(
        admitted(
            &limiter,
            "WorkspacesCreate",
            &browser,
            window_start,
            DEFAULT_TOKENS_PER_WINDOW
        ),
        DEFAULT_TOKENS_PER_WINDOW,
        "a thousand reads did not spend one token of the mutation's budget"
    );
    assert!(
        limiter
            .admit_at("WorkspacesCreate", &browser, window_start)
            .is_some()
    );
}

#[test]
fn the_entry_point_the_mount_calls_answers_on_its_own_clock() {
    // Every other test here drives `admit_at` so a window is reachable exactly.
    // This one is the call `http::listener` makes, with no instant threaded in,
    // so the one path a test cannot see is the one the mount actually walks.
    let limiter = RateLimiter::new();
    let browser = caller(16);

    for _ in 0..DEFAULT_TOKENS_PER_WINDOW {
        assert!(limiter.admit("AuthRedeemBrowser", &browser).is_none());
    }
    assert!(
        limiter.admit("AuthRedeemBrowser", &browser).is_some(),
        "100 requests inside one 60 s window, then the 101st is refused"
    );
    assert!(
        limiter.admit("WorkspacesList", &browser).is_none(),
        "a read is admitted whatever the mutation budget says"
    );
}


#[test]
fn a_budget_is_chosen_per_rpc_and_never_inherited_by_a_sibling() {
    assert_eq!(
        bucket_for_method("PairPoll"),
        Some(RateLimitBucket::PairPoll)
    );
    assert_eq!(
        bucket_for_method("PairApprove"),
        Some(RateLimitBucket::Sensitive)
    );

    for read in [
        "WorkspacesList",
        "TasksNextPending",
        "TranscriptionGetConfig",
        "UiReportState",
        "UiListStates",
        "WorkersRegister",
        "MiscHealth",
        "AuthCoordIdentity",
    ] {
        assert_eq!(
            bucket_for_method(read),
            None,
            "{read} is a read, or a fixed-cadence sender the operator does not \
             throttle; a bucket on it is spent before the user acts"
        );
    }
}
