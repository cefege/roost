//! Durable observed-agent identity contracts for the worker registry: private
//! pid continuity, ordered replacement tombstones, retired reporter fencing,
//! and reconnect snapshots without a pid on any wire status. Ports v2
//! `apps/worker/tests/agents/agent-status-registry-identity.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_status_support;

use agent_status_support::{
    OTHER_SESSION_ID, SESSION_ID, assert_no_process_id, registry_harness, session,
};
use roost_protocol::wire::agent_status::{AgentRuntimeState as State, AgentStatusSource as Source};
use roost_worker::agents::BuiltinAgentId as Agent;
use roost_worker::agents::registry::{IntegrationStatusReport, ScreenStatusReport};

fn screen(agent_id: Agent, process_id: u32, state: State) -> ScreenStatusReport {
    ScreenStatusReport {
        agent_id,
        process_id,
        state,
        visible_blocker: false,
    }
}

fn integration(
    agent_id: Agent,
    process_id: u32,
    state: State,
    seq: u64,
    active: bool,
) -> IntegrationStatusReport {
    IntegrationStatusReport {
        session_id: session(SESSION_ID),
        agent_id,
        process_id,
        state,
        message: None,
        seq,
        active,
    }
}

fn with_message(mut report: IntegrationStatusReport, message: &str) -> IntegrationStatusReport {
    report.message = Some(message.to_owned());
    report
}

#[test]
fn source_state_and_message_changes_retain_one_occupant_for_one_process() {
    let harness = registry_harness(1_000);
    let (registry, published) = (&harness.registry, &harness.published);
    registry.report_screen(
        &session(SESSION_ID),
        screen(Agent::Omp, 101, State::Working),
    );
    let screen_working = published.last();
    assert!(screen_working.active);
    assert_eq!(screen_working.common.source, Some(Source::Screen));
    assert_eq!(screen_working.common.state, State::Working);
    assert_eq!(screen_working.common.completed_revision, 0);

    assert!(registry.report_integration(integration(Agent::Omp, 101, State::Working, 1, true)));
    let source_only = published.last();
    assert_eq!(source_only.common.source, Some(Source::Integration));
    assert_eq!(source_only.common.state, screen_working.common.state);
    assert_eq!(
        source_only.common.status_epoch,
        screen_working.common.status_epoch
    );
    assert_eq!(
        source_only.common.occupant_id,
        screen_working.common.occupant_id
    );

    let blocked = with_message(
        integration(Agent::Omp, 101, State::Blocked, 2, true),
        "approval needed",
    );
    assert!(registry.report_integration(blocked));
    let integrated = published.last();
    assert!(integrated.active);
    assert_eq!(integrated.common.source, Some(Source::Integration));
    assert_eq!(integrated.common.state, State::Blocked);
    assert_eq!(
        integrated.common.message.as_deref(),
        Some("approval needed")
    );
    assert_eq!(
        integrated.common.occupant_id,
        screen_working.common.occupant_id
    );

    let idle = with_message(
        integration(Agent::Omp, 101, State::Idle, 3, true),
        "complete",
    );
    assert!(registry.report_integration(idle));
    let integrated_idle = published.last();
    assert_eq!(
        integrated_idle.common.occupant_id,
        screen_working.common.occupant_id
    );
    assert_eq!(
        integrated_idle.common.completed_revision,
        integrated_idle.common.revision
    );

    let before_screen = published.len();
    registry.report_screen(
        &session(SESSION_ID),
        screen(Agent::Omp, 101, State::Working),
    );
    assert_eq!(
        published.len(),
        before_screen,
        "a live integration hides the screen"
    );
    harness.clock.advance(101);
    registry.expire_leases();
    let screen_fallback = published.last();
    assert!(screen_fallback.active);
    assert_eq!(screen_fallback.common.source, Some(Source::Screen));
    assert_eq!(screen_fallback.common.state, State::Working);
    assert_eq!(
        screen_fallback.common.status_epoch,
        screen_working.common.status_epoch
    );
    assert_eq!(
        screen_fallback.common.occupant_id,
        screen_working.common.occupant_id
    );
    assert_eq!(
        screen_fallback.common.completed_revision,
        integrated_idle.common.completed_revision
    );
    assert_eq!(screen_fallback.common.message, None);
    for status in published.all() {
        assert_no_process_id(&status);
    }
    registry.dispose();
}

