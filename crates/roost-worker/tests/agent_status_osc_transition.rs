//! The OSC-evidence clear on agent transition: retained OSC title and progress
//! are the top-priority input of several manifests, so a replacement process
//! in the same PTY must not be judged by the previous agent's final title.
//! Drives the detector against a scripted scanner, the real pinned manifests
//! and a recording registry. Ports v2
//! `apps/worker/tests/agents/agent-status-osc-transition.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_status_support;

use agent_status_support::{DetectorHarness, SESSION_ID, detector_harness};
use roost_protocol::wire::agent_status::AgentRuntimeState as State;
use roost_worker::agents::BuiltinAgentId as Agent;

const BLOCKED_TITLE: &str = "Action Required — review the patch";

fn harness(agent_id: Agent, osc_title: &str) -> DetectorHarness {
    let harness = detector_harness(None);
    harness.scanner.set(SESSION_ID, agent_id, 4_321);
    harness.sessions.add(osc_title);
    harness
}

/// Past the screen-rescan gate and the acquisition grace window, so a settled
/// identity has actually published.
async fn settle(harness: &DetectorHarness) {
    for _ in 0..3 {
        harness.clock.advance(4_000);
        harness.detector.scan_now().await;
        harness.detector.scan_now().await;
    }
}

#[tokio::test]
async fn a_replacement_agent_is_not_judged_by_the_previous_agents_title() {
    let harness = harness(Agent::Codex, BLOCKED_TITLE);
    settle(&harness).await;
    assert_eq!(harness.published.last().common.state, State::Blocked);

    harness.scanner.set(SESSION_ID, Agent::Omp, 5_555);
    settle(&harness).await;
    assert_eq!(harness.sessions.osc_title(), "");
    assert_eq!(harness.published.last().common.agent_id.as_str(), "omp");
    assert_eq!(harness.published.last().common.state, State::Idle);
    harness.detector.dispose();
}

#[tokio::test]
async fn a_stale_idle_title_cannot_hold_a_working_replacement_at_idle() {
    let harness = harness(Agent::Grok, "session - grok");
    settle(&harness).await;
    assert_eq!(harness.published.last().common.state, State::Idle);

    harness.scanner.set(SESSION_ID, Agent::Omp, 5_555);
    settle(&harness).await;
    assert_eq!(harness.sessions.osc_title(), "");
    harness.sessions.set_osc_title("π ⠋ building");
    settle(&harness).await;
    assert_eq!(harness.published.last().common.state, State::Working);
    harness.detector.dispose();
}

#[tokio::test]
async fn a_same_agent_pid_replacement_also_drops_the_retained_title() {
    let harness = harness(Agent::Codex, BLOCKED_TITLE);
    settle(&harness).await;
    harness.scanner.set(SESSION_ID, Agent::Codex, 9_999);
    settle(&harness).await;
    assert_eq!(harness.sessions.osc_title(), "");
    harness.detector.dispose();
}

#[tokio::test]
async fn first_acquisition_keeps_evidence_the_new_process_already_emitted() {
    let harness = harness(Agent::Codex, BLOCKED_TITLE);
    settle(&harness).await;
    assert_eq!(harness.sessions.osc_title(), BLOCKED_TITLE);
    assert_eq!(harness.published.last().common.state, State::Blocked);
    harness.detector.dispose();
}

#[tokio::test]
async fn a_stable_identity_does_not_clear_evidence_on_every_pass() {
    let harness = harness(Agent::Codex, BLOCKED_TITLE);
    settle(&harness).await;
    settle(&harness).await;
    settle(&harness).await;
    assert_eq!(harness.sessions.osc_title(), BLOCKED_TITLE);
    harness.detector.dispose();
}
