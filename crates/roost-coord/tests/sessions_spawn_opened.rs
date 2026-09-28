//! A worker's committed `opened` reaches the spawn it completes through the real
//! durable dispatch arm, and only through that worker: the reconciliation v2
//! runs at `apps/coord/src/workers/worker-frame-dispatch.ts:216`
//! (`resolvePendingSpawnOpened`) after the append commits.
//!
//! Ports the durable-open half of `apps/coord/tests/sessions/pending-spawns.test.ts`
//! "ambiguous failures reconcile with durable opened while definite failures reject".

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod frame_dispatch_support;
mod workers_support;

use connectrpc::{ConnectError, ErrorCode};
use frame_dispatch_support::events::opened;
use frame_dispatch_support::{LinkFixture, OTHER_FP, SESSION_ID, WORKER_FP, event_frame};
use roost_coord::sessions::pending_spawns::{
    PendingSpawnResult, PendingSpawnSignature, SpawnReservation,
};
use roost_coord::worker_link::dispatch::{DispatchOutcome, FrameDispatch};

fn signature(worker_fp: &str) -> PendingSpawnSignature {
    PendingSpawnSignature {
        caller_key: "browser:tab-a".to_owned(),
        worker_fp: worker_fp.to_owned(),
        kind: "shell".to_owned(),
        folder: "/tmp".to_owned(),
        cols: None,
        rows: None,
    }
}

#[tokio::test]
async fn a_committed_opened_answers_a_spawn_whose_worker_reply_was_lost() {
    let fixture = LinkFixture::new("spawn-opened").await;
    let spawns = fixture.services.sessions.pending_spawns();
    let SpawnReservation::New(waiter) = spawns.reserve(SESSION_ID, signature(WORKER_FP)) else {
        panic!("the UUID was free");
    };
    let lost = ConnectError::new(ErrorCode::Unavailable, "worker disconnected");
    assert!(spawns.reject(SESSION_ID, lost, false));

    let mut dispatcher = fixture.dispatcher();
    let outcome = dispatcher
        .handle_durable(WORKER_FP, event_frame(opened(WORKER_FP, 6), 1))
        .await;
    assert_eq!(outcome, DispatchOutcome::Handled);
    assert_eq!(
        waiter.outcome().await.unwrap(),
        PendingSpawnResult {
            session_id: SESSION_ID.to_owned(),
            channel_id: 6,
        }
    );
}

#[tokio::test]
async fn an_opened_from_another_worker_does_not_answer_the_spawn() {
    let fixture = LinkFixture::new("spawn-opened-foreign").await;
    let spawns = fixture.services.sessions.pending_spawns();
    let SpawnReservation::New(_waiter) = spawns.reserve(SESSION_ID, signature(OTHER_FP)) else {
        panic!("the UUID was free");
    };
    let lost = ConnectError::new(ErrorCode::Unavailable, "worker disconnected");
    assert!(spawns.reject(SESSION_ID, lost, false));

    let mut dispatcher = fixture.dispatcher();
    let outcome = dispatcher
        .handle_durable(WORKER_FP, event_frame(opened(WORKER_FP, 6), 1))
        .await;
    assert_eq!(outcome, DispatchOutcome::Handled);
    // Still pending for its own worker: that worker's `opened` resolves it.
    assert!(spawns.resolve_opened(OTHER_FP, SESSION_ID, 8));
}
