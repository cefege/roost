//! The spawn reservation table: exact retries join one answer, conflicts are
//! refused before any worker command, ambiguous failures wait for the durable
//! `opened`, and definite failures, deadlines and revocations reject.
//!
//! Ports `apps/coord/tests/sessions/pending-spawns.test.ts`. Time is paused, so
//! the thirty-second deadlines are crossed without waiting them out.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use connectrpc::{ConnectError, ErrorCode};
use roost_coord::sessions::pending_spawns::{
    COMPLETED_SPAWN_RETENTION, MAX_PENDING_SPAWNS, PendingSpawnResult, PendingSpawnSignature,
    PendingSpawns, SpawnReservation, SpawnWaiter,
};

const SID: &str = "00000000-0000-4000-8000-000000000616";
const OTHER_SID: &str = "00000000-0000-4000-8000-000000000617";

fn signature() -> PendingSpawnSignature {
    PendingSpawnSignature {
        caller_key: "browser:tab-a".to_owned(),
        worker_fp: "aa".repeat(32),
        kind: "shell".to_owned(),
        folder: "/work".to_owned(),
        cols: Some(100),
        rows: Some(31),
    }
}

fn new_waiter(reservation: SpawnReservation) -> SpawnWaiter {
    match reservation {
        SpawnReservation::New(waiter) => waiter,
        other => panic!("expected a new reservation, got {other:?}"),
    }
}

fn joined_waiter(reservation: SpawnReservation) -> SpawnWaiter {
    match reservation {
        SpawnReservation::Joined(waiter) => waiter,
        other => panic!("expected a joined reservation, got {other:?}"),
    }
}

fn opened(session_id: &str, channel_id: u32) -> PendingSpawnResult {
    PendingSpawnResult {
        session_id: session_id.to_owned(),
        channel_id,
    }
}

/// The future's output if it is ready now, without letting paused time pass.
async fn ready_now<F: Future + Unpin>(future: &mut F) -> Option<F::Output> {
    tokio::time::timeout(Duration::ZERO, future).await.ok()
}

// v2: "exact caller and parameters join one caller-minted spawn".
#[tokio::test(start_paused = true)]
async fn an_exact_retry_joins_the_first_spawn_and_opened_waits_for_the_reply() {
    let table = Arc::new(PendingSpawns::new());
    let first = new_waiter(table.reserve(SID, signature()));
    let joined = joined_waiter(table.reserve(SID, signature()));

    // Durable `opened` is recorded, but the normal success still waits for the
    // worker's reply, which is ordered after its first full frame.
    assert!(table.resolve_opened(&signature().worker_fp, SID, 17));
    let mut first_outcome = Box::pin(first.outcome());
    assert!(
        ready_now(&mut first_outcome).await.is_none(),
        "opened alone must not answer a reply that has not been lost"
    );

    assert!(table.reject(
        SID,
        ConnectError::new(ErrorCode::Unavailable, "rpc reply lost"),
        false
    ));
    assert_eq!(first_outcome.await.unwrap(), opened(SID, 17));
    assert_eq!(joined.outcome().await.unwrap(), opened(SID, 17));

    // A retry inside the retention window gets the same answer, not a second spawn.
    let mut late = Box::pin(joined_waiter(table.reserve(SID, signature())).outcome());
    let answered = ready_now(&mut late)
        .await
        .expect("a retained answer is immediate");
    assert_eq!(answered.unwrap(), opened(SID, 17));
}

// v2: "conflicting caller or parameters fail before sharing the pending result".
#[tokio::test(start_paused = true)]
async fn a_different_caller_or_geometry_conflicts() {
    let table = Arc::new(PendingSpawns::new());
    let _first = new_waiter(table.reserve(SID, signature()));
    let other_tab = PendingSpawnSignature {
        caller_key: "browser:tab-b".to_owned(),
        ..signature()
    };
    assert!(matches!(
        table.reserve(SID, other_tab),
        SpawnReservation::Conflict
    ));
    let wider = PendingSpawnSignature {
        cols: Some(101),
        ..signature()
    };
    assert!(matches!(
        table.reserve(SID, wider),
        SpawnReservation::Conflict
    ));
}

// v2: "one global session UUID cannot dispatch to two workers".
#[tokio::test(start_paused = true)]
async fn one_uuid_cannot_dispatch_to_two_workers() {
    let table = Arc::new(PendingSpawns::new());
    let _first = new_waiter(table.reserve(SID, signature()));
    let elsewhere = PendingSpawnSignature {
        caller_key: "browser-b:tab-a".to_owned(),
        worker_fp: "bb".repeat(32),
        ..signature()
    };
    assert!(matches!(
        table.reserve(SID, elsewhere),
        SpawnReservation::Conflict
    ));
    // And an `opened` from the other worker cannot complete it.
    assert!(!table.resolve_opened(&"bb".repeat(32), SID, 3));
}

