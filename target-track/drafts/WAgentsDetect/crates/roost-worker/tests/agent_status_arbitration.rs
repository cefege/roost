//! Integration and screen arbitration in the worker registry: a live
//! integration wins and expires to the screen, heartbeats do not fan out, and a
//! visible blocker corrects an identity-only integration but never a
//! full-lifecycle one. Ports v2 `apps/worker/tests/agents/agent-status.test.ts`
//! ("integration and screen arbitration") and
//! `agent-status-visible-blocker.test.ts`. v2's "inherited prototype key" case
//! has no Rust counterpart: `BuiltinAgentId` is a closed enum.
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
    registry.report_integration(integration(Agent::Pi, 20, State::Working, 1));
    let revision = published.all()[0].common.revision;
    harness.clock.advance(1);
    registry.report_integration(integration(Agent::Pi, 20, State::Working, 2));
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
fn a_visible_blocker_corrects_an_identity_only_integration() {
    let harness = default_registry();
    report_pair(&harness.registry, Agent::Codex, State::Working, true);
    assert_eq!(harness.published.last().common.state, State::Blocked);
    assert_eq!(harness.published.last().common.source, Some(Source::Screen));
}

#[test]
fn a_full_lifecycle_integration_keeps_its_own_state() {
    for agent_id in [Agent::Omp, Agent::Pi] {
        let harness = default_registry();
        report_pair(&harness.registry, agent_id, State::Working, true);
        assert_eq!(harness.published.last().common.state, State::Working);
        assert_eq!(
            harness.published.last().common.source,
            Some(Source::Integration)
        );
    }
}

#[test]
fn no_visible_blocker_leaves_the_integration_state_alone() {
    let harness = default_registry();
    report_pair(&harness.registry, Agent::Codex, State::Working, false);
    assert_eq!(harness.published.last().common.state, State::Working);
    assert_eq!(
        harness.published.last().common.source,
        Some(Source::Integration)
    );
}

#[test]
fn a_screen_blocker_for_a_different_agent_does_not_override() {
    let harness = default_registry();
    let registry = &harness.registry;
    registry.report_screen(
        &session(SESSION_ID),
        screen(Agent::Gemini, 41, State::Idle, true),
    );
    registry.report_integration(integration(Agent::Codex, 40, State::Working, 1));
    assert_eq!(harness.published.last().common.state, State::Working);
}
