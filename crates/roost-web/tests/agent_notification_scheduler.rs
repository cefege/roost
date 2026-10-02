//! Which agent status transitions earn a card, and how a pending card survives
//! or dies across the changes that land before its timer fires. Mirrors
//! `crates/roost-web/src/components/notifications/agent_notifications/scheduler.rs`;
//! ports `apps/web/tests/agent-notification-core.test.ts`. The timer is the
//! test's: `take_due` is called where v2's fake clock reached the deadline.
//!
//! Test root, so the unwrap allowance is declared here (`CLAUDE.md` "the test
//! exemption reaches a test binary and not a fixture").

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_attention_support;

use agent_attention_support::{OTHER, agent_status};
use roost_client_core::client::agents::status_policy::agent_status_revision_token;
use roost_protocol::wire::{AgentOccupantId, AgentRuntimeState, AgentStatus, AgentStatusSource};
use roost_web::components::notifications::agent_notifications::scheduler::{
    AgentNotificationDelivery, AgentNotificationKind, AgentNotificationScheduler,
    ArmedNotification, classify_agent_transition,
};

fn status(state: AgentRuntimeState, revision: i64, completed_revision: i64) -> AgentStatus {
    agent_status(OTHER, state, revision, completed_revision)
}

/// The same occupant, now reported by screen detection instead of the
/// integration: a metadata change, not a lifecycle one.
fn screen_sourced(mut status: AgentStatus) -> AgentStatus {
    status.common.source = Some(AgentStatusSource::Screen);
    status
}

fn with_message(mut status: AgentStatus, message: &str) -> AgentStatus {
    status.common.message = Some(message.to_owned());
    status
}

fn replacement_occupant(mut status: AgentStatus) -> AgentStatus {
    status.common.occupant_id =
        Some(AgentOccupantId::try_from("bbbbbbbb-2222-4222-8222-bbbbbbbbbbbb").expect("a uuid"));
    status
}

fn legacy(mut status: AgentStatus) -> AgentStatus {
    status.common.status_epoch = None;
    status.common.occupant_id = None;
    status.common.source = None;
    status
}

/// Feed one observed change, as the component does, returning the armed timer.
fn publish(
    scheduler: &mut AgentNotificationScheduler,
    previous: &AgentStatus,
    next: Option<&AgentStatus>,
) -> Option<ArmedNotification> {
    scheduler.observe(OTHER, Some(previous), next, false)
}

#[test]
fn only_an_ordered_transition_of_one_occupant_earns_a_card() {
    let working = status(AgentRuntimeState::Working, 1, 0);
    let blocked = status(AgentRuntimeState::Blocked, 2, 0);
    let done = status(AgentRuntimeState::Idle, 3, 3);

    assert_eq!(
        classify_agent_transition(Some(&working), &blocked),
        Some(AgentNotificationKind::Blocked)
    );
    assert_eq!(
        classify_agent_transition(Some(&blocked), &done),
        Some(AgentNotificationKind::Done)
    );
    assert_eq!(
        classify_agent_transition(Some(&blocked), &blocked),
        None,
        "a blocked row read again is not a transition: re-reading it on every \
         store write must not raise the card again"
    );
    assert_eq!(
        classify_agent_transition(None, &blocked),
        None,
        "a first sighting is a level, not a transition"
    );
    assert_eq!(
        classify_agent_transition(Some(&working), &replacement_occupant(blocked.clone())),
        None,
        "a replacement agent's report says nothing about the previous occupant"
    );
    assert_eq!(
        classify_agent_transition(Some(&working), &status(AgentRuntimeState::Idle, 2, 1)),
        None,
        "an idle report whose completion is not its own revision announces nothing"
    );
}

#[test]
fn re_reading_a_pending_blocked_row_keeps_its_one_timer() {
    let mut scheduler = AgentNotificationScheduler::default();
    let working = status(AgentRuntimeState::Working, 1, 0);
    let blocked = status(AgentRuntimeState::Blocked, 2, 0);

    let armed = publish(&mut scheduler, &working, Some(&blocked)).expect("blocked arms a timer");
    assert_eq!(publish(&mut scheduler, &blocked, Some(&blocked)), None);
    assert_eq!(scheduler.pending_count(), 1);
    assert!(scheduler.take_due(&armed).is_some());
    assert_eq!(scheduler.take_due(&armed), None, "a delivery is spent once");
}