#[test]
fn each_registry_has_one_epoch_and_separate_registries_have_different_epochs() {
    let first = registry_harness(1_000);
    let second = registry_harness(1_000);
    first.registry.report_screen(
        &session(SESSION_ID),
        screen(Agent::Codex, 11, State::Working),
    );
    first.registry.report_screen(
        &session(OTHER_SESSION_ID),
        screen(Agent::Pi, 12, State::Idle),
    );
    second.registry.report_screen(
        &session(SESSION_ID),
        screen(Agent::Codex, 11, State::Working),
    );

    let (first_all, second_all) = (first.published.all(), second.published.all());
    assert_eq!(
        first_all[0].common.status_epoch,
        first_all[1].common.status_epoch
    );
    assert_ne!(
        first_all[0].common.status_epoch,
        second_all[0].common.status_epoch
    );
}

#[test]
fn new_pid_publishes_exact_old_inactive_before_fresh_idle_occupant() {
    let harness = registry_harness(1_000);
    let (registry, published) = (&harness.registry, &harness.published);
    let old = with_message(
        integration(Agent::Omp, 100, State::Working, 900, true),
        "old process",
    );
    assert!(registry.report_integration(old));
    let old_active = published.last();

    assert!(registry.report_integration(integration(Agent::Omp, 200, State::Idle, 0, true)));
    let (old_inactive, new_active) = (published.from_end(2), published.from_end(1));
    assert!(!old_inactive.active);
    assert_eq!(old_inactive.common.agent_id, old_active.common.agent_id);
    assert_eq!(old_inactive.common.state, old_active.common.state);
    assert_eq!(old_inactive.common.message, old_active.common.message);
    assert_eq!(
        old_inactive.common.status_epoch,
        old_active.common.status_epoch
    );
    assert_eq!(
        old_inactive.common.occupant_id,
        old_active.common.occupant_id
    );
    assert_eq!(old_inactive.common.source, old_active.common.source);
    assert_eq!(
        old_inactive.common.completed_revision,
        old_active.common.completed_revision
    );
    assert!(old_inactive.common.revision > old_active.common.revision);
    assert!(new_active.active);
    assert_eq!(new_active.common.state, State::Idle);
    assert_eq!(
        new_active.common.status_epoch,
        old_active.common.status_epoch
    );
    assert_eq!(new_active.common.completed_revision, 0);
    assert_ne!(new_active.common.occupant_id, old_active.common.occupant_id);
    assert!(new_active.common.revision > old_inactive.common.revision);
}

#[test]
fn agent_kind_change_at_one_numeric_pid_is_also_a_replacement() {
    let harness = registry_harness(1_000);
    let (registry, published) = (&harness.registry, &harness.published);
    registry.report_screen(
        &session(SESSION_ID),
        screen(Agent::Omp, 300, State::Working),
    );
    let old_occupant = published.last().common.occupant_id;
    registry.report_screen(&session(SESSION_ID), screen(Agent::Pi, 300, State::Working));

    assert!(!published.from_end(2).active);
    assert!(published.from_end(1).active);
    assert_eq!(published.from_end(2).common.occupant_id, old_occupant);
    assert_ne!(published.from_end(1).common.occupant_id, old_occupant);
}

