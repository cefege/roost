//! The per-worker global-search lane: one page per worker at a time, distinct
//! workers in parallel, bounded queues, and waiters that leave cleanly.
//!
//! Ported from the "global search worker lane" block of
//! `apps/coord/tests/search/global-search-cursors.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use roost_coord::search::worker_lanes::{
    GLOBAL_SEARCH_MAX_WAITERS_PER_WORKER, GlobalSearchWorkerLaneOwner,
};
use tokio_util::sync::CancellationToken;

// "serializes one worker while granting distinct workers in parallel"
#[tokio::test]
async fn one_worker_is_serialized_while_distinct_workers_run_in_parallel() {
    let lanes = GlobalSearchWorkerLaneOwner::new();
    let signal = CancellationToken::new();
    let deadline = lanes.deadline_after(5_000);
    let first = lanes.acquire("worker-a", deadline, &signal).await;
    let other_worker = lanes.acquire("worker-b", deadline, &signal).await;
    assert!(first.is_some());
    assert!(other_worker.is_some());

    let second = tokio::spawn(lanes.acquire("worker-a", deadline, &signal));
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!second.is_finished(), "the second page waits for the first");
    drop(first);
    let second = second.await.unwrap();
    assert!(second.is_some());
}

// "removes an aborted queued waiter without disturbing the active lease"
#[tokio::test]
async fn an_aborted_waiter_leaves_the_queue_without_disturbing_the_active_lease() {
    let lanes = GlobalSearchWorkerLaneOwner::new();
    let first_signal = CancellationToken::new();
    let queued_signal = CancellationToken::new();
    let deadline = lanes.deadline_after(5_000);
    let first = lanes.acquire("worker-a", deadline, &first_signal).await;
    let queued = tokio::spawn(lanes.acquire("worker-a", deadline, &queued_signal));
    queued_signal.cancel();
    assert!(queued.await.unwrap().is_none());
    drop(first);
    assert!(
        lanes
            .acquire("worker-a", deadline, &first_signal)
            .await
            .is_some()
    );
}

// "rejects excess per-worker waiters before allocating timers"
#[tokio::test]
async fn excess_waiters_are_refused_at_admission() {
    let lanes = GlobalSearchWorkerLaneOwner::new();
    let signal = CancellationToken::new();
    let deadline = lanes.deadline_after(5_000);
    let active = lanes.acquire("worker-a", deadline, &signal).await;
    assert!(active.is_some());
    let queued: Vec<_> = (0..GLOBAL_SEARCH_MAX_WAITERS_PER_WORKER)
        .map(|_| tokio::spawn(lanes.acquire("worker-a", deadline, &signal)))
        .collect();
    // Refused at admission, not after waiting out the 5 s deadline.
    let excess = tokio::time::timeout(
        Duration::from_millis(200),
        lanes.acquire("worker-a", deadline, &signal),
    )
    .await;
    assert!(
        matches!(excess, Ok(None)),
        "the excess waiter is refused at once"
    );
    signal.cancel();
    for waiter in queued {
        assert!(waiter.await.unwrap().is_none());
    }
    drop(active);
}
