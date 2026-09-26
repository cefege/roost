//! The three durable reads and the windows they use, which are recovery
//! invariants rather than conveniences.
//!
//! Ported from `apps/coord/src/events/event-query.ts`, whose header names the
//! invariant: "Ascending IDs and stable cursor cutoffs are recovery invariants."
//! An off-by-one in either comparison is a duplicated or a missing event, and a
//! duplicated `closed` is a terminal that vanishes -- so the boundaries are
//! asserted directly rather than inferred from a passing backfill.
//!
//! The private kind is exercised here too: `agent_reference` is durable and its
//! owning worker recovers it, and all three reads must leave it out.

// Every unwrap here is an assertion over a value the test just built: the panic
// IS the failure, which is why `unwrap_used` is denied in product code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod event_support;

use event_support::{
    EventFixture, closed_event, fingerprint, opened_event, respawned_event, session_id,
    worker_caller,
};
use roost_coord::events::append::append_event;
use roost_coord::events::bus_messages::SessionBusMessage;
use roost_coord::events::event_log::EventLog;
use roost_coord::events::pending_publications::PendingPublicationStore;
use roost_protocol::wire::{SessionEvent, SessionId, WorkerFp};
use std::sync::{Arc, Mutex, PoisonError};

/// A live-effects implementation for a test that only cares about the log.
struct NoEffects;

impl roost_coord::events::append::LiveEffects for NoEffects {
    fn index_durable_channel(&self, _event: &SessionEvent, _worker_fp: Option<&WorkerFp>) {}

    fn kill_orphan_pty(&self, _worker_fp: &WorkerFp, _session_id: &str) {}
}

/// An `EventLog` over the fixture, so the four-method surface is what the tests
/// drive rather than the free functions behind it.
fn event_log(fixture: &EventFixture) -> EventLog {
    EventLog::new(
        fixture.writer.clone(),
        Arc::clone(&fixture.buses),
        Arc::new(Mutex::new(PendingPublicationStore::new())),
        Arc::new(NoEffects),
    )
}

/// Commit `count` events for one worker, on consecutive sequences.
async fn commit_events(fixture: &EventFixture, worker: &WorkerFp, session: &SessionId, count: u64) {
    let effects = NoEffects;
    for sequence in 1..=count {
        let event = if sequence == 1 {
            opened_event(session, worker, 11)
        } else {
            respawned_event(session, 10 + sequence as i64)
        };
        append_event(
            &fixture.writer,
            event,
            &worker_caller(worker, sequence),
            &mut fixture.options(&effects),
        )
        .await
        .expect("the append commits");
    }
}

#[tokio::test]
async fn a_backfill_returns_events_in_order_above_the_cursor() {
    let fixture = EventFixture::new("query-since").await;
    let worker = fingerprint('d');
    let session = session_id('a');
    commit_events(&fixture, &worker, &session, 4).await;
    let log = event_log(&fixture);

    let all = log.get_events_since(0, None).await.expect("the backfill runs");
    assert_eq!(
        all.iter().map(|stored| stored.id).collect::<Vec<_>>(),
        vec![1, 2, 3, 4],
        "ascending ids, oldest first"
    );

    let tail = log
        .get_events_since(2, None)
        .await
        .expect("the backfill runs");
    assert_eq!(
        tail.iter().map(|stored| stored.id).collect::<Vec<_>>(),
        vec![3, 4],
        "`id > since_id` is strict: the cursor's own event is not repeated"
    );
    assert!(log.get_events_since(4, None).await.expect("it runs").is_empty());
    fixture.close();
}

#[tokio::test]
async fn a_backfill_page_honours_its_limit() {
    let fixture = EventFixture::new("query-limit").await;
    let worker = fingerprint('d');
    let session = session_id('a');
    commit_events(&fixture, &worker, &session, 5).await;
    let log = event_log(&fixture);

    let page = log.get_events_since(0, Some(2)).await.expect("it runs");
    assert_eq!(
        page.iter().map(|stored| stored.id).collect::<Vec<_>>(),
        vec![1, 2],
        "a short page is the oldest events, so the next cursor is the last one"
    );
    fixture.close();
}

#[tokio::test]
async fn the_recovery_cutoff_is_the_newest_public_event() {
    let fixture = EventFixture::new("query-cutoff").await;
    let worker = fingerprint('d');
    let session = session_id('a');
    let log = event_log(&fixture);

    assert_eq!(
        log.get_event_max_id().await.expect("it runs"),
        0,
        "an empty log has no cutoff, and zero is the answer the feed expects"
    );

    commit_events(&fixture, &worker, &session, 3).await;
    assert_eq!(log.get_event_max_id().await.expect("it runs"), 3);
    fixture.close();
}

#[tokio::test]
async fn one_recovery_interval_is_closed_at_the_cutoff_and_open_at_the_cursor() {
    let fixture = EventFixture::new("query-through").await;
    let worker = fingerprint('d');
    let session = session_id('a');
    commit_events(&fixture, &worker, &session, 5).await;
    let log = event_log(&fixture);

    // The seam: the live lane carries what was published after the cutoff, and the
    // backfill carries the closed interval below it. `cursor < id <= cutoff` is
    // what makes a reconnect read every event exactly once across that seam.
    let interval = log
        .get_events_through(2, 4, None)
        .await
        .expect("it runs");
    assert_eq!(
        interval.iter().map(|stored| stored.id).collect::<Vec<_>>(),
        vec![3, 4],
        "the cursor is excluded and the cutoff is included"
    );

    // A page inside the same interval, then the rest: the ceiling never moves.
    let first = log
        .get_events_through(2, 4, Some(1))
        .await
        .expect("it runs");
    assert_eq!(first.iter().map(|stored| stored.id).collect::<Vec<_>>(), vec![3]);
    let rest = log
        .get_events_through(3, 4, None)
        .await
        .expect("it runs");
    assert_eq!(rest.iter().map(|stored| stored.id).collect::<Vec<_>>(), vec![4]);
    fixture.close();
}

