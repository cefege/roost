//! The append transaction's two load-bearing guarantees, proven against a
//! database rather than by reading the code.
//!
//! 1. **Idempotence.** The same `(worker_fp, client_seq)` twice yields one durable
//!    row, one projection, and one publication. This is the `ON CONFLICT (worker_fp,
//!    client_seq) DO NOTHING` clause plus the dedupe short-circuit, and both are
//!    load-bearing: without the clause the second insert raises a unique-constraint
//!    failure, and without the short-circuit the projection runs again for an event
//!    that is already applied.
//! 2. **Publish strictly after commit.** The live effects are asked, during
//!    publication, whether a *second connection* can see the event row. A WAL reader
//!    never sees uncommitted work, so a `false` there means the publication ran
//!    inside the transaction. The same recorder pins route-before-bus.
//!
//! The remaining cases are the ones the source pins and a port can quietly lose:
//! the lost-publication claim path, a refusal that must write nothing, a
//! coordinator-side producer that must bind no channel, the workspace cascade, and
//! the force-close tombstone a returning worker must not resurrect.

// Every unwrap here is an assertion over a value the test just built: the panic
// IS the failure, which is why `unwrap_used` is denied in product code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod event_support;

use std::sync::{Arc, Mutex, PoisonError};

use event_support::{
    DASHBOARD_ID, EventFixture, RecordingEffects, Step, fingerprint, opened_event, respawned_event,
    session_id, worker_caller,
};
use roost_coord::events::append::{Caller, append_event};
use roost_coord::events::bus::Subscription;
use roost_coord::events::bus_messages::SessionBusMessage;

/// Watch the session bus for the life of the returned subscription, and collect
/// what arrives. The subscription is returned because a bus hands out RAII
/// subscriptions: dropping this one is what unsubscribes.
fn watch_session_bus(
    fixture: &EventFixture,
) -> (Arc<Mutex<Vec<SessionBusMessage>>>, Subscription<SessionBusMessage>) {
    let seen: Arc<Mutex<Vec<SessionBusMessage>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let observed = Arc::clone(&fixture.observed);
    let subscription = fixture.buses.session_bus.subscribe(move |message| {
        sink.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(message.clone());
        // The bus publication is a step in the same ordered log as the live
        // effects, because "route before bus" is only observable if both write to
        // one sequence.
        observed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .steps
            .push(Step::SessionPublished {
                kind: message.event.kind_name().to_owned(),
                event_id: message.event_id,
            });
    });
    (seen, subscription)
}

fn session_published(seen: &Arc<Mutex<Vec<SessionBusMessage>>>) -> Vec<String> {
    seen.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .map(|message| message.event.kind_name().to_owned())
        .collect()
}

#[tokio::test]
async fn a_replayed_sequence_writes_one_row_and_publishes_once() {
    let fixture = EventFixture::new("idempotence").await;
    let effects = RecordingEffects::new(&fixture);
    let (seen, _watching) = watch_session_bus(&fixture);
    let worker = fingerprint('d');
    let session = session_id('a');
    let caller = worker_caller(&worker, 7);

    let first = append_event(
        &fixture.writer,
        opened_event(&session, &worker, 11),
        &caller,
        &mut fixture.options(&effects),
    )
    .await
    .expect("the first append commits");
    assert!(first.admitted && first.inserted && first.published);

    let replay = append_event(
        &fixture.writer,
        opened_event(&session, &worker, 11),
        &caller,
        &mut fixture.options(&effects),
    )
    .await
    .expect("a dedupe is an ordinary outcome, not an error");

    // The dedupe is ADMITTED -- it must be, or a retry could never reach the claim
    // path -- and it neither inserted nor published anything.
    assert!(replay.admitted, "a dedupe must still be acknowledged");
    assert!(!replay.inserted, "a dedupe inserted a second durable row");
    assert!(!replay.published, "a dedupe published a second time");
    assert_eq!(
        fixture.rows_for(&worker, 7).await,
        1,
        "the log holds one row for one sequence"
    );
    assert_eq!(session_published(&seen), vec!["opened"]);
    assert_eq!(
        fixture
            .steps()
            .iter()
            .filter(|step| matches!(step, Step::Indexed { .. }))
            .count(),
        1,
        "the durable channel index was applied once"
    );
    fixture.close();
}

