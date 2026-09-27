//! The CLIENT half of the agent-status fence: what a report arriving on a
//! socket may and may not change about a row this browser is already showing.
//!
//! The coordinator fences the same reports over the same rule
//! (`roost_protocol::wire::agent_status::order`), and its fence cannot save
//! this side: a frame it admitted in order still reaches a browser out of order
//! after a reconnect, a retry or a second socket. These tests drive the client's
//! own copy of that rule directly, so a change to either end that breaks the
//! other is caught here.
//!
//! The property under test is never "the status updated". It is always "the
//! older report was dropped and the newer one won", asserted against the
//! retained row, the attention order, and the absence of a change record.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::client::agents::{
    AgentSeenLedger, AgentStatusProjection, derive_agent_status_level, AgentStatusLevel,
};
use roost_protocol::wire::{
    AgentId, AgentOccupantId, AgentRuntimeState, AgentStatus, AgentStatusFields, AgentStatusSource,
    AgentStatusUpdate, SessionId, StatusEpoch,
};

const SESSION_A: &str = "30000000-0000-4000-8000-000000000030";
const SESSION_B: &str = "10000000-0000-4000-8000-000000000010";
const EPOCH_A: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const OCCUPANT_A: &str = "11111111-aaaa-4aaa-8aaa-111111111111";
const OCCUPANT_B: &str = "22222222-aaaa-4aaa-8aaa-222222222222";

fn session_id(value: &str) -> SessionId {
    SessionId::try_from(value).expect("a session id")
}

fn base_fields(session: &str, revision: i64, state: AgentRuntimeState) -> AgentStatusFields {
    AgentStatusFields {
        session_id: session_id(session),
        agent_id: AgentId::try_from("omp").expect("an agent id"),
        state,
        message: None,
        revision,
        completed_revision: 0,
        updated_at: 1_800_000_000_000,
        status_epoch: Some(StatusEpoch::try_from(EPOCH_A).expect("an epoch")),
        occupant_id: Some(AgentOccupantId::try_from(OCCUPANT_A).expect("an occupant")),
        source: Some(AgentStatusSource::Integration),
        occupant_exited: false,
    }
}

/// An identified report, the shape a durable worker sends.
fn report(session: &str, revision: i64, state: AgentRuntimeState) -> AgentStatusUpdate {
    AgentStatusUpdate {
        common: base_fields(session, revision, state),
        active: true,
    }
}

/// An identified report from a replacement occupant of the same epoch.
fn replacement_report(
    session: &str,
    revision: i64,
    state: AgentRuntimeState,
) -> AgentStatusUpdate {
    let mut update = report(session, revision, state);
    update.common.occupant_id = Some(AgentOccupantId::try_from(OCCUPANT_B).expect("an occupant"));
    update
}

/// A report from a deployment predating durable observation: no identity.
fn legacy_report(session: &str, revision: i64, state: AgentRuntimeState) -> AgentStatusUpdate {
    let mut update = report(session, revision, state);
    update.common.status_epoch = None;
    update.common.occupant_id = None;
    update.common.source = None;
    update
}

/// A deletion, the update that removes a retained row.
fn deletion(session: &str, revision: i64) -> AgentStatusUpdate {
    AgentStatusUpdate {
        common: base_fields(session, revision, AgentRuntimeState::Idle),
        active: false,
    }
}

fn retained(projection: &AgentStatusProjection, session: &str) -> Option<AgentStatus> {
    projection.status(&session_id(session)).cloned()
}

fn state_of(projection: &AgentStatusProjection, session: &str) -> Option<(AgentRuntimeState, i64)> {
    retained(projection, session)
        .map(|status| (status.common.state, status.common.revision))
}

#[test]
fn a_late_report_was_dropped_and_the_newer_one_won() {
    let seen = AgentSeenLedger::new();
    let mut projection = AgentStatusProjection::new();
    let fresh = report(SESSION_A, 90, AgentRuntimeState::Idle);
    assert!(
        projection.apply_update(&fresh, &seen).is_some(),
        "the fresh report is the first one and must be admitted"
    );

    let late = report(SESSION_A, 89, AgentRuntimeState::Blocked);
    assert_eq!(
        projection.apply_update(&late, &seen),
        None,
        "a report one revision behind the retained row is stale, and a dropped \
         report is not a change"
    );
    assert_eq!(
        state_of(&projection, SESSION_A),
        Some((AgentRuntimeState::Idle, 90)),
        "the late report displaced the fresh one"
    );
}