#[tokio::test]
async fn a_reconnect_reads_every_event_exactly_once_across_the_seam() {
    let fixture = EventFixture::new("query-seam").await;
    let effects = NoEffects;
    let worker = fingerprint('d');
    let session = session_id('a');
    let log = event_log(&fixture);

    // A feed subscribes FIRST and captures the cutoff AFTER, which is the order
    // the sync feed uses and the reason the interval closes at the cutoff rather
    // than at whatever was durable when the client last spoke.
    let live: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&live);
    let _subscription = fixture.buses.session_bus.subscribe(move |message: &SessionBusMessage| {
        if let Some(id) = message.event_id {
            sink.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(id);
        }
    });

    // The client is current through event 1. Three more commit before the cutoff,
    // and three after it.
    for sequence in 1..=3 {
        let event = if sequence == 1 {
            opened_event(&session, &worker, 11)
        } else {
            respawned_event(&session, 10 + sequence as i64)
        };
        append_event(
            &fixture.writer,
            event,
            &worker_caller(&worker, sequence),
            &mut fixture.options(&effects),
        )
        .await
        .expect("the append commits");
    }
    let cutoff = log.get_event_max_id().await.expect("it runs");
    assert_eq!(cutoff, 3);
    for sequence in 4..=6 {
        append_event(
            &fixture.writer,
            respawned_event(&session, 10 + sequence as i64),
            &worker_caller(&worker, sequence),
            &mut fixture.options(&effects),
        )
        .await
        .expect("the append commits");
    }

    // The backfill reads the closed interval, and the lane carries only what the
    // cutoff did not already cover -- exactly as a feed filters its own stream.
    let interval = log
        .get_events_through(1, cutoff, None)
        .await
        .expect("it runs");
    let mut delivered = interval.iter().map(|stored| stored.id).collect::<Vec<_>>();
    delivered.extend(
        live.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .copied()
            .filter(|id| *id > cutoff),
    );
    delivered.sort_unstable();
    assert_eq!(
        delivered,
        vec![2, 3, 4, 5, 6],
        "every event after the client's cursor arrives exactly once, across the seam"
    );
    fixture.close();
}

#[tokio::test]
async fn the_private_kind_is_invisible_to_every_read() {
    let fixture = EventFixture::new("query-private").await;
    let effects = NoEffects;
    let worker = fingerprint('d');
    let session = session_id('a');
    let log = event_log(&fixture);

    append_event(
        &fixture.writer,
        opened_event(&session, &worker, 11),
        &worker_caller(&worker, 1),
        &mut fixture.options(&effects),
    )
    .await
    .expect("the opened append commits");

    // A private recovery reference: durable, owned by its worker, and invisible to
    // every browser through either lane.
    let mut reference = opened_event(&session, &worker, 12);
    if let SessionEvent::Opened { session_id, .. } = &mut reference {
        *session_id = session.clone();
    }
    let private = SessionEvent::AgentReference {
        session_id: session.clone(),
        reference: None,
        ts: 7,
        trace_id: None,
    };
    append_event(
        &fixture.writer,
        private,
        &worker_caller(&worker, 2),
        &mut fixture.options(&effects),
    )
    .await
    .expect("the private append commits");

    // It is durable: the worker's own replay can still find it.
    assert_eq!(
        fixture.event_ids().await.len(),
        2,
        "the private event is stored"
    );
    // And invisible: the cutoff, the backfill and the interval all skip it.
    assert_eq!(
        log.get_event_max_id().await.expect("it runs"),
        1,
        "the cutoff names the last PUBLIC event"
    );
    let backfill = log.get_events_since(0, None).await.expect("it runs");
    assert_eq!(
        backfill
            .iter()
            .map(|stored| stored.event.kind_name())
            .collect::<Vec<_>>(),
        vec!["opened"],
        "no browser lane sees the private kind"
    );
    let interval = log.get_events_through(0, 2, None).await.expect("it runs");
    assert_eq!(interval.len(), 1);
    fixture.close();
}

#[tokio::test]
async fn a_close_is_readable_by_a_client_that_reconnects_afterwards() {
    // The `closed` row is what a browser that missed the live lane recovers, and
    // it is the row whose absence is a terminal that never disappears.
    let fixture = EventFixture::new("query-closed").await;
    let effects = NoEffects;
    let worker = fingerprint('d');
    let session = session_id('a');
    let log = event_log(&fixture);

    append_event(
        &fixture.writer,
        opened_event(&session, &worker, 11),
        &worker_caller(&worker, 1),
        &mut fixture.options(&effects),
    )
    .await
    .expect("the opened append commits");
    append_event(
        &fixture.writer,
        closed_event(&session),
        &worker_caller(&worker, 2),
        &mut fixture.options(&effects),
    )
    .await
    .expect("the close commits");

    let recovered = log.get_events_since(0, None).await.expect("it runs");
    assert_eq!(
        recovered
            .iter()
            .map(|stored| stored.event.kind_name())
            .collect::<Vec<_>>(),
        vec!["opened", "closed"],
        "the whole life of a session replays in order"
    );
    fixture.close();
}