#[tokio::test]
async fn publication_happens_after_the_commit_and_routes_before_the_bus() {
    let fixture = EventFixture::new("ordering").await;
    let effects = RecordingEffects::new(&fixture);
    let (seen, _watching) = watch_session_bus(&fixture);
    let worker = fingerprint('d');
    let session = session_id('a');

    append_event(
        &fixture.writer,
        opened_event(&session, &worker, 11),
        &worker_caller(&worker, 1),
        &mut fixture.options(&effects),
    )
    .await
    .expect("the append commits");

    let steps = fixture.steps();

    // The guard comes FIRST, before any count or ordering assertion, because it is
    // the one that names the defect: if the publication ever moves back inside the
    // transaction, a second connection cannot see the row, and the count assertion
    // would only fail as a side effect of the mutation's shape.
    let indexed = steps
        .iter()
        .find_map(|step| match step {
            Step::Indexed {
                committed,
                authenticated_worker_fp,
                ..
            } => Some((committed, authenticated_worker_fp)),
            _ => None,
        })
        .expect("the durable channel index was applied");
    assert!(
        *indexed.0,
        "the publication ran before the commit: a second connection could not see the row"
    );
    assert_eq!(
        indexed.1.as_deref(),
        Some(worker.as_str()),
        "the index is told the authenticated fingerprint"
    );

    // Route before bus, in one function: the index is the first step and the bus
    // publication is the second.
    assert_eq!(
        steps.len(),
        2,
        "one index application and one bus publication, in that order"
    );
    match (&steps[0], &steps[1]) {
        (Step::Indexed { .. }, Step::SessionPublished { event_id, kind }) => {
            assert_eq!(kind, "opened");
            assert!(
                event_id.is_some_and(|id| id > 0),
                "the message is stamped with its durable id"
            );
        }
        other => panic!("route before bus, in one function: {other:?}"),
    }
    assert_eq!(session_published(&seen), vec!["opened"]);
    fixture.close();
}

#[tokio::test]
async fn an_event_for_an_unknown_session_writes_nothing_and_publishes_nothing() {
    let fixture = EventFixture::new("unknown-session").await;
    let effects = RecordingEffects::new(&fixture);
    let (seen, _watching) = watch_session_bus(&fixture);
    let worker = fingerprint('d');
    let stranger = session_id('b');

    let result = append_event(
        &fixture.writer,
        respawned_event(&stranger, 44),
        &worker_caller(&worker, 1),
        &mut fixture.options(&effects),
    )
    .await
    .expect("a refusal is a data outcome, not an exception");

    assert!(!result.admitted, "an unknown session is refused");
    assert!(!result.inserted && !result.published);
    assert!(
        fixture.event_ids().await.is_empty(),
        "a refusal writes nothing at all"
    );
    assert!(session_published(&seen).is_empty());
    assert!(fixture.steps().is_empty(), "no live effect was applied");
    fixture.close();
}

#[tokio::test]
async fn a_coordinator_producer_binds_no_channel() {
    let fixture = EventFixture::new("no-inference").await;
    let effects = RecordingEffects::new(&fixture);
    let worker = fingerprint('d');
    let session = session_id('a');

    append_event(
        &fixture.writer,
        opened_event(&session, &worker, 11),
        &worker_caller(&worker, 1),
        &mut fixture.options(&effects),
    )
    .await
    .expect("the worker append commits");

    // A producer with no authenticated fingerprint cannot bind the new channel:
    // guessing the worker from the route cache could bind on a worker that has
    // already been replaced.
    let respawn = append_event(
        &fixture.writer,
        respawned_event(&session, 12),
        &Caller::coordinator(DASHBOARD_ID),
        &mut fixture.options(&effects),
    )
    .await
    .expect("the coordinator append commits");

    assert!(respawn.published);
    let indexed = fixture
        .steps()
        .into_iter()
        .filter_map(|step| match step {
            Step::Indexed {
                authenticated_worker_fp,
                ..
            } => Some(authenticated_worker_fp),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(indexed.len(), 2, "both commits reached the live effect");
    assert_eq!(indexed[0].as_deref(), Some(worker.as_str()));
    assert_eq!(
        indexed[1], None,
        "an unauthenticated producer binds nothing"
    );
    fixture.close();
}
