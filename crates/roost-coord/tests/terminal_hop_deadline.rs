//! The coordinator half of the terminal hop budget: the worker is handed a
//! relative slice strictly smaller than what the coordinator still waits, the
//! slice is refused below its floor, and only monotonic elapsed time spends it.
//!
//! Ports the budget rules of `apps/coord/tests/terminal/terminal-hop-deadline.test.ts`
//! (`workers::hop_deadline`). Time is tokio's paused clock, so every boundary is
//! asserted exactly rather than raced. `unwrap`/`expect` are denied outside
//! `#[cfg(test)]`; an integration test is its own crate, hence the allow.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use roost_coord::workers::hop_deadline::{HopDeadline, INPUT_CONTROL_TIMEOUT_MS, worker_budget_ms};

// v2 "a healthy remaining budget still sends, and the worker slice is strictly
// smaller": the worker gets what is left less the return-trip reserve.
#[tokio::test(start_paused = true)]
async fn the_worker_slice_is_the_remaining_budget_less_the_return_trip_reserve() {
    let input = HopDeadline::start(INPUT_CONTROL_TIMEOUT_MS);
    assert_eq!(worker_budget_ms(&input), Some(4_250));

    tokio::time::advance(Duration::from_millis(1_000)).await;
    assert_eq!(
        worker_budget_ms(&input),
        Some(3_250),
        "a lane wait spends the budget before the send, not after it"
    );
}

// v2 "a budget too short to survive the hop is refused rather than half-spent":
// 1000 ms left is exactly the reserve plus the smallest usable slice.
#[tokio::test(start_paused = true)]
async fn a_slice_below_the_floor_is_refused_and_the_remaining_time_is_floored() {
    let deadline = HopDeadline::start(INPUT_CONTROL_TIMEOUT_MS);
    tokio::time::advance(Duration::from_millis(4_000)).await;
    assert_eq!(
        worker_budget_ms(&deadline),
        Some(250),
        "the floor itself sends"
    );

    tokio::time::advance(Duration::from_micros(500)).await;
    assert_eq!(
        worker_budget_ms(&deadline),
        None,
        "999.5 ms left floors to 999, one under the floor"
    );

    // v2's own example: 800 ms left cannot cover the reserve plus a slice.
    let short = HopDeadline::start(INPUT_CONTROL_TIMEOUT_MS);
    tokio::time::advance(Duration::from_millis(4_200)).await;
    assert!((short.remaining_ms() - 800.0).abs() < f64::EPSILON);
    assert_eq!(worker_budget_ms(&short), None);
}

// v2 "an expired budget stays expired no matter what the wall clock says".
#[tokio::test(start_paused = true)]
async fn an_expired_budget_reports_negative_time_and_never_a_slice() {
    let deadline = HopDeadline::start(INPUT_CONTROL_TIMEOUT_MS);
    tokio::time::advance(Duration::from_millis(5_005)).await;
    assert!(
        (deadline.remaining_ms() + 5.0).abs() < f64::EPSILON,
        "remaining time goes negative rather than clamping to a fresh budget"
    );
    assert_eq!(worker_budget_ms(&deadline), None);
    assert_eq!(deadline.total_ms(), INPUT_CONTROL_TIMEOUT_MS);
}

// v2 "a wall clock stepped hours in either direction leaves the verdict intact":
// real time passing while the monotonic clock stands still spends nothing, so a
// wall-clock step is not a quantity the budget can observe.
#[tokio::test(start_paused = true)]
async fn only_monotonic_elapsed_time_spends_the_budget() {
    let deadline = HopDeadline::start(INPUT_CONTROL_TIMEOUT_MS);
    std::thread::sleep(Duration::from_millis(30));
    assert!(
        (deadline.remaining_ms() - 5_000.0).abs() < f64::EPSILON,
        "the budget reads the monotonic clock, never the host's wall time"
    );
    assert_eq!(worker_budget_ms(&deadline), Some(4_250));
}