// v2: "ambiguous failures reconcile with durable opened while definite failures reject".
#[tokio::test(start_paused = true)]
async fn ambiguous_failures_reconcile_and_definite_failures_reject() {
    let table = Arc::new(PendingSpawns::new());
    let ambiguous = new_waiter(table.reserve(SID, signature()));
    assert!(table.reject(
        SID,
        ConnectError::new(ErrorCode::Unavailable, "worker disconnected"),
        false
    ));
    // Still reserved: an exact retry joins rather than dispatching twice.
    let _retry = joined_waiter(table.reserve(SID, signature()));
    assert!(table.resolve_opened(&signature().worker_fp, SID, 23));
    assert_eq!(ambiguous.outcome().await.unwrap(), opened(SID, 23));

    let definite = new_waiter(table.reserve(OTHER_SID, signature()));
    assert!(table.reject(
        OTHER_SID,
        ConnectError::new(ErrorCode::Internal, "keeper rejected spawn"),
        true
    ));
    let error = definite.outcome().await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Internal);
    assert_eq!(error.message.as_deref(), Some("keeper rejected spawn"));
    // A definite failure frees the UUID.
    let _again = new_waiter(table.reserve(OTHER_SID, signature()));
}

// v2 `reservePendingSpawn`'s timer: a spawn that never durably opens fails
// with DeadlineExceeded and releases its UUID.
#[tokio::test(start_paused = true)]
async fn a_spawn_that_never_opens_times_out_and_frees_its_uuid() {
    let table = Arc::new(PendingSpawns::new());
    let waiter = new_waiter(table.reserve(SID, signature()));
    assert!(table.reject(
        SID,
        ConnectError::new(ErrorCode::Unavailable, "lost"),
        false
    ));
    let error = waiter.outcome().await.unwrap_err();
    assert_eq!(error.code, ErrorCode::DeadlineExceeded);
    assert_eq!(
        error.message.as_deref(),
        Some(format!("spawn {SID} did not durably open within 30000ms").as_str())
    );
    assert!(
        !table.resolve_opened(&signature().worker_fp, SID, 9),
        "nothing is pending"
    );
    let _again = new_waiter(table.reserve(SID, signature()));
}

// v2 COMPLETED_SPAWN_RETENTION_MS: a resolved answer is retained for retries,
// then released so the table does not grow without bound.
#[tokio::test(start_paused = true)]
async fn a_resolved_spawn_is_retained_for_retries_and_then_released() {
    let table = Arc::new(PendingSpawns::new());
    let waiter = new_waiter(table.reserve(SID, signature()));
    assert!(table.resolve(SID, opened(SID, 5)));
    assert_eq!(waiter.outcome().await.unwrap(), opened(SID, 5));
    tokio::time::advance(COMPLETED_SPAWN_RETENTION - Duration::from_millis(1)).await;
    let _retained = joined_waiter(table.reserve(SID, signature()));
    tokio::time::advance(Duration::from_millis(2)).await;
    let _released = new_waiter(table.reserve(SID, signature()));
}

// v2 MAX_PENDING_SPAWNS.
#[tokio::test(start_paused = true)]
async fn the_table_refuses_past_its_capacity() {
    let table = Arc::new(PendingSpawns::new());
    let waiters: Vec<SpawnWaiter> = (0..MAX_PENDING_SPAWNS)
        .map(|index| {
            new_waiter(table.reserve(&format!("00000000-0000-4000-8000-{index:012}"), signature()))
        })
        .collect();
    assert_eq!(waiters.len(), MAX_PENDING_SPAWNS);
    let beyond = "ffffffff-0000-4000-8000-000000000001";
    assert!(matches!(
        table.reserve(beyond, signature()),
        SpawnReservation::Capacity
    ));
}

// v2 `rejectPendingSpawnsForWorker`: a revoked credential rejects that worker's
// pending spawns and drops its retained ones; another worker's are untouched.
#[tokio::test(start_paused = true)]
async fn a_revoked_worker_loses_its_spawns_and_no_other_worker_does() {
    let table = Arc::new(PendingSpawns::new());
    let doomed = new_waiter(table.reserve(SID, signature()));
    let retained = new_waiter(table.reserve(OTHER_SID, signature()));
    assert!(table.resolve(OTHER_SID, opened(OTHER_SID, 2)));
    let bystander_sid = "00000000-0000-4000-8000-000000000618";
    let bystander = PendingSpawnSignature {
        worker_fp: "bb".repeat(32),
        ..signature()
    };
    let survivor = new_waiter(table.reserve(bystander_sid, bystander.clone()));

    assert_eq!(table.reject_for_worker(&signature().worker_fp), 1);
    let error = doomed.outcome().await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Unauthenticated);
    assert_eq!(error.message.as_deref(), Some("worker credential revoked"));
    assert_eq!(retained.outcome().await.unwrap(), opened(OTHER_SID, 2));
    let _fresh = new_waiter(table.reserve(OTHER_SID, signature()));

    assert!(table.resolve(bystander_sid, opened(bystander_sid, 4)));
    assert_eq!(survivor.outcome().await.unwrap(), opened(bystander_sid, 4));
}
