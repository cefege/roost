//! Whether the deferred-reap capability is REACHABLE from production.
//!
//! The guard moved here whole from `event_publication.rs`, which keeps the half
//! that is about correctness -- the ids come back right, and nothing was
//! reaped before the caller's barrier. This binary keeps the half that is about
//! the capability existing at all: a passing assertion on unreachable code
//! reads as coverage and is more dangerous than no test.

// Every expect here is an assertion over a value the test just built: the panic
// IS the failure, which is why `unwrap_used` is denied in product code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod event_support;

use event_support::{
    EventFixture, RecordingEffects, Step, closed_event, fingerprint, live_session, opened_event,
    session_id, snapshot_event, the_deferred_append_path_has_an_execution_path,
    the_deferred_reap_ids_have_a_production_reader, worker_caller,
};
use roost_coord::events::append::{AppendOptions, append_event};

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
    // THE REACHABILITY GUARD, and the reason this test is RED. The assertion
    // above proves the ids come back CORRECT; this one proves the path that
    // consumes them is ever TAKEN. Until the worker link sets the flag and
    // drains the ids, the whole deferred-append capability is inert and these
    // assertions are camouflage.
    assert!(
        the_deferred_append_path_has_an_execution_path()
            && the_deferred_reap_ids_have_a_production_reader(),
        "the deferred-append capability needs BOTH halves and has neither yet: \
         something must set `defer_snapshot_reap` (R3, the dispatcher), AND \
         something must READ the ids it returns (R4, the drain). The flag alone \
         is only reachability -- with the flag set and no reader, the ids are \
         still returned and dropped, and a force-closed PTY on an offline worker \
         is still never killed. GREEN WHEN: both are true."
    );
    assert!(
        !fixture
            .steps()
            .iter()
            .any(|step| matches!(step, Step::Reaped { .. })),
        "a worker connection defers the kill until its snapshot barrier"
    );
    fixture.close();
}
