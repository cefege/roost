//! What a browser does with the SECOND report one occupant makes, from the
//! coordinator's wire bytes rather than a hand-built update.
//!
//! `agent_status_ordering.rs` starts at the projection; this starts at the
//! frame, because a field lost in transit is a field the fence reads as a
//! different occupant. The values are the ones a measured run put on a Sync
//! socket, and the pinned behaviour is the one
//! `smoke/terminal/agent-status.spec.ts:91` and the toast
//! `smoke/terminal/toast-target.spec.ts:49` both depend on.
//!
//! Depends on `roost_proto` and `roost_client_core::sync::decode`; adds no state.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::ClientEvent;
use roost_client_core::SyncFrame;
use roost_client_core::client::agents::status_policy::{
    AgentStatusLevel, agent_status_level_token_for, derive_agent_status_level,
};
use roost_client_core::client::agents::{
    AgentSeenLedger, AgentStatusProjection, agent_status_completion_unseen,
};
use roost_client_core::sync::decode::{SyncFrameMeta, decode_firehose};
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::buffa::{EnumValue, Message};
use roost_proto::{AgentStatusFrame, FirehoseFrame, SyncDomain};
use roost_protocol::wire::{
    AgentId, AgentOccupantId, AgentRuntimeState, AgentStatusFields, AgentStatusSource,
    AgentStatusUpdate, SessionId, StatusEpoch,
};

/// The session the measured run reported into.
const SESSION: &str = "7667be15-f846-41b8-b746-9d6edac63526";
/// The worker's status epoch for that run.
const EPOCH: &str = "ca9d16cc-7d29-4c34-8ae9-963f514fd296";
/// The occupant the process scan proved, which then reported twice.
const OCCUPANT: &str = "54522cf6-ed3d-4942-9f48-62a16fefc365";
/// The occupant a replacement agent takes over from.
const REPLACEMENT: &str = "6b1d4c0a-9f38-4a71-8c22-0f4b1d6e77aa";

/// The measured run's three revisions, in the wall-clock microseconds
/// `nextRevision` counts in — a report is never "revision 2" to the fence.
const SCREEN_IDLE_REVISION: u64 = 1_790_860_764_395_000;
const WORKING_REVISION: u64 = 1_790_860_767_450_000;
const BLOCKED_REVISION: u64 = 1_790_860_771_659_000;
const BLOCKED_MESSAGE: &str = "Approval needed";

/// The terminal-domain generation that run's frames were stamped with.
const DOMAIN_GENERATION: u64 = 1_833_842_094_059_521;

/// The coordinator's wire bytes for one report. `delivery_seq` and `domain`
/// are the v2 metadata `sync::decode::check_meta` refuses an application arm
/// without, so a frame built lacking them would test the meta rule, not the fold.
fn wire_bytes(update: &AgentStatusUpdate, delivery_seq: u64) -> Vec<u8> {
    let common = &update.common;
    let frame = FirehoseFrame {
        delivery_seq,
        domain_generation: DOMAIN_GENERATION,
        domain: EnumValue::Known(SyncDomain::Terminal),
        frame: Some(Frame::AgentStatus(Box::new(AgentStatusFrame {
            session_id: common.session_id.as_str().to_owned(),
            agent_id: common.agent_id.as_str().to_owned(),
            state: common.state.as_str().to_owned(),
            message: common.message.clone(),
            revision: common.revision.unsigned_abs(),
            completed_revision: common.completed_revision.unsigned_abs(),
            updated_at: common.updated_at as f64,
            active: update.active,
            status_epoch: common.status_epoch.as_ref().map(|e| e.as_str().to_owned()),
            occupant_id: common.occupant_id.as_ref().map(|o| o.as_str().to_owned()),
            source: common.source.map(|source| source.as_str().to_owned()),
            occupant_exited: common.occupant_exited,
            ..AgentStatusFrame::default()
        }))),
        ..FirehoseFrame::default()
    };
    frame.encode_to_vec()
}

fn session_id() -> SessionId {
    SessionId::try_from(SESSION).expect("a session id")
}

fn fields(
    revision: i64,
    state: AgentRuntimeState,
    message: Option<&str>,
    occupant: &str,
) -> AgentStatusFields {
    AgentStatusFields {
        session_id: session_id(),
        agent_id: AgentId::try_from("omp").expect("an agent id"),
        state,
        message: message.map(str::to_owned),
        revision,
        completed_revision: 0,
        updated_at: 1_790_860_767_450,
        status_epoch: Some(StatusEpoch::try_from(EPOCH).expect("an epoch")),
        occupant_id: Some(AgentOccupantId::try_from(occupant).expect("an occupant")),
        source: Some(AgentStatusSource::Integration),
        occupant_exited: false,
    }
}

