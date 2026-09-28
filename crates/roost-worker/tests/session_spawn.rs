//! The spawn: the two durable claims, the PTY, and the record. Every variant
//! here is about a resource that leaks — a claim the store can never give back,
//! a PTY nobody can reach, or an `opened` event a browser learns about twice.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "session_spawn_support/mod.rs"]
mod spawn_support;

use std::sync::Arc;

use roost_protocol::wire::event::SessionEvent;
use roost_worker::channel_fsm::ChannelState;
use roost_worker::event_store::DurableEventKind;
use roost_worker::session::spawn::{SpawnRefusal, spawn_shell};
use roost_worker::session::types::SessionRecord;

use spawn_support::{
    BindingThatRecordsDelivery, FakeKeeper, FixedResolver, LedgerSink, context, request,
    roomy_capacity,
};
use spawn_support::{session_id as support_session_id, shell_spec, worker_fp};

/// A spawn that cannot open its PTY must give BOTH claims back. A leaked claim
/// is capacity the store will never hand out again, and a store that has lost
/// its capacity refuses every later write in the worker, not just this one.
#[tokio::test]
async fn a_spawn_the_keeper_refuses_releases_both_claims_and_opens_nothing() {
    let events = LedgerSink::new();
    let opened = events.reserve(DurableEventKind::Opened);
    let close = events.reserve(DurableEventKind::Closed);
    let keeper = FakeKeeper::refusing();
    let resolver = FixedResolver::at("/tmp");
    let fp = worker_fp();
    let capacity = roomy_capacity();

    let refused = spawn_shell(
        &context(&keeper, &events, &resolver, &fp, &capacity),
        opened,
        close,
        Arc::new(BindingThatRecordsDelivery),
        request(21),
        1_000,
    )
    .await;
    assert!(
        matches!(refused, Err(SpawnRefusal::KeeperRefused { .. })),
        "the refusal did not surface, got {refused:?}"
    );
    let mut released = events.released();
    released.sort_unstable();
    let mut expected = vec![opened.id(), close.id()];
    expected.sort_unstable();
    assert_eq!(released, expected, "a claim survived the failed spawn");
    assert!(
        events.emitted().is_empty(),
        "a refused spawn announced a session"
    );
    assert!(
        keeper.opened_channels().is_empty(),
        "a refusing keeper opened a PTY anyway"
    );
    assert_eq!(
        events.live_claims(),
        0,
        "the store is still holding capacity"
    );
}

/// The successful shape: the `opened` claim is consumed by the event, the
/// close claim is committed and left ON THE RECORD for the close to consume,
/// and the record comes back attached.
#[tokio::test]
async fn a_spawn_consumes_the_opened_claim_and_leaves_the_close_claim_committed() {
    let events = LedgerSink::new();
    let opened = events.reserve(DurableEventKind::Opened);
    let close = events.reserve(DurableEventKind::Closed);
    let keeper = FakeKeeper::working();
    let resolver = FixedResolver::at("/tmp");
    let fp = worker_fp();
    let capacity = roomy_capacity();

    let record: SessionRecord = spawn_shell(
        &context(&keeper, &events, &resolver, &fp, &capacity),
        opened,
        close,
        Arc::new(BindingThatRecordsDelivery),
        request(22),
        1_000,
    )
    .await
    .expect("a working keeper spawns");

    let emitted = events.emitted();
    assert_eq!(emitted.len(), 1, "the spawn announced more than one event");
    match emitted[0].as_ref().expect("an event was written") {
        SessionEvent::Opened {
            session_id,
            channel,
            cwd,
            ts,
            ..
        } => {
            assert_eq!(session_id, record.session_id());
            assert_eq!(*channel, record.channel_id());
            assert_eq!(cwd, &record.identity.cwd);
            assert_eq!(*ts, 1_000);
        }
        other => panic!("a spawn announced {other:?} rather than an opened"),
    }
    assert_eq!(
        events.held(),
        vec![close.id()],
        "the close claim was not committed"
    );
    assert!(
        events.released().is_empty(),
        "a successful spawn released a claim"
    );
    assert_eq!(
        record.fsm.state(),
        Some(ChannelState::Attached),
        "a spawned channel is not attached, so nothing may ever close it"
    );
    assert_eq!(record.child_pid, Some(4242));
    assert_eq!(keeper.opened_channels(), vec![(22, 80, 24)]);
    assert_eq!(record.identity.socket_path, "mux:22");
}

