//! The DURABLE arm: which event classes a worker socket may commit, and which it
//! may not, per contract §3.3 and the transport gates in front of it (§3.4).
//!
//! One test per admitted class and one per rejected class, and each says which
//! rule it is standing on. The two invariants that are the reason the whole layer
//! exists are in `worker_frame_ordering.rs`, and the two synchronous arms are in
//! `worker_live_frames.rs`; a reader who wants to know whether the dispatcher
//! merely works should read those two files instead of this one.
//!
//! `unwrap` and `expect` are denied outside `#[cfg(test)]`, and an integration
//! test is its own crate rather than a module of one, so the exemption has to be
//! stated here. Every panic names a value the test just built.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod frame_dispatch_support;
mod workers_support;

use std::sync::Arc;

use frame_dispatch_support::events::{
    agent_reference, attached, closed, opened, respawned, snapshot,
};
use frame_dispatch_support::{
    LinkFixture, OTHER_FP, WORKER_FP, event_frame, live_session, session_id, worker,
};
use roost_coord::worker_link::dispatch::{
    DispatchOutcome, FrameClass, FrameDispatch, InboundFrame,
};
use roost_coord::worker_link::frame_dispatch::WorkerFrameDispatcher;

/// Open the fixture's one session, so a later event has a row to speak about.
async fn open_session(dispatcher: &mut WorkerFrameDispatcher) {
    let outcome = dispatcher
        .handle_durable(WORKER_FP, event_frame(opened(WORKER_FP, 1), 1))
        .await;
    assert_eq!(outcome, DispatchOutcome::Handled, "the opening commits");
}

// ---------------------------------------------------------------------------
// ADMITTED CLASSES. Five kinds may cross a socket that has not yet published a
// snapshot, and each must produce a durable row and an ACK of its exact sequence.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_opened_event_is_appended_and_acknowledged() {
    let fixture = LinkFixture::new("durable-opened").await;
    let mut dispatcher = fixture.dispatcher();

    let outcome = dispatcher
        .handle_durable(WORKER_FP, event_frame(opened(WORKER_FP, 1), 1))
        .await;

    assert_eq!(outcome, DispatchOutcome::Handled);
    assert_eq!(fixture.rows_for(1).await, 1, "the event is durable");
    assert_eq!(fixture.acks(), vec![1], "the exact sequence is ACKed");
}

#[tokio::test]
async fn a_closed_event_for_a_session_this_worker_opened_is_appended() {
    let fixture = LinkFixture::new("durable-closed").await;
    let mut dispatcher = fixture.dispatcher();
    open_session(&mut dispatcher).await;

    let outcome = dispatcher
        .handle_durable(WORKER_FP, event_frame(closed(), 2))
        .await;

    assert_eq!(outcome, DispatchOutcome::Handled);
    assert_eq!(fixture.rows_for(2).await, 1);
    assert_eq!(fixture.acks(), vec![1, 2]);
}

#[tokio::test]
async fn a_respawned_event_is_appended_and_acknowledged() {
    let fixture = LinkFixture::new("durable-respawned").await;
    let mut dispatcher = fixture.dispatcher();
    open_session(&mut dispatcher).await;

    let outcome = dispatcher
        .handle_durable(WORKER_FP, event_frame(respawned(2), 2))
        .await;

    assert_eq!(outcome, DispatchOutcome::Handled);
    assert_eq!(fixture.rows_for(2).await, 1);
    assert_eq!(fixture.acks(), vec![1, 2]);
}

#[tokio::test]
async fn an_agent_reference_is_appended_and_acknowledged() {
    // Rule 10 of §3.3: a reference for a row the coordinator force-closed
    // offline is still ADMITTED, because refusing it would wedge the worker's
    // ordered durable replay forever.
    let fixture = LinkFixture::new("durable-reference").await;
    let mut dispatcher = fixture.dispatcher();
    open_session(&mut dispatcher).await;

    let outcome = dispatcher
        .handle_durable(WORKER_FP, event_frame(agent_reference(), 2))
        .await;

    assert_eq!(outcome, DispatchOutcome::Handled);
    assert_eq!(fixture.rows_for(2).await, 1);
    assert_eq!(fixture.acks(), vec![1, 2]);
}

