//! The two invariants the frame-dispatch layer exists to hold. Read these
//! before `worker_frame_dispatch.rs`: every case there could be satisfied by a
//! dispatcher that appended, logged and dropped, and these could not.
//!
//! 1. **A durable frame's `client_seq` slot is offered BEFORE its append runs.**
//!    A `client_seq` written before its slot exists can be acknowledged in an
//!    order the coordinator never durably recorded, and a coordinator that
//!    acknowledges a sequence it has not sequenced loses that frame on restart.
//! 2. **A snapshot's force-closed PTYs are killed by the DISPATCHER, after the
//!    readiness barrier** — not from inside the commit, before it. That is what
//!    `AppendOptions::defer_snapshot_reap` is for, and the reader it needs is the
//!    one `tests/event_publication.rs` demands exist in production code.
//!
//! `unwrap` and `expect` are denied outside `#[cfg(test)]`, and an integration
//! test is its own crate rather than a module of one, so the exemption has to be
//! stated here. Every panic names a value the test just built.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;
mod frame_dispatch_support;
mod workers_support;

use std::sync::{Arc, Mutex};

use frame_dispatch_support::events::{closed, opened, snapshot};
use frame_dispatch_support::{
    LinkFixture, ParkedEffects, SESSION_ID, WORKER_FP, event_frame, live_session, worker,
};
use roost_coord::worker_link::dispatch::{DispatchOutcome, FrameDispatch};

/// The fingerprint of a machine that is not this socket's, for an admission
/// refusal the cursor still has to move for.
const STRANGER: &str = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sequence_slot_is_offered_before_the_append_it_belongs_to_runs() {
    let (reached_tx, reached_rx) = std::sync::mpsc::channel();
    let release = Arc::new(std::sync::Barrier::new(2));
    let effects = ParkedEffects::new(reached_tx, Arc::clone(&release));
    let fixture = LinkFixture::with_effects("ordering", Arc::clone(&effects) as Arc<_>).await;
    let cursor = fixture.cursor();
    let dispatcher = fixture.dispatcher();

    // The append parks itself inside `index_durable_channel`, which runs after
    // the commit and before the append returns. That is the one instant at which
    // "the slot was offered first" is a question about two DIFFERENT instants
    // rather than a single one -- and it is why this is not a happy-path test.
    let worker_fp = WORKER_FP.to_owned();
    let dispatched = tokio::spawn(async move {
        let mut dispatcher = dispatcher;
        dispatcher
            .handle_durable(&worker_fp, event_frame(opened(WORKER_FP, 1), 1))
            .await
    });
    tokio::task::spawn_blocking(move || {
        reached_rx
            .recv()
            .expect("the append reached the durable publication");
    })
    .await
    .expect("the probe observed the append");

    // READ FIRST, ASSERT LAST. The append is parked inside the publication, so
    // this is the value the cursor held at that instant -- the only reading that
    // can tell "offered before the append" from "offered after it". Releasing
    // the barrier before asserting is deliberate: an assertion that panicked
    // here would leave the recorder parked on a barrier nobody will reach, and
    // a test that HANGS on a regression is worse than one that fails.
    let offered_while_in_flight = cursor.last_admitted().await;
    release.wait();
    let settled = dispatched.await.expect("the dispatch settles");

    assert_eq!(
        offered_while_in_flight,
        Some(1),
        "WHILE THE APPEND IS IN FLIGHT the sequence slot is already reserved. \
         Offering afterwards would read None here, and that is how a \
         coordinator ends up acknowledging a client_seq it never durably \
         sequenced -- the worker then drops it from its outbox and, on restart, \
         nothing replays it."
    );
    assert_eq!(settled, DispatchOutcome::Handled);
    assert_eq!(fixture.rows_for(1).await, 1, "and the event is durable");
    assert_eq!(fixture.acks(), vec![1]);
}