#[test]
fn a_duplicate_report_at_the_same_revision_is_idempotent() {
    let seen = AgentSeenLedger::new();
    let mut projection = AgentStatusProjection::new();
    let first = report(SESSION_A, 7, AgentRuntimeState::Working);
    let change = projection
        .apply_update(&first, &seen)
        .expect("the first report is admitted");
    let arrival_after_first = projection.arrival(&session_id(SESSION_A));
    let rows_after_first = projection.statuses().clone();

    // A socket that reconnects re-sends what it already sent. Same revision,
    // same occupant: one moment observed twice.
    let resend = report(SESSION_A, 7, AgentRuntimeState::Working);
    assert_eq!(
        projection.apply_update(&resend, &seen),
        None,
        "a resend at the retained revision is not progress"
    );
    assert_eq!(
        projection.statuses(),
        &rows_after_first,
        "a resend changed the retained rows"
    );
    assert_eq!(
        projection.arrival(&session_id(SESSION_A)),
        arrival_after_first,
        "a resend took a second place in the attention order"
    );
    assert_eq!(change.revision, 7);
}

#[test]
fn the_retained_state_follows_revision_order_not_arrival_order() {
    let seen = AgentSeenLedger::new();
    let mut projection = AgentStatusProjection::new();
    for (revision, state) in [
        (3, AgentRuntimeState::Working),
        (12, AgentRuntimeState::Blocked),
        (7, AgentRuntimeState::Idle),
        (11, AgentRuntimeState::Working),
    ] {
        let update = report(SESSION_A, revision, state);
        let admitted = projection.apply_update(&update, &seen).is_some();
        assert_eq!(
            admitted,
            matches!(revision, 3 | 12),
            "only revisions above the retained high water mark are admitted"
        );
    }
    assert_eq!(
        state_of(&projection, SESSION_A),
        Some((AgentRuntimeState::Blocked, 12)),
        "the retained row must be the HIGHEST revision, whatever order the \
         socket delivered them in"
    );
    assert_eq!(
        projection.arrival(&session_id(SESSION_A)),
        2,
        "only admitted reports take an arrival number, so the attention order \
         counts changes rather than frames"
    );
}

#[test]
fn a_replacement_occupant_is_not_a_stale_report() {
    let seen = AgentSeenLedger::new();
    let mut projection = AgentStatusProjection::new();
    let first = report(SESSION_A, 90, AgentRuntimeState::Idle);
    projection.apply_update(&first, &seen);

    // A replacement occupant numbers its revisions from 1, so a revision
    // comparison across occupants would refuse this as stale forever.
    let replacement = replacement_report(SESSION_A, 1, AgentRuntimeState::Working);
    assert!(
        projection.apply_update(&replacement, &seen).is_some(),
        "a new occupant is not a stale report"
    );
    assert_eq!(
        state_of(&projection, SESSION_A),
        Some((AgentRuntimeState::Working, 1))
    );

    // And the retired occupant cannot come back at its own high revision.
    let resurrected = report(SESSION_A, 91, AgentRuntimeState::Blocked);
    assert_eq!(
        projection.apply_update(&resurrected, &seen),
        None,
        "the occupant the replacement retired must stay retired"
    );
    assert_eq!(
        state_of(&projection, SESSION_A),
        Some((AgentRuntimeState::Working, 1))
    );
}

#[test]
fn a_legacy_report_is_refused_once_an_identified_occupant_was_accepted() {
    let seen = AgentSeenLedger::new();
    let mut projection = AgentStatusProjection::new();
    let legacy = legacy_report(SESSION_A, 40, AgentRuntimeState::Working);
    assert!(
        projection.apply_update(&legacy, &seen).is_some(),
        "a legacy deployment still reports, and nothing has been identified yet"
    );
    let identified = report(SESSION_A, 1, AgentRuntimeState::Idle);
    projection.apply_update(&identified, &seen);

    let late_legacy = legacy_report(SESSION_A, 41, AgentRuntimeState::Blocked);
    assert_eq!(
        projection.apply_update(&late_legacy, &seen),
        None,
        "a legacy report yields permanently once an identified occupant answered"
    );
    assert_eq!(
        state_of(&projection, SESSION_A),
        Some((AgentRuntimeState::Idle, 1))
    );
}

#[test]
fn a_closed_session_refuses_every_report_until_an_upsert_reopens_it() {
    let seen = AgentSeenLedger::new();
    let mut projection = AgentStatusProjection::new();
    let first = report(SESSION_A, 5, AgentRuntimeState::Working);
    projection.apply_update(&first, &seen);
    assert!(projection.clear_session(&session_id(SESSION_A)).is_some());

    let in_flight = report(SESSION_A, 6, AgentRuntimeState::Blocked);
    assert_eq!(
        projection.apply_update(&in_flight, &seen),
        None,
        "a report that was in flight when the session closed must not \
         resurrect a row for a session the durable plane removed"
    );
    assert!(projection.status(&session_id(SESSION_A)).is_none());

    projection.mark_session_open(&session_id(SESSION_A));
    let reopened = report(SESSION_A, 6, AgentRuntimeState::Blocked);
    assert!(
        projection.apply_update(&reopened, &seen).is_some(),
        "an authoritative upsert starts a fresh lifecycle fence"
    );
}