#[tokio::test]
async fn a_published_snapshot_marks_the_generation_ready_and_acknowledges_it() {
    let fixture = LinkFixture::new("durable-snapshot").await;
    let mut dispatcher = fixture.dispatcher();
    open_session(&mut dispatcher).await;
    assert!(
        !fixture.handle.is_ready(),
        "the barrier is closed until a snapshot publishes"
    );

    let event = snapshot(vec![live_session(&worker(WORKER_FP), 1)]);
    let outcome = dispatcher
        .handle_durable(WORKER_FP, event_frame(event, 2))
        .await;

    assert_eq!(outcome, DispatchOutcome::Handled);
    assert!(fixture.handle.is_ready(), "the barrier is now crossed");
    assert_eq!(fixture.acks(), vec![1, 2]);
}

// ---------------------------------------------------------------------------
// REFUSED CLASSES. Each drops with no ACK and no close unless it says otherwise,
// because "never existed" and "not yours" must look the same to a prober.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_kind_outside_the_barrier_allow_list_is_dropped_before_any_write() {
    let fixture = LinkFixture::new("refuse-barrier").await;
    let mut dispatcher = fixture.dispatcher();

    let outcome = dispatcher
        .handle_durable(WORKER_FP, event_frame(attached(), 1))
        .await;

    assert_eq!(
        outcome,
        DispatchOutcome::Refused,
        "an attached before the snapshot is dropped silently, so the worker replays it"
    );
    assert!(
        fixture.acks().is_empty(),
        "no ACK releases the replay barrier"
    );
    assert_eq!(fixture.rows_for(1).await, 0, "and nothing is written");
    assert!(!fixture.handle.is_ready());
}

#[tokio::test]
async fn folder_metadata_before_the_snapshot_is_acknowledged_and_never_written() {
    // The worker replays its journal one unacknowledged row at a time before
    // it sends the snapshot, so a git row left unacknowledged here would hold
    // that snapshot back for good and the worker would never become routable.
    let fixture = LinkFixture::new("metadata-before-barrier").await;
    let mut dispatcher = fixture.dispatcher();
    let git = roost_protocol::wire::event::SessionEvent::Git {
        session_id: session_id(),
        branch: Some("main".to_owned()),
        remote: None,
        ts: 1_500,
        trace_id: None,
    };

    let outcome = dispatcher
        .handle_durable(WORKER_FP, event_frame(git, 1))
        .await;

    assert_eq!(outcome, DispatchOutcome::Handled);
    assert_eq!(
        fixture.acks(),
        vec![1],
        "the ACK releases the worker's replay"
    );
    assert_eq!(fixture.rows_for(1).await, 0, "and nothing is written");
    assert!(!fixture.handle.is_ready());
}

#[tokio::test]
async fn the_same_kind_is_admitted_once_the_barrier_is_crossed() {
    // The gate is a gate, not a wall: the frame dropped above is appended once
    // the socket is ready, which is what "the worker replays it after its
    // snapshot" has to mean.
    let fixture = LinkFixture::new("refuse-barrier-open").await;
    let mut dispatcher = fixture.dispatcher();
    open_session(&mut dispatcher).await;
    fixture.mark_ready();

    let outcome = dispatcher
        .handle_durable(WORKER_FP, event_frame(attached(), 2))
        .await;

    assert_eq!(outcome, DispatchOutcome::Handled);
    assert_eq!(fixture.rows_for(2).await, 1);
}

#[tokio::test]
async fn a_zero_sequence_is_refused_rather_than_admitted_as_sequence_zero() {
    let fixture = LinkFixture::new("refuse-zero-seq").await;
    let mut dispatcher = fixture.dispatcher();

    let outcome = dispatcher
        .handle_durable(WORKER_FP, event_frame(opened(WORKER_FP, 1), 0))
        .await;

    assert_eq!(outcome, DispatchOutcome::Refused);
    assert_eq!(fixture.rows_for(0).await, 0);
    assert!(fixture.acks().is_empty());
    assert_eq!(
        fixture.cursor().last_admitted().await,
        None,
        "a refused frame reserves nothing"
    );
}

// v2 `handleEvent` appends any positive `clientSeq`: a worker resuming its
// outbox against a restarted coordinator is written and ACKed.
#[tokio::test]
async fn a_resumed_sequence_on_a_fresh_cursor_is_written_and_acked() {
    let fixture = LinkFixture::new("resume-above-one").await;
    let mut dispatcher = fixture.dispatcher();

    let outcome = dispatcher
        .handle_durable(WORKER_FP, event_frame(opened(WORKER_FP, 1), 3))
        .await;

    assert_eq!(outcome, DispatchOutcome::Handled);
    assert_eq!(fixture.rows_for(3).await, 1, "the resumed event is durable");
    assert_eq!(fixture.acks(), vec![3], "and its exact sequence is ACKed");
    assert_eq!(fixture.cursor().last_admitted().await, Some(3));
}

