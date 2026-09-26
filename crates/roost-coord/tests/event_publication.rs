//! The publication half of the append path, and the refusals that must publish
//! nothing.
//!
//! Split from `event_append.rs` by concern rather than by size: that file pins the
//! two guarantees every caller depends on (idempotence, and publish strictly after
//! commit), and this one pins what a committed event *does* once it is published --
//! the lost-publication claim, the workspace cascade, the force-close tombstone and
//! its reap, and the refusals that write nothing at all.

// Every unwrap here is an assertion over a value the test just built: the panic
// IS the failure, which is why `unwrap_used` is denied in product code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod event_support;

use std::sync::{Arc, Mutex, PoisonError};

use event_support::{
    DASHBOARD_ID, EventFixture, RecordingEffects, Step, closed_event, fingerprint, live_session,
    opened_event, respawned_event, session_id, snapshot_event, worker_caller, workspace_id,
};
use roost_coord::events::append::{AppendOptions, append_event};
use roost_coord::events::bus_messages::SessionBusMessage;
use roost_protocol::wire::{SessionEvent, SessionId, WorkspaceDelta};

/// Watch the session bus, and record every publication into the fixture's ordered
/// step log so a test can see where a bus delta fell relative to the live effects.
fn watch_session_bus(
    fixture: &EventFixture,
) -> (
    Arc<Mutex<Vec<SessionBusMessage>>>,
    roost_coord::events::bus::Subscription<SessionBusMessage>,
) {
    let seen: Arc<Mutex<Vec<SessionBusMessage>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let observed = Arc::clone(&fixture.observed);
    let subscription = fixture.buses.session_bus.subscribe(move |message| {
        sink.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(message.clone());
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
async fn a_dedupe_after_a_lost_publication_publishes_the_retained_effect() {
    let fixture = EventFixture::new("claim").await;
    let effects = RecordingEffects::new(&fixture);
    let (seen, _watching) = watch_session_bus(&fixture);
    let worker = fingerprint('d');
    let session = session_id('a');
    let caller = worker_caller(&worker, 3);

    // The connection generation is stale: the row commits, the live publication is
    // withheld, and the effect is retained.
    let mut fenced = fixture.options(&effects);
    fenced.can_publish = Some(&|| false);
    let first = append_event(
        &fixture.writer,
        opened_event(&session, &worker, 11),
        &caller,
        &mut fenced,
    )
    .await
    .expect("a fenced append still commits");
    assert!(first.inserted, "the durable row is written");
    assert!(!first.published, "a stale generation publishes nothing");
    assert!(
        session_published(&seen).is_empty(),
        "nothing reached the bus yet"
    );

    // The worker's retry: same sequence, same payload, live generation. The INSERT
    // dedupes and the retained effect is claimed and published.
    let replay = append_event(
        &fixture.writer,
        opened_event(&session, &worker, 11),
        &caller,
        &mut fixture.options(&effects),
    )
    .await
    .expect("the retry commits");

    assert!(replay.admitted && !replay.inserted && replay.published);
    assert!(!replay.replay_rejected, "the payload matched byte for byte");
    assert_eq!(fixture.rows_for(&worker, 3).await, 1);
    assert_eq!(
        session_published(&seen),
        vec!["opened"],
        "the claimed effect is published exactly once"
    );
    let stamped = seen.lock().unwrap_or_else(PoisonError::into_inner)[0].event_id;
    assert!(
        stamped.is_some(),
        "the claimed publication keeps the durable id the first delivery committed"
    );
    fixture.close();
}


#[tokio::test]
async fn a_dedupe_with_a_different_payload_is_refused_as_a_protocol_violation() {
    let fixture = EventFixture::new("mismatch").await;
    let effects = RecordingEffects::new(&fixture);
    let (seen, _watching) = watch_session_bus(&fixture);
    let worker = fingerprint('d');
    let session = session_id('a');
    let caller = worker_caller(&worker, 4);

    let mut fenced = fixture.options(&effects);
    fenced.can_publish = Some(&|| false);
    append_event(
        &fixture.writer,
        opened_event(&session, &worker, 11),
        &caller,
        &mut fenced,
    )
    .await
    .expect("the fenced append commits");

    // A different event under the same sequence is not a retry.
    let replay = append_event(
        &fixture.writer,
        respawned_event(&session, 12),
        &caller,
        &mut fixture.options(&effects),
    )
    .await
    .expect("the replay is refused as data, not as an error");

    assert!(replay.replay_rejected, "two payloads for one sequence");
    assert!(!replay.published);
    assert!(session_published(&seen).is_empty());
    fixture.close();
}


#[tokio::test]
async fn a_closed_event_orphans_its_workspace_and_publishes_the_deletion() {
    let fixture = EventFixture::new("cascade").await;
    let effects = RecordingEffects::new(&fixture);
    let worker = fingerprint('d');
    let session = session_id('a');
    let workspace = workspace_id('a');

    sqlx::query(
        "INSERT INTO workspaces (id, worker_fp, name, created_at_ms, updated_at_ms, dashboard_id) \
         VALUES (?, ?, 'w', 1, 1, ?)",
    )
    .bind(workspace.as_str())
    .bind(worker.as_str())
    .bind(DASHBOARD_ID)
    .execute(fixture.writer.pool())
    .await
    .expect("the workspace inserts");

    append_event(
        &fixture.writer,
        opened_event(&session, &worker, 11),
        &worker_caller(&worker, 1),
        &mut fixture.options(&effects),
    )
    .await
    .expect("the opened append commits");
    assign_workspace(&fixture, &effects, &session, &workspace, 2).await;

    let deletions: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&deletions);
    let _watching = fixture.buses.workspace_bus.subscribe(move |delta| {
        if let WorkspaceDelta::Deleted { id } = delta {
            sink.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(id.to_string());
        }
    });

    append_event(
        &fixture.writer,
        closed_event(&session),
        &worker_caller(&worker, 3),
        &mut fixture.options(&effects),
    )
    .await
    .expect("the close commits");

    assert_eq!(
        deletions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_slice(),
        &[workspace.as_str().to_owned()],
        "the last pane in a workspace deletes it, and says so on the bus"
    );
    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspaces WHERE id = ?")
        .bind(workspace.as_str())
        .fetch_one(fixture.writer.pool())
        .await
        .expect("the count runs");
    assert_eq!(remaining, 0, "the orphaned workspace row is gone");
    fixture.close();
}


#[tokio::test]
async fn a_snapshot_never_resurrects_a_force_closed_session() {
    let fixture = EventFixture::new("tombstone").await;
    let effects = RecordingEffects::new(&fixture);
    let worker = fingerprint('d');
    let session = session_id('a');
    let other = session_id('b');

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

    // The worker comes back and re-announces both sessions. The force-closed one is
    // a permanent tombstone, so the effective snapshot drops it and the caller is
    // told to reap it after the commit.
    let result = append_event(
        &fixture.writer,
        snapshot_event(
            &worker,
            vec![
                live_session(&session, &worker, 11, None),
                live_session(&other, &worker, 12, None),
            ],
        ),
        &worker_caller(&worker, 3),
        &mut fixture.options(&effects),
    )
    .await
    .expect("the snapshot commits");

    assert!(result.admitted && result.published);
    assert_eq!(result.snapshot_reap_ids, vec![session.as_str().to_owned()]);
    let SessionEvent::Snapshot { sessions, .. } = &result.event else {
        panic!("a snapshot append answers with a snapshot");
    };
    assert_eq!(
        sessions
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        vec![other.as_str()],
        "the tombstoned session is out of the effective snapshot"
    );
    assert_eq!(
        fixture
            .steps()
            .into_iter()
            .filter(|step| matches!(step, Step::Reaped { .. }))
            .collect::<Vec<_>>(),
        vec![Step::Reaped {
            session_id: session.as_str().to_owned()
        }],
        "the reap is dispatched after the commit"
    );
    fixture.close();
}


#[tokio::test]
async fn a_deferred_reap_waits_for_the_callers_readiness_barrier() {
    let fixture = EventFixture::new("deferred-reap").await;
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
    .expect("the opened append commits");
    append_event(
        &fixture.writer,
        closed_event(&session),
        &worker_caller(&worker, 2),
        &mut fixture.options(&effects),
    )
    .await
    .expect("the close commits");

    let mut options: AppendOptions<'_> = fixture.options(&effects);
    options.defer_snapshot_reap = true;
    let result = append_event(
        &fixture.writer,
        snapshot_event(&worker, vec![live_session(&session, &worker, 11, None)]),
        &worker_caller(&worker, 3),
        &mut options,
    )
    .await
    .expect("the snapshot commits");

    assert_eq!(result.snapshot_reap_ids, vec![session.as_str().to_owned()]);
    assert!(
        !fixture
            .steps()
            .iter()
            .any(|step| matches!(step, Step::Reaped { .. })),
        "a worker connection defers the kill until its snapshot barrier"
    );
    fixture.close();
}


#[tokio::test]
async fn a_snapshot_over_the_cap_is_refused_before_anything_is_written() {
    let fixture = EventFixture::new("snapshot-cap").await;
    let effects = RecordingEffects::new(&fixture);
    let worker = fingerprint('d');
    let oversized = (0..1_025_u32)
        .map(|index| live_session(&bulk_session_id(index), &worker, 1, None))
        .collect::<Vec<_>>();

    let error = append_event(
        &fixture.writer,
        snapshot_event(&worker, oversized),
        &worker_caller(&worker, 1),
        &mut fixture.options(&effects),
    )
    .await
    .expect_err("a snapshot past the cap is an error, not a truncated list");

    assert!(
        error.to_string().contains("exceeds 1024 sessions"),
        "the cap is named: {error}"
    );
    assert!(
        fixture.event_ids().await.is_empty(),
        "a refused snapshot writes nothing"
    );
    fixture.close();
}

/// Put a session in a workspace, through the path a handler uses.
async fn assign_workspace(
    fixture: &EventFixture,
    effects: &RecordingEffects,
    session: &SessionId,
    workspace: &roost_protocol::wire::WorkspaceId,
    client_seq: u64,
) {
    append_event(
        &fixture.writer,
        SessionEvent::WorkspaceAssigned {
            session_id: session.clone(),
            workspace_id: Some(workspace.clone()),
            ts: 4,
            trace_id: None,
        },
        &worker_caller(&fingerprint('d'), client_seq),
        &mut fixture.options(effects),
    )
    .await
    .expect("the assignment commits");
}


/// A well-formed session id per index, for the oversized snapshot. Only the shape
/// matters here: the append is refused before any of them is stored.
fn bulk_session_id(index: u32) -> SessionId {
    session_id(char::from_digit(index % 16, 16).unwrap_or('a'))
}
