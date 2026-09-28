//! Process-exit completion contracts for the worker registry: a finished agent
//! that leaves keeps its completion on the wire until the session closes and
//! says so with `occupant_exited`, an agent with nothing to acknowledge is
//! retired, an explicit `active: false` withdrawal stays a withdrawal, and a
//! dead occupant backs no prompt proof. Ports v2
//! `apps/worker/tests/agents/agent-status-exit-completion.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_status_support;

use agent_status_support::{registry_harness, session};
use roost_protocol::wire::agent_status::AgentRuntimeState as State;
use roost_worker::agents::BuiltinAgentId as Agent;
use roost_worker::agents::registry::{IntegrationStatusReport, ScreenStatusReport};

const SESSION_ID: &str = "33333333-3333-4333-8333-333333333333";

fn screen(agent_id: Agent, process_id: u32, state: State) -> ScreenStatusReport {
    ScreenStatusReport {
        agent_id,
        process_id,
        state,
        visible_blocker: false,
    }
}

fn integration(process_id: u32, state: State, seq: u64, active: bool) -> IntegrationStatusReport {
    IntegrationStatusReport {
        session_id: session(SESSION_ID),
        agent_id: Agent::Omp,
        process_id,
        state,
        message: None,
        seq,
        active,
    }
}

#[test]
fn a_completed_agent_that_loses_its_process_keeps_the_completion_until_close() {
    let harness = registry_harness(5_000);
    let (registry, published, id) = (&harness.registry, &harness.published, session(SESSION_ID));
    registry.report_screen(&id, screen(Agent::Omp, 71, State::Working));
    let working = published.last();
    harness.clock.advance(5);
    registry.report_screen(&id, screen(Agent::Omp, 71, State::Idle));
    let completed = published.last();
    assert_eq!(
        completed.common.completed_revision,
        completed.common.revision
    );

    harness.clock.advance(5);
    registry.clear_screen(&id);
    let exited = published.last();
    assert!(exited.active);
    assert_eq!(exited.common.state, State::Idle);
    assert_eq!(exited.common.occupant_id, working.common.occupant_id);
    assert_eq!(
        exited.common.completed_revision,
        completed.common.completed_revision
    );
    assert!(exited.common.occupant_exited);
    assert!(!completed.common.occupant_exited);
    assert!(exited.common.revision > completed.common.revision);
    assert_eq!(registry.snapshot(), vec![exited.clone()]);

    harness.clock.advance(5);
    registry.close_session(&id);
    let closed = published.last();
    assert!(!closed.active);
    assert_eq!(closed.common.occupant_id, working.common.occupant_id);
    assert_eq!(
        closed.common.completed_revision,
        completed.common.completed_revision
    );
}

#[test]
fn an_exit_while_working_publishes_the_completion_the_viewer_never_saw() {
    let harness = registry_harness(5_000);
    let (registry, published) = (&harness.registry, &harness.published);
    registry.report_integration(IntegrationStatusReport {
        message: Some("Approval needed".to_owned()),
        ..integration(72, State::Blocked, 1, true)
    });
    let blocked = published.last();

    harness.clock.advance(101);
    registry.expire_leases();
    let exited = published.last();
    assert!(exited.active);
    assert_eq!(exited.common.state, State::Idle);
    assert_eq!(exited.common.occupant_id, blocked.common.occupant_id);
    assert_eq!(exited.common.source, blocked.common.source);
    assert!(exited.common.occupant_exited);
    assert_eq!(exited.common.completed_revision, exited.common.revision);
    assert_eq!(exited.common.message, None);
}

#[test]
fn an_idle_agent_with_nothing_to_acknowledge_is_retired_on_exit() {
    let harness = registry_harness(5_000);
    let (registry, published, id) = (&harness.registry, &harness.published, session(SESSION_ID));
    registry.report_screen(&id, screen(Agent::Pi, 73, State::Idle));
    let idle = published.last();
    assert_eq!(idle.common.completed_revision, 0);

    harness.clock.advance(5);
    registry.clear_screen(&id);
    let retired = published.last();
    assert!(!retired.active);
    assert_eq!(retired.common.occupant_id, idle.common.occupant_id);
    assert_eq!(retired.common.completed_revision, 0);
    assert!(registry.snapshot().is_empty());
}

#[test]
fn an_integrations_explicit_withdrawal_retires_the_row_instead_of_completing() {
    let harness = registry_harness(5_000);
    let (registry, published) = (&harness.registry, &harness.published);
    registry.report_integration(integration(74, State::Working, 1, true));
    registry.report_integration(integration(74, State::Working, 2, false));
    let withdrawn = published.last();
    assert!(!withdrawn.active);
    assert_eq!(withdrawn.common.state, State::Working);
    assert!(registry.snapshot().is_empty());
}

#[test]
fn a_retained_completion_backs_no_prompt_proof() {
    let harness = registry_harness(5_000);
    let (registry, id) = (&harness.registry, session(SESSION_ID));
    registry.report_screen(&id, screen(Agent::Omp, 75, State::Working));
    let proof = registry
        .current_private_proof(&id)
        .expect("a live occupant proves itself");
    assert_eq!(proof.state, State::Working);
    assert_eq!(proof.process.agent_id, Agent::Omp);
    assert_eq!(proof.process.pid, 75);

    registry.clear_screen(&id);
    assert_eq!(registry.current_private_proof(&id), None);
}
