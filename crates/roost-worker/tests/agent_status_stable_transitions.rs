//! Acquisition grace and screen-state stabilization for one observed agent
//! process, exercising `StableScreenDetector` directly. Ports v2
//! `apps/worker/tests/agents/agent-status-stable-transitions.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_protocol::wire::agent_status::AgentRuntimeState as State;
use roost_protocol::wire::brand::SessionId;
use roost_worker::agents::BuiltinAgentId as Agent;
use roost_worker::agents::manifest_engine::ManifestDetection;
use roost_worker::agents::process_scan::AgentProcessIdentity;
use roost_worker::agents::registry::ScreenStatusReport;
use roost_worker::agents::stable_detection::StableScreenDetector;

fn session() -> SessionId {
    SessionId::try_from("11111111-1111-4111-8111-111111111111").unwrap()
}

fn detection(state: State, visible: bool) -> ManifestDetection {
    ManifestDetection {
        state: Some(state),
        visible_idle: visible && state == State::Idle,
        visible_blocker: visible && state == State::Blocked,
        visible_working: visible && state == State::Working,
        skip_state_update: false,
        matched_rule_id: visible.then_some("visible"),
    }
}

fn codex(pid: u32) -> AgentProcessIdentity {
    AgentProcessIdentity {
        agent_id: Agent::Codex,
        pid,
        foreground: None,
    }
}

fn report(state: State, visible_blocker: bool) -> Option<ScreenStatusReport> {
    Some(ScreenStatusReport {
        agent_id: Agent::Codex,
        process_id: 20,
        state,
        visible_blocker,
    })
}

/// Closes the acquisition grace window so a test can exercise the
/// working→idle stabilizer from a settled identity.
fn acquire(stable: &mut StableScreenDetector) {
    assert_eq!(
        stable.observe(&session(), &codex(20), &detection(State::Working, true), 0),
        None
    );
    assert_eq!(
        stable.observe(&session(), &codex(20), &detection(State::Working, true), 1),
        report(State::Working, false)
    );
}

#[test]
fn withholds_the_first_evaluation_of_a_newly_acquired_identity() {
    let mut stable = StableScreenDetector::new();
    assert_eq!(
        stable.observe(&session(), &codex(20), &detection(State::Blocked, true), 0),
        None
    );
    assert_eq!(stable.current(&session()).unwrap().state, State::Blocked);
    assert_eq!(
        stable.observe(
            &session(),
            &codex(20),
            &detection(State::Working, true),
            100
        ),
        None
    );
    assert_eq!(
        stable.observe(
            &session(),
            &codex(20),
            &detection(State::Working, true),
            200
        ),
        report(State::Working, false)
    );
}

#[test]
fn publishes_a_disagreeing_acquisition_once_the_grace_window_expires() {
    let mut stable = StableScreenDetector::new();
    assert_eq!(
        stable.observe(&session(), &codex(20), &detection(State::Blocked, true), 0),
        None
    );
    assert_eq!(
        stable.observe(
            &session(),
            &codex(20),
            &detection(State::Working, true),
            2_999
        ),
        None
    );
    assert_eq!(
        stable.observe(
            &session(),
            &codex(20),
            &detection(State::Blocked, true),
            3_000
        ),
        report(State::Blocked, true)
    );
}

#[test]
fn re_arms_the_grace_window_when_the_process_behind_an_agent_is_replaced() {
    let mut stable = StableScreenDetector::new();
    acquire(&mut stable);
    assert_eq!(
        stable.observe(&session(), &codex(21), &detection(State::Blocked, true), 2),
        None
    );
}

#[test]
fn holds_transient_working_to_plain_idle_spinner_loss() {
    let mut stable = StableScreenDetector::new();
    acquire(&mut stable);
    assert_eq!(
        stable.observe(&session(), &codex(20), &detection(State::Idle, false), 100),
        None
    );
    assert_eq!(
        stable.observe(&session(), &codex(20), &detection(State::Idle, false), 200),
        None
    );
    assert_eq!(
        stable.observe(
            &session(),
            &codex(20),
            &detection(State::Working, true),
            250
        ),
        None
    );
    assert_eq!(stable.current(&session()).unwrap().state, State::Working);
}

#[test]
fn confirms_sustained_plain_idle_but_accepts_visible_idle_immediately() {
    let mut stable = StableScreenDetector::new();
    acquire(&mut stable);
    assert_eq!(
        stable.observe(&session(), &codex(20), &detection(State::Idle, false), 100),
        None
    );
    assert_eq!(
        stable.observe(&session(), &codex(20), &detection(State::Idle, false), 200),
        None
    );
    assert_eq!(
        stable.observe(&session(), &codex(20), &detection(State::Idle, false), 300),
        None
    );
    assert_eq!(
        stable.observe(&session(), &codex(20), &detection(State::Idle, false), 400),
        report(State::Idle, false)
    );

    stable.observe(
        &session(),
        &codex(20),
        &detection(State::Working, true),
        500,
    );
    assert_eq!(
        stable.observe(&session(), &codex(20), &detection(State::Idle, true), 501),
        report(State::Idle, false)
    );
}

#[test]
fn holds_the_previous_state_on_skip_state_screens() {
    let mut stable = StableScreenDetector::new();
    acquire(&mut stable);
    let skipped = ManifestDetection {
        state: None,
        skip_state_update: true,
        ..detection(State::Idle, false)
    };
    assert_eq!(stable.observe(&session(), &codex(20), &skipped, 100), None);
    assert_eq!(stable.current(&session()).unwrap().state, State::Working);
}