#[test]
fn a_replacement_reporter_resets_sequence_while_a_retired_reporter_cannot_reclaim() {
    let harness = registry_harness(1_000);
    let (registry, published) = (&harness.registry, &harness.published);
    registry.report_integration(integration(Agent::Omp, 401, State::Working, 50_000, true));
    assert!(registry.report_integration(integration(Agent::Omp, 402, State::Blocked, 0, true)));
    let replacement = published.last();
    let count_after_replacement = published.len();

    assert!(!registry.report_integration(integration(Agent::Omp, 401, State::Idle, 50_001, true)));
    assert!(!registry.report_integration(integration(
        Agent::Omp,
        401,
        State::Working,
        50_002,
        false
    )));
    assert_eq!(published.len(), count_after_replacement);
    assert_eq!(
        registry.snapshot()[0].common.occupant_id,
        replacement.common.occupant_id
    );
}

#[test]
fn disappearance_followed_by_the_same_numeric_pid_mints_a_new_occupant() {
    let harness = registry_harness(1_000);
    let (registry, published) = (&harness.registry, &harness.published);
    registry.report_screen(
        &session(SESSION_ID),
        screen(Agent::Codex, 501, State::Working),
    );
    let first = published.last();
    registry.clear_screen(&session(SESSION_ID));
    let exited = published.last();
    assert!(exited.active);
    assert_eq!(exited.common.state, State::Idle);
    assert_eq!(exited.common.occupant_id, first.common.occupant_id);

    registry.report_screen(&session(SESSION_ID), screen(Agent::Codex, 501, State::Idle));
    let (retired, reappeared) = (published.from_end(2), published.from_end(1));
    assert!(!retired.active);
    assert_eq!(retired.common.occupant_id, first.common.occupant_id);
    assert_eq!(
        retired.common.completed_revision,
        exited.common.completed_revision
    );
    assert!(reappeared.active);
    assert_eq!(reappeared.common.completed_revision, 0);
    assert_eq!(reappeared.common.status_epoch, first.common.status_epoch);
    assert_ne!(reappeared.common.occupant_id, first.common.occupant_id);
}

#[test]
fn inactive_report_rejects_delayed_active_until_absence_proves_a_new_incarnation() {
    let harness = registry_harness(1_000);
    let (registry, published) = (&harness.registry, &harness.published);
    registry.report_integration(integration(Agent::Pi, 502, State::Working, 900, true));
    let first = published.last();
    assert!(registry.report_integration(integration(Agent::Pi, 502, State::Working, 901, false)));
    assert!(!published.last().active);
    assert_eq!(
        published.last().common.occupant_id,
        first.common.occupant_id
    );
    let count_after_inactive = published.len();

    for seq in [900, 901, 902] {
        assert!(!registry.report_integration(integration(Agent::Pi, 502, State::Idle, seq, true)));
    }
    assert!(!registry.report_screen(&session(SESSION_ID), screen(Agent::Pi, 502, State::Idle)));
    assert_eq!(published.len(), count_after_inactive);

    registry.clear_screen(&session(SESSION_ID));
    assert!(registry.report_screen(&session(SESSION_ID), screen(Agent::Pi, 502, State::Idle)));
    let reappeared = published.last();
    assert!(reappeared.active);
    assert_eq!(reappeared.common.source, Some(Source::Screen));
    assert_eq!(reappeared.common.completed_revision, 0);
    assert_eq!(reappeared.common.status_epoch, first.common.status_epoch);
    assert_ne!(reappeared.common.occupant_id, first.common.occupant_id);
    assert!(registry.report_integration(integration(Agent::Pi, 502, State::Idle, 0, true)));
    assert!(published.last().active);
    assert_eq!(published.last().common.source, Some(Source::Integration));
    assert_eq!(
        published.last().common.occupant_id,
        reappeared.common.occupant_id
    );
}

#[test]
fn snapshot_and_reconnect_resend_preserve_exact_identity_and_revision() {
    let harness = registry_harness(1_000);
    let (registry, published) = (&harness.registry, &harness.published);
    let waiting = IntegrationStatusReport {
        message: Some("waiting".to_owned()),
        ..integration(Agent::Pi, 601, State::Blocked, 1, true)
    };
    registry.report_integration(waiting);
    let original = published.last();
    assert_eq!(registry.snapshot(), vec![original.clone()]);

    registry.resend();
    assert_eq!(published.len(), 2);
    assert_eq!(published.last(), original);
}
