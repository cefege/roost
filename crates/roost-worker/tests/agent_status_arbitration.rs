//! OMP integration/screen arbitration: live lifecycle reports take precedence,
//! lease expiry falls back to screen evidence, heartbeats do not fan out,
//! and reconnect preserves the last published revision.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_status_support;

use agent_status_support::{SESSION_ID, default_registry, registry_harness, session};
use roost_protocol::wire::agent_status::{AgentRuntimeState as State, AgentStatusSource as Source};
use roost_worker::agents::BuiltinAgentId as Agent;
use roost_worker::agents::registry::{
    AgentStatusRegistry, IntegrationStatusReport, ScreenStatusReport,
};

fn integration(
    agent_id: Agent,
    process_id: u32,
    state: State,
    seq: u64,
) -> IntegrationStatusReport {
    IntegrationStatusReport {
        session_id: session(SESSION_ID),
        agent_id,
        process_id,
        state,
        message: None,
        seq,
        active: true,
    }
}

fn screen(
    agent_id: Agent,
    process_id: u32,
    state: State,
    visible_blocker: bool,
) -> ScreenStatusReport {
    ScreenStatusReport {
        agent_id,
        process_id,
        state,
        visible_blocker,
    }
}

#[test]
fn live_integration_wins_expires_to_screen_and_derives_completion_revisions() {
    let harness = registry_harness(1_000);
    let (registry, published, id) = (&harness.registry, &harness.published, session(SESSION_ID));
    registry.report_screen(&id, screen(Agent::Omp, 20, State::Working, false));
    assert!(registry.report_integration(integration(Agent::Omp, 20, State::Blocked, 1)));
    registry.report_screen(&id, screen(Agent::Omp, 20, State::Idle, false));
    assert_eq!(published.last().common.state, State::Blocked);
    assert!(!registry.report_integration(integration(Agent::Omp, 20, State::Working, 1)));
    harness.clock.advance(101);
    registry.expire_leases();
    let completed = published.last();
    assert_eq!(completed.common.state, State::Idle);
    assert_eq!(
        completed.common.completed_revision,
        completed.common.revision
    );
}

#[test]
fn heartbeats_do_not_fan_out_and_reconnect_resend_preserves_revision() {
    let harness = registry_harness(2_000);
    let (registry, published, id) = (&harness.registry, &harness.published, session(SESSION_ID));
    registry.report_integration(integration(Agent::Omp, 20, State::Working, 1));
    let revision = published.all()[0].common.revision;
    harness.clock.advance(1);
    registry.report_integration(integration(Agent::Omp, 20, State::Working, 2));
    assert_eq!(published.len(), 1);
    registry.resend();
    assert_eq!(published.len(), 2);
    assert_eq!(published.all()[1].common.revision, revision);
    registry.close_session(&id);
    assert!(!published.last().active);
}

/// v2 `reportPair`: an idle screen (with or without a blocker) then a live
/// integration for the same process.
fn report_pair(
    registry: &AgentStatusRegistry,
    agent_id: Agent,
    state: State,
    visible_blocker: bool,
) {
    registry.report_screen(
        &session(SESSION_ID),
        screen(agent_id, 40, State::Idle, visible_blocker),
    );
    registry.report_integration(integration(agent_id, 40, state, 1));
}

#[test]
fn omp_lifecycle_reports_outrank_visible_screen_blockers() {
    let harness = default_registry();
    report_pair(&harness.registry, Agent::Omp, State::Working, true);
    assert_eq!(harness.published.last().common.state, State::Working);
    assert_eq!(
        harness.published.last().common.source,
        Some(Source::Integration)
    );
}

#[test]
fn no_visible_blocker_leaves_the_integration_state_alone() {
    let harness = default_registry();
    report_pair(&harness.registry, Agent::Omp, State::Working, false);
    assert_eq!(harness.published.last().common.state, State::Working);
    assert_eq!(
        harness.published.last().common.source,
        Some(Source::Integration)
    );
}
