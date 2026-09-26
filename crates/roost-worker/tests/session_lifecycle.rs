//! What the session lifecycle guarantees about ENDING a session: one close per
//! channel, an orphan that is still killable, a durability failure that is its
//! own outcome, a claim that answers the floor, and the dead-birth rule. The
//! adoption and geometry properties are in `session_adoption.rs`; the
//! collaborators are in `session_support`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod session_support;

use roost_worker::browser_commands::session_lifecycle::{SessionLifecycle, SessionOutcome};
use roost_worker::session::respawn::classify_birth;
use roost_worker::strays::Stillborn;

use session_support::{Harness, NOW, OTHER, SESSION, session_id};

/// A SESSION THAT ENDS TWICE IS ONE CLOSE. The second kill finds no record, so
/// it takes the tombstone path — and a session this worker closed itself is not
/// tombstoned, because the coordinator already holds its `closed`.
#[test]
fn a_session_that_ends_twice_emits_one_close() {
    let harness = Harness::new();
    harness.install(SESSION, 7, "/home/user/project", "/home/user/project");
    assert_eq!(
        harness
            .manager
            .kill_held_session(&session_id(SESSION))
            .unwrap(),
        SessionOutcome::Killed
    );
    assert_eq!(
        harness.sink.closed_events(),
        1,
        "the close is published once"
    );
    assert_eq!(
        harness
            .manager
            .kill_held_session(&session_id(SESSION))
            .unwrap(),
        SessionOutcome::Killed,
        "a kill of a session this worker already ended is not an error"
    );
    assert_eq!(
        harness.sink.closed_events(),
        1,
        "a second kill must not publish a second close for one channel"
    );
    assert_eq!(
        harness
            .cells
            .lock()
            .expect("held")
            .forgotten
            .lock()
            .expect("held")
            .len(),
        1,
        "the channel's delivery is forgotten once, not twice"
    );
}

/// A SESSION THIS WORKER NEVER HELD still gets a tombstone, because a row whose
/// keeper died can never be closed from a browser otherwise.
#[test]
fn a_kill_of_an_orphan_publishes_a_tombstone() {
    let harness = Harness::new();
    assert_eq!(
        harness
            .manager
            .kill_held_session(&session_id(OTHER))
            .unwrap(),
        SessionOutcome::Killed
    );
    assert_eq!(harness.sink.closed_events(), 1);
    assert!(matches!(
        harness.sink.published().first(),
        Some(SessionEvent::Closed {
            exit_code: None,
            ..
        })
    ));
}

/// A CLOSE THAT CANNOT BE RECORDED IS ITS OWN OUTCOME, because no retry fixes a
/// coordinator that will believe a dead session is alive forever.
#[test]
fn an_unrecordable_close_reports_durability_lost() {
    let harness = Harness::new();
    harness.install(SESSION, 7, "/home/user/project", "/home/user/project");
    *harness.sink.fail_next.lock().expect("held") = true;
    assert_eq!(
        harness
            .manager
            .kill_held_session(&session_id(SESSION))
            .unwrap(),
        SessionOutcome::DurabilityLost
    );
    assert!(harness.sink.published().is_empty());
}

/// A CLAIM ANSWERS WITH THE FLOOR WHEN THE VIEWER FELL BELOW IT, because a
/// window this worker cannot address is not a window it may serve.
#[test]
fn a_claim_from_below_the_floor_answers_the_floor() {
    let harness = Harness::new();
    harness.install(SESSION, 7, "/home/user/project", "/home/user/project");
    harness
        .table
        .with_record_mut(&session_id(SESSION), |record| {
            record.append_retained(b"0123456789");
        })
        .expect("live");
    let answered = harness.manager.claim_viewer(&session_id(SESSION), Some(3));
    assert_eq!(
        answered.unwrap(),
        SessionOutcome::Attached { replay_offset: 3 }
    );
    let clamped = harness.manager.claim_viewer(&session_id(SESSION), Some(99));
    assert_eq!(
        clamped.unwrap(),
        SessionOutcome::Attached { replay_offset: 10 },
        "a viewer past the head is pulled back to it rather than told 99"
    );
    assert_eq!(harness.sink.closed_events(), 0, "a claim is not a close");
}

/// A SESSION THIS WORKER DOES NOT HOLD IS NOT CLAIMED. A survivor is adopted at
/// reconcile, which is the only place that knows which channel it was.
#[tokio::test]
async fn a_claim_of_a_session_this_worker_does_not_hold_is_refused() {
    let harness = Harness::new();
    let refused = harness
        .manager
        .attach(session_id(OTHER), None)
        .await
        .expect_err("there is no such session here");
    assert_eq!(
        refused.message(),
        Some("`attach`: this worker holds no session 00000000-0000-4000-8000-00000000cafe")
    );
}

/// A KILL ANSWERED OVER THE TRAIT IS THE SAME OUTCOME THE PATH REPORTS.
#[tokio::test]
async fn the_kill_arm_answers_with_what_the_close_path_did() {
    let harness = Harness::new();
    harness.install(SESSION, 7, "/home/user/project", "/home/user/project");
    assert_eq!(
        harness.manager.kill(session_id(SESSION)).await.unwrap(),
        SessionOutcome::Killed
    );
    assert_eq!(harness.sink.closed_events(), 1);
}

/// THE DEAD-BIRTH DISCRIMINATOR. `head_seq == 0` inside the window is stillborn;
/// the same record a moment later with a byte on it is not, because a real
/// shell prints a prompt before it exits; and a child that lived long enough is
/// ordinary whatever it printed.
#[test]
fn a_fast_child_with_no_output_is_stillborn_and_one_with_output_is_not() {
    let harness = Harness::new();
    harness.install(SESSION, 7, "/home/user/project", "/home/user/project");
    let silent = harness
        .table
        .with_record(&session_id(SESSION), |record| {
            classify_birth(record, NOW + 10)
        })
        .expect("live");
    assert_eq!(silent, Stillborn::Stillborn);

    harness
        .table
        .with_record_mut(&session_id(SESSION), |record| {
            record.append_retained(b"$ ");
        })
        .expect("live");
    let printed = harness
        .table
        .with_record(&session_id(SESSION), |record| {
            classify_birth(record, NOW + 10)
        })
        .expect("live");
    assert_eq!(
        printed,
        Stillborn::ProducedOutput,
        "a prompt before the exit is what separates a shell from a dead birth"
    );

    let long_lived = harness
        .table
        .with_record(&session_id(SESSION), |record| {
            classify_birth(record, NOW + 60_000)
        })
        .expect("live");
    assert_eq!(
        long_lived,
        Stillborn::LivedLongEnough,
        "a child that lived long enough is ordinary whatever it printed"
    );
}