#[test]
fn keeps_the_original_blocked_timer_through_same_occupant_source_and_message_revisions() {
    let mut scheduler = AgentNotificationScheduler::default();
    let working = status(AgentRuntimeState::Working, 1, 0);
    let blocked = status(AgentRuntimeState::Blocked, 2, 0);
    let armed = publish(&mut scheduler, &working, Some(&blocked)).expect("blocked arms a timer");

    let source_update = screen_sourced(status(AgentRuntimeState::Blocked, 3, 0));
    assert_eq!(
        publish(&mut scheduler, &blocked, Some(&source_update)),
        None,
        "a metadata revision must not restart the debounce"
    );
    let message_update = with_message(
        screen_sourced(status(AgentRuntimeState::Blocked, 4, 0)),
        "still waiting",
    );
    assert_eq!(
        publish(&mut scheduler, &source_update, Some(&message_update)),
        None
    );

    assert_eq!(
        scheduler.take_due(&armed),
        Some(AgentNotificationDelivery {
            session_id: OTHER.to_owned(),
            token: agent_status_revision_token(&blocked),
            status_revision: 4,
            kind: AgentNotificationKind::Blocked,
            completed_revision: None,
        })
    );
}

#[test]
fn keeps_the_original_done_token_while_refreshing_the_current_status_revision() {
    let mut scheduler = AgentNotificationScheduler::default();
    let blocked = status(AgentRuntimeState::Blocked, 7, 0);
    let completed = status(AgentRuntimeState::Idle, 8, 8);
    let armed = publish(&mut scheduler, &blocked, Some(&completed)).expect("done arms a timer");

    let message_update = with_message(status(AgentRuntimeState::Idle, 9, 8), "summary ready");
    assert_eq!(
        publish(&mut scheduler, &completed, Some(&message_update)),
        None
    );
    let source_update = screen_sourced(with_message(
        status(AgentRuntimeState::Idle, 10, 8),
        "summary ready",
    ));
    assert_eq!(
        publish(&mut scheduler, &message_update, Some(&source_update)),
        None
    );

    let mut token = agent_status_revision_token(&completed);
    token.revision = 8;
    assert_eq!(
        scheduler.take_due(&armed),
        Some(AgentNotificationDelivery {
            session_id: OTHER.to_owned(),
            token,
            status_revision: 10,
            kind: AgentNotificationKind::Done,
            completed_revision: Some(8),
        })
    );
}

#[test]
fn cancels_pending_notifications_on_occupant_replacement_state_exit_and_retirement() {
    let working = status(AgentRuntimeState::Working, 1, 0);
    let blocked = status(AgentRuntimeState::Blocked, 2, 0);

    let mut replacement = AgentNotificationScheduler::default();
    let armed = publish(&mut replacement, &working, Some(&blocked)).expect("armed");
    let replaced = replacement_occupant(status(AgentRuntimeState::Blocked, 1, 0));
    assert_eq!(publish(&mut replacement, &blocked, Some(&replaced)), None);
    assert_eq!(replacement.pending_count(), 0);
    assert_eq!(replacement.take_due(&armed), None);

    let mut exit = AgentNotificationScheduler::default();
    let completed = status(AgentRuntimeState::Idle, 3, 3);
    let armed = publish(&mut exit, &blocked, Some(&completed)).expect("armed");
    let resumed = status(AgentRuntimeState::Working, 4, 3);
    assert_eq!(publish(&mut exit, &completed, Some(&resumed)), None);
    assert_eq!(exit.pending_count(), 0);
    assert_eq!(exit.take_due(&armed), None);

    let mut retirement = AgentNotificationScheduler::default();
    let armed = publish(&mut retirement, &working, Some(&blocked)).expect("armed");
    assert_eq!(publish(&mut retirement, &blocked, None), None);
    assert_eq!(retirement.pending_count(), 0);
    assert_eq!(retirement.take_due(&armed), None);
}

#[test]
fn does_not_carry_an_identityless_legacy_notification() {
    let mut scheduler = AgentNotificationScheduler::default();
    let working = legacy(status(AgentRuntimeState::Working, 1, 0));
    let blocked = legacy(status(AgentRuntimeState::Blocked, 2, 0));
    let armed = publish(&mut scheduler, &working, Some(&blocked)).expect("armed");
    let update = with_message(
        legacy(status(AgentRuntimeState::Blocked, 3, 0)),
        "legacy update",
    );

    assert_eq!(publish(&mut scheduler, &blocked, Some(&update)), None);
    assert_eq!(scheduler.pending_count(), 0);
    assert_eq!(scheduler.take_due(&armed), None);
}

#[test]
fn a_viewed_session_owes_no_card() {
    let mut scheduler = AgentNotificationScheduler::default();
    let working = status(AgentRuntimeState::Working, 1, 0);
    let blocked = status(AgentRuntimeState::Blocked, 2, 0);
    assert_eq!(
        scheduler.observe(OTHER, Some(&working), Some(&blocked), true),
        None
    );
    assert_eq!(scheduler.pending_count(), 0);
}