#[tokio::test]
async fn a_sequence_slot_is_offered_even_when_the_append_itself_refuses_the_event() {
    // The same rule with nothing parked: the reservation is not a consequence of
    // a successful commit, it happens FIRST. Were the offer after the append, a
    // refused append would leave the cursor where it was and the next frame
    // would read as a gap against a sequence that was never offered.
    let fixture = LinkFixture::new("ordering-refusal").await;
    let mut dispatcher = fixture.dispatcher();
    fixture.mark_ready();

    // An `opened` claiming another worker is refused by admission (row 3).
    let refused = dispatcher
        .handle_durable(WORKER_FP, event_frame(opened(STRANGER, 1), 1))
        .await;
    assert_eq!(refused, DispatchOutcome::Refused);

    assert_eq!(
        fixture.cursor().last_admitted().await,
        Some(1),
        "the slot was reserved before the append refused the event"
    );
    let admitted = dispatcher
        .handle_durable(WORKER_FP, event_frame(opened(WORKER_FP, 1), 2))
        .await;
    assert_eq!(
        admitted,
        DispatchOutcome::Handled,
        "so sequence 2 is the exact successor and is admitted, rather than \
         reading as a gap against a sequence that was never offered"
    );
}

#[tokio::test]
async fn a_snapshot_that_force_closes_a_session_reaches_the_owed_kills_from_the_dispatcher() {
    let fixture = LinkFixture::new("deferred-reap").await;
    let mut dispatcher = fixture.dispatcher();
    dispatcher
        .handle_durable(WORKER_FP, event_frame(opened(WORKER_FP, 1), 1))
        .await;
    // A durable `closed` is a TOMBSTONE, not a status: it is what permanently
    // force-closes a session, and nothing else can. The next test pins the half
    // of that which is easy to get wrong.
    dispatcher
        .handle_durable(WORKER_FP, event_frame(closed(), 2))
        .await;
    assert_eq!(
        fixture.services.orphan_kills.owed_count(),
        0,
        "nothing is owed until a snapshot reconciles the tombstone"
    );

    // The snapshot RE-ANNOUNCES the tombstoned session. That is the case the
    // filter exists for: a returning worker must not resurrect it, so the
    // effective snapshot strips the id and hands it back to be killed. The
    // append was told to defer, so the id comes back to the DISPATCHER -- which
    // is the reader `tests/event_publication.rs` asserts exists in production.
    let reannounced = snapshot(vec![live_session(&worker(WORKER_FP), 11)]);
    let outcome = dispatcher
        .handle_durable(WORKER_FP, event_frame(reannounced, 3))
        .await;

    assert_eq!(outcome, DispatchOutcome::Handled);
    assert!(fixture.handle.is_ready(), "the barrier crossed first");
    assert_eq!(fixture.acks(), vec![1, 2, 3], "and the snapshot is ACKed");
    assert_eq!(
        fixture.services.orphan_kills.owed_count(),
        1,
        "the force-closed session's kill is owed to a worker with no link yet, \
         which is the only way this id can be delivered at all"
    );

    // And it names the session the snapshot dropped: the kill is a browser
    // command addressed by session id, so the wrong id aims at the wrong PTY.
    let outbox = Arc::new(Mutex::new(Vec::new()));
    let delivered = fixture
        .services
        .orphan_kills
        .attach(&worker(WORKER_FP), outbox);
    assert_eq!(delivered.len(), 1, "exactly one kill is owed");
    assert_eq!(
        delivered[0].session_id, SESSION_ID,
        "and it names the session the filter stripped"
    );
    assert_eq!(fixture.services.orphan_kills.owed_count(), 0);
}

#[tokio::test]
async fn a_snapshot_that_omits_a_session_no_closed_tombstoned_owes_no_kill() {
    // The other half of the same assertion, and the one that would catch a
    // dispatcher that reaped a worker's whole set instead of the force-closed
    // ids. An open session a snapshot does not re-announce KEEPS ITS ROW, so
    // there is nothing to kill and the coordinator owes nothing.
    let fixture = LinkFixture::new("deferred-reap-open").await;
    let mut dispatcher = fixture.dispatcher();
    dispatcher
        .handle_durable(WORKER_FP, event_frame(opened(WORKER_FP, 1), 1))
        .await;

    // Re-announced, and never tombstoned: the filter has nothing to strip.
    let reannounced = snapshot(vec![live_session(&worker(WORKER_FP), 1)]);
    let outcome = dispatcher
        .handle_durable(WORKER_FP, event_frame(reannounced, 2))
        .await;

    assert_eq!(outcome, DispatchOutcome::Handled);
    assert_eq!(
        fixture.services.orphan_kills.owed_count(),
        0,
        "a session that was never tombstoned is not force-closed, so its PTY \
         must not be killed"
    );
}