/// One identified report, the shape a durable worker publishes.
fn report(revision: i64, state: AgentRuntimeState, message: Option<&str>) -> AgentStatusUpdate {
    AgentStatusUpdate {
        common: fields(revision, state, message, OCCUPANT),
        active: true,
    }
}

/// The screen report the process scan produced before the agent ever spoke:
/// same occupant, no message, and `screen` rather than `integration`.
fn screen_report(revision: i64) -> AgentStatusUpdate {
    let mut update = report(revision, AgentRuntimeState::Idle, None);
    update.common.source = Some(AgentStatusSource::Screen);
    update
}

/// A report from a replacement occupant of the same epoch, which the worker
/// publishes under its OWN revision numbering.
fn replacement_report(revision: i64, state: AgentRuntimeState) -> AgentStatusUpdate {
    AgentStatusUpdate {
        common: fields(revision, state, None, REPLACEMENT),
        active: true,
    }
}

/// The update one wire message carried, decoded by the client's own decoder.
fn decode(update: &AgentStatusUpdate, delivery_seq: u64) -> AgentStatusUpdate {
    let event = decode_firehose(
        &wire_bytes(update, delivery_seq),
        SyncFrameMeta { generation: 7 },
    )
    .expect("the coordinator's own frame shape decodes");
    match event {
        ClientEvent::SyncFrameReceived {
            frame: SyncFrame::AgentStatus { update },
            ..
        } => update,
        other => panic!("expected an agent-status frame, got {other:?}"),
    }
}

/// Fold one wire message into a projection, reporting whether it changed a row.
fn fold(
    projection: &mut AgentStatusProjection,
    seen: &AgentSeenLedger,
    update: &AgentStatusUpdate,
    delivery_seq: u64,
) -> bool {
    projection
        .apply_update(&decode(update, delivery_seq), seen)
        .is_some()
}

fn level_token(projection: &AgentStatusProjection, seen: &AgentSeenLedger) -> &'static str {
    let status = projection.status(&session_id()).expect("a retained row");
    let acknowledged = Some(seen.acknowledged_revision(status));
    agent_status_level_token_for(Some(status), acknowledged)
}

/// The measured run's whole sequence, decoded from wire bytes: the screen
/// `idle`, the agent's `working`, then its `blocked`.
///
/// The pinned behaviour is that the SECOND report REPLACES the row an occupant
/// the browser already holds. A fold that ignored it — or read it as a
/// different occupant because a field went missing in transit — leaves the
/// level at `working` and no attention surface ever fires.
#[test]
fn a_second_report_from_the_same_occupant_moves_the_level_from_working_to_blocked() {
    let seen = AgentSeenLedger::new();
    let mut projection = AgentStatusProjection::new();

    assert!(
        fold(
            &mut projection,
            &seen,
            &screen_report(SCREEN_IDLE_REVISION as i64),
            1
        ),
        "the process scan's report is the first one and must be admitted"
    );
    assert!(fold(
        &mut projection,
        &seen,
        &report(WORKING_REVISION as i64, AgentRuntimeState::Working, None),
        2
    ));
    assert_eq!(
        level_token(&projection, &seen),
        "working",
        "the agent's own report is what the surfaces show"
    );

    let arrival_after_working = projection.arrival(&session_id());
    assert!(
        fold(
            &mut projection,
            &seen,
            &report(
                BLOCKED_REVISION as i64,
                AgentRuntimeState::Blocked,
                Some(BLOCKED_MESSAGE),
            ),
            3,
        ),
        "a second report from the same occupant at a higher revision is progress, \
         not a duplicate"
    );

    let blocked = projection.status(&session_id()).expect("a retained row");
    assert_eq!(blocked.common.state, AgentRuntimeState::Blocked);
    assert_eq!(blocked.common.message.as_deref(), Some(BLOCKED_MESSAGE));
    assert_eq!(
        blocked
            .common
            .occupant_id
            .as_ref()
            .map(AgentOccupantId::as_str),
        Some(OCCUPANT),
        "the occupant identity survived the wire, or the fence is reading a \
         different occupant than the one that reported"
    );
    assert_eq!(level_token(&projection, &seen), "blocked");
    assert!(
        projection.arrival(&session_id()) > arrival_after_working,
        "a replacing report takes the next place in the attention order; an \
         ignored one would leave the row where the first report put it"
    );
}

