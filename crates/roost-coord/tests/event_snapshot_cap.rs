//! The snapshot size cap on the append path: a worker snapshot naming more than
//! 1024 sessions is refused whole, before anything is written.
//!
//! Split from `event_publication.rs`, which pins what a committed event does once
//! published; this file pins a refusal that must commit and publish nothing.

// Every unwrap here is an assertion over a value the test just built: the panic
// IS the failure, which is why `unwrap_used` is denied in product code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod event_support;

use event_support::{
    EventFixture, RecordingEffects, fingerprint, live_session, session_id, snapshot_event,
    worker_caller,
};
use roost_coord::events::append::append_event;
use roost_protocol::wire::SessionId;

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

/// A well-formed session id per index, for the oversized snapshot. Only the shape
/// matters here: the append is refused before any of them is stored.
fn bulk_session_id(index: u32) -> SessionId {
    session_id(char::from_digit(index % 16, 16).unwrap_or('a'))
}