#[tokio::test]
async fn an_opened_claiming_another_worker_is_refused_as_data_with_no_ack_and_no_close() {
    let fixture = LinkFixture::new("refuse-foreign").await;
    let mut dispatcher = fixture.dispatcher();

    // Admission row 3: an `opened` may not claim a worker other than the
    // caller's. The caller's fingerprint is the HANDLE's, never the event's.
    let outcome = dispatcher
        .handle_durable(WORKER_FP, event_frame(opened(OTHER_FP, 1), 1))
        .await;

    assert_eq!(outcome, DispatchOutcome::Refused);
    assert!(fixture.acks().is_empty(), "a refusal gets no ACK");
    assert_eq!(fixture.rows_for(1).await, 0);
}

#[tokio::test]
async fn a_superseded_generation_drops_the_frame_with_no_ack() {
    let fixture = LinkFixture::new("refuse-fenced").await;
    let mut dispatcher = fixture.dispatcher();

    // A newer hello claims the fingerprint, which fences this socket.
    let newer = Arc::new(roost_coord::coord_core::worker_handle::WorkerHandle::new(
        worker(WORKER_FP),
        None,
        "generation-2".to_owned(),
        std::collections::BTreeSet::new(),
        fixture.socket.sender(),
    ));
    roost_coord::workers::registry::claim_generation(
        &fixture.services.buses,
        &fixture.services.workers,
        Arc::clone(&newer),
    );

    let outcome = dispatcher
        .handle_durable(WORKER_FP, event_frame(opened(WORKER_FP, 1), 1))
        .await;

    assert_eq!(outcome, DispatchOutcome::Refused);
    assert_eq!(
        fixture.rows_for(1).await,
        0,
        "a fenced socket writes nothing"
    );
    assert!(fixture.acks().is_empty());
}

#[tokio::test]
async fn a_durable_frame_on_the_synchronous_arm_is_refused_and_never_acked() {
    let fixture = LinkFixture::new("refuse-class-mismatch").await;
    let dispatcher = fixture.dispatcher();
    // The read loop classified an event as live. The event is neither appended
    // nor acknowledged, so the worker still holds it in its outbox and replays
    // it: a refusal, not a close, and the difference is worth a test.
    let misrouted = InboundFrame {
        class: FrameClass::Live,
        channel: 0,
        frame: event_frame(opened(WORKER_FP, 1), 1).frame,
    };

    assert_eq!(
        dispatcher.handle_now(WORKER_FP, misrouted),
        DispatchOutcome::Refused
    );
    assert!(
        fixture.acks().is_empty(),
        "nothing releases the replay barrier"
    );
    assert_eq!(fixture.rows_for(1).await, 0, "and no row was written");
}

#[tokio::test]
async fn a_frame_addressed_to_another_socket_is_refused() {
    let fixture = LinkFixture::new("refuse-identity").await;
    let dispatcher = fixture.dispatcher();

    assert_eq!(
        dispatcher.handle_now(
            OTHER_FP,
            frame_dispatch_support::rpc_frame(
                roost_protocol::wire::coord_worker::CoordWorkerUpstream::RpcOk {
                    request_id: "rpc-x".to_owned(),
                    data: serde_json::json!({}),
                    trace_id: None,
                }
            )
        ),
        DispatchOutcome::Refused,
        "one socket's dispatcher may not act for another"
    );
}

#[tokio::test]
async fn the_session_a_foreign_open_would_have_claimed_is_never_written() {
    // The named consequence of the refusal above, asserted on the row rather
    // than on the outcome: a prober that got a `sessions` row would learn the
    // difference between "never existed" and "not yours".
    let fixture = LinkFixture::new("refuse-foreign-row").await;
    let mut dispatcher = fixture.dispatcher();
    dispatcher
        .handle_durable(WORKER_FP, event_frame(opened(OTHER_FP, 1), 1))
        .await;

    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE id = ?")
        .bind(session_id().as_str())
        .fetch_one(fixture.database.pool())
        .await
        .expect("the count query runs");

    assert_eq!(rows, 0, "a refused open writes no session row");
}