/// The change record itself: the second report REPLACED the row rather than
/// being folded over it, and the row it replaced is the one a viewer saw.
#[test]
fn the_second_reports_change_record_carries_the_row_it_replaced() {
    let seen = AgentSeenLedger::new();
    let mut projection = AgentStatusProjection::new();
    fold(
        &mut projection,
        &seen,
        &report(WORKING_REVISION as i64, AgentRuntimeState::Working, None),
        1,
    );

    let change = projection
        .apply_update(
            &decode(
                &report(
                    BLOCKED_REVISION as i64,
                    AgentRuntimeState::Blocked,
                    Some(BLOCKED_MESSAGE),
                ),
                2,
            ),
            &seen,
        )
        .expect("the second report changes the row");
    assert_eq!(
        change.previous.map(|status| status.common.state),
        Some(AgentRuntimeState::Working),
        "a viewer who saw `working` must be able to be told it is no longer true"
    );
    assert_eq!(
        change.next.map(|status| status.common.state),
        Some(AgentRuntimeState::Blocked)
    );
    assert_eq!(change.revision, BLOCKED_REVISION as i64);
}

/// A report at or below the retained revision changes nothing at all: not the
/// state, not the message a tooltip shows, not the attention order.
///
/// The worker republishes an occupant whenever its message or its authority
/// source changes, so the refusal must be total.
#[test]
fn a_late_or_repeated_report_leaves_the_held_row_untouched() {
    let seen = AgentSeenLedger::new();
    let mut projection = AgentStatusProjection::new();
    fold(
        &mut projection,
        &seen,
        &report(
            BLOCKED_REVISION as i64,
            AgentRuntimeState::Blocked,
            Some(BLOCKED_MESSAGE),
        ),
        1,
    );
    let held = projection
        .status(&session_id())
        .cloned()
        .expect("a retained row");
    let arrival = projection.arrival(&session_id());

    for (label, revision) in [
        ("one revision behind", BLOCKED_REVISION as i64 - 1),
        ("at the retained revision", BLOCKED_REVISION as i64),
    ] {
        let stale = report(revision, AgentRuntimeState::Working, None);
        assert!(
            !fold(&mut projection, &seen, &stale, 2),
            "a report {label} must be refused"
        );
        assert_eq!(
            projection.status(&session_id()),
            Some(&held),
            "a refused report must not disturb the row a viewer is reading"
        );
        assert_eq!(
            projection.arrival(&session_id()),
            arrival,
            "a refused report must not take a place in the attention order"
        );
    }
    assert_eq!(level_token(&projection, &seen), "blocked");
}

/// A replacement agent publishes TWO frames: the occupant it replaces going
/// inactive, then the new one active at its own low revision. Both must land.
#[test]
fn a_replacement_occupants_inactive_then_active_pair_both_land() {
    let seen = AgentSeenLedger::new();
    let mut projection = AgentStatusProjection::new();
    fold(
        &mut projection,
        &seen,
        &report(90, AgentRuntimeState::Working, None),
        1,
    );

    let retired = AgentStatusUpdate {
        common: fields(91, AgentRuntimeState::Idle, None, OCCUPANT),
        active: false,
    };
    assert!(
        fold(&mut projection, &seen, &retired, 2),
        "the occupant being replaced must be able to release the row"
    );
    assert!(
        projection.status(&session_id()).is_none(),
        "an inactive report removes the row rather than freezing it"
    );

    assert!(
        fold(
            &mut projection,
            &seen,
            &replacement_report(1, AgentRuntimeState::Blocked),
            3
        ),
        "a new occupant numbers its revisions from 1, so it must not be read as \
         stale against the previous occupant's high water mark"
    );
    assert_eq!(level_token(&projection, &seen), "blocked");

    let resurrected = report(92, AgentRuntimeState::Blocked, None);
    assert!(
        !fold(&mut projection, &seen, &resurrected, 4),
        "the occupant a replacement retired must stay retired"
    );
}

/// A report whose occupant this profile has never acknowledged is retained, and
/// the completion it carries reads as news.
///
/// The floor for an identified occupant with no acknowledgement is BELOW every
/// real revision; the opposite floor is what a legacy status gets, and getting
/// it backwards turns every first completion into a Done nobody can clear.
#[test]
fn a_report_from_an_unacknowledged_occupant_lands_and_its_completion_is_unseen() {
    let mut seen = AgentSeenLedger::new();
    let mut projection = AgentStatusProjection::new();
    let mut finished = report(4, AgentRuntimeState::Idle, None);
    finished.common.completed_revision = 4;
    finished.common.occupant_exited = true;

    assert!(fold(&mut projection, &seen, &finished, 1));
    let held = projection.status(&session_id()).expect("a retained row");
    assert!(
        agent_status_completion_unseen(held, Some(seen.acknowledged_revision(held))),
        "an identified occupant's first completion is the one this profile can \
         have missed"
    );
    assert_eq!(
        derive_agent_status_level(Some(held), Some(seen.acknowledged_revision(held))),
        AgentStatusLevel::Done
    );
    assert!(
        seen.mark_seen(held),
        "acknowledging it is what makes the row spent"
    );
    assert_eq!(
        projection.retire_spent_released(&seen).len(),
        1,
        "an acknowledged completion outlives the agent that earned it only until \
         this profile has been told about it"
    );
}