/// A respawn announces a `respawned`, not an `opened`. An `opened` here would
/// tell every browser watching that row to paint a start moment it never had.
#[tokio::test]
async fn a_respawn_announces_a_respawn_and_not_an_opened() {
    let events = LedgerSink::new();
    let opened = events.reserve(DurableEventKind::State);
    let close = events.reserve(DurableEventKind::Closed);
    let keeper = FakeKeeper::working();
    let resolver = FixedResolver::at("/tmp");
    let fp = worker_fp();
    let capacity = roomy_capacity();
    let mut wanted = request(23);
    wanted.event = DurableEventKind::State;
    wanted.session_id = Some(support_session_id());
    wanted.shell_spec = Some(shell_spec("/somewhere/that/is/gone"));

    let record = spawn_shell(
        &context(&keeper, &events, &resolver, &fp, &capacity),
        opened,
        close,
        Arc::new(BindingThatRecordsDelivery),
        wanted,
        2_000,
    )
    .await
    .expect("a respawn succeeds");

    match events.emitted()[0].as_ref().expect("an event was written") {
        SessionEvent::Respawned {
            session_id,
            new_channel,
            ..
        } => {
            assert_eq!(
                session_id,
                &support_session_id(),
                "the respawn changed the session id"
            );
            assert_eq!(*new_channel, record.channel_id());
        }
        other => panic!("a respawn announced {other:?}"),
    }
    // The retained launch contract is used verbatim: a folder that has since
    // been deleted must not fail a session that was working a second ago.
    assert_eq!(record.identity.shell_spec.cwd, "/somewhere/that/is/gone");
}

/// Geometry is validated before a PTY is opened, and the refusal gives BOTH
/// claims back: v2 `session-spawn.ts:49-50` throws inside the try whose catch
/// (`:143-146`) releases the opened and close reservations it still owns.
#[tokio::test]
async fn a_refused_geometry_opens_no_pty_and_releases_both_claims() {
    let events = LedgerSink::new();
    let opened = events.reserve(DurableEventKind::Opened);
    let close = events.reserve(DurableEventKind::Closed);
    let keeper = FakeKeeper::working();
    let resolver = FixedResolver::at("/tmp");
    let fp = worker_fp();
    let capacity = roomy_capacity();
    let mut too_wide = request(24);
    too_wide.cols = 900;

    let refused = spawn_shell(
        &context(&keeper, &events, &resolver, &fp, &capacity),
        opened,
        close,
        Arc::new(BindingThatRecordsDelivery),
        too_wide,
        1_000,
    )
    .await;
    assert!(
        matches!(refused, Err(SpawnRefusal::Geometry { cols: 900, .. })),
        "an impossible geometry was not refused, got {refused:?}"
    );
    let mut released = events.released();
    released.sort_unstable();
    let mut expected = vec![opened.id(), close.id()];
    expected.sort_unstable();
    assert_eq!(released, expected, "a refused geometry kept a claim");
    assert_eq!(events.live_claims(), 0, "the store is still holding capacity");
    assert!(events.emitted().is_empty());
    assert!(
        keeper.opened_channels().is_empty(),
        "a refused geometry still reached the keeper"
    );
}

/// An event kind that is neither a spawn nor a respawn has no business
/// announcing a PTY, and it is refused rather than coerced into one.
#[tokio::test]
async fn a_close_may_not_announce_a_spawn() {
    let events = LedgerSink::new();
    let opened = events.reserve(DurableEventKind::Closed);
    let close = events.reserve(DurableEventKind::Closed);
    let keeper = FakeKeeper::working();
    let resolver = FixedResolver::at("/tmp");
    let fp = worker_fp();
    let capacity = roomy_capacity();
    let mut wrong = request(25);
    wrong.event = DurableEventKind::Exited;

    let refused = spawn_shell(
        &context(&keeper, &events, &resolver, &fp, &capacity),
        opened,
        close,
        Arc::new(BindingThatRecordsDelivery),
        wrong,
        1_000,
    )
    .await;
    assert!(
        matches!(refused, Err(SpawnRefusal::UnnameableEvent { .. })),
        "an unnameable event was not refused, got {refused:?}"
    );
    assert!(
        keeper.opened_channels().is_empty(),
        "an unnameable event still opened a PTY"
    );
}