#[test]
fn a_hydrated_seed_is_not_read_as_progress_by_the_next_frame() {
    let seen = AgentSeenLedger::new();
    let seeded = AgentStatusUpdate {
        common: base_fields(SESSION_A, 20, AgentRuntimeState::Idle),
        active: true,
    };
    let mut projection = AgentStatusProjection::seeded(
        [(
            session_id(SESSION_A),
            AgentStatus {
                common: seeded.common.clone(),
                active: true,
            },
        )]
        .into_iter()
        .collect(),
    );

    let resend = seeded.clone();
    assert_eq!(
        projection.apply_update(&resend, &seen),
        None,
        "re-sending a hydrated row verbatim is a resend, not progress"
    );

    let newer = report(SESSION_A, 21, AgentRuntimeState::Working);
    assert!(projection.apply_update(&newer, &seen).is_some());
    assert_eq!(
        state_of(&projection, SESSION_A),
        Some((AgentRuntimeState::Working, 21))
    );
}

#[test]
fn a_deletion_removes_the_row_and_leaves_the_revision_floor_behind_it() {
    let seen = AgentSeenLedger::new();
    let mut projection = AgentStatusProjection::new();
    projection.apply_update(&report(SESSION_A, 30, AgentRuntimeState::Working), &seen);

    let removed = projection.apply_update(&deletion(SESSION_A, 31), &seen);
    let change = removed.expect("a deletion is a change");
    assert_eq!(change.previous.map(|status| status.common.revision), Some(30));
    assert!(change.next.is_none());
    assert!(projection.status(&session_id(SESSION_A)).is_none());
    assert_eq!(
        projection.arrival(&session_id(SESSION_A)),
        0,
        "a removed row takes no place in the attention order"
    );

    let late = report(SESSION_A, 31, AgentRuntimeState::Blocked);
    assert_eq!(
        projection.apply_update(&late, &seen),
        None,
        "the occupant a deletion retired must not come back"
    );
}

#[test]
fn a_released_occupants_row_is_spent_once_its_completion_is_acknowledged() {
    let mut seen = AgentSeenLedger::new();
    let mut projection = AgentStatusProjection::new();
    let mut idle = report(SESSION_A, 4, AgentRuntimeState::Idle);
    idle.common.completed_revision = 4;
    idle.common.occupant_exited = true;
    projection.apply_update(&idle, &seen);

    let held = retained(&projection, SESSION_A).expect("the row is retained");
    assert_eq!(
        derive_agent_status_level(Some(&held), Some(seen.acknowledged_revision(&held))),
        AgentStatusLevel::Done,
        "an unacknowledged completion is the reason the row is still here"
    );
    assert_eq!(projection.spent_released_count(&seen), 0);

    assert!(seen.mark_seen(&held), "the first acknowledgement moves the ledger");
    assert_eq!(projection.spent_released_count(&seen), 1);
    let retired = projection.retire_spent_released(&seen);
    assert_eq!(retired.len(), 1);
    assert_eq!(retired[0].previous.map(|status| status.common.revision), Some(4));
    assert!(projection.status(&session_id(SESSION_A)).is_none());
}

#[test]
fn a_second_sessions_row_is_untouched_by_another_sessions_report() {
    let seen = AgentSeenLedger::new();
    let mut projection = AgentStatusProjection::new();
    projection.apply_update(&report(SESSION_A, 1, AgentRuntimeState::Working), &seen);
    projection.apply_update(&report(SESSION_B, 99, AgentRuntimeState::Blocked), &seen);

    let arrival_a = projection.arrival(&session_id(SESSION_A));
    let late_b = report(SESSION_B, 98, AgentRuntimeState::Idle);
    assert_eq!(projection.apply_update(&late_b, &seen), None);

    assert_eq!(state_of(&projection, SESSION_A), Some((AgentRuntimeState::Working, 1)));
    assert_eq!(
        projection.arrival(&session_id(SESSION_A)),
        arrival_a,
        "one session's stale report must not reorder another session's attention \
         position"
    );
    assert_eq!(
        state_of(&projection, SESSION_B),
        Some((AgentRuntimeState::Blocked, 99))
    );
}
