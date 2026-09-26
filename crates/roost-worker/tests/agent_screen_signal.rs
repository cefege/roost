//! What each observation SOURCE is allowed to prove. Split from the identity
//! rules because the two are different questions: one is "is this the same
//! agent", the other is "may this reporter correct the state".
//!
//! A screen observation proves identity and activity. A visible blocker PROMPT
//! additionally corrects an integration that is not a full-lifecycle authority.
//! Nothing corrects a full-lifecycle integration.

use roost_worker::agent_occupancy::{Candidate, Loss, Occupancy, ProcessKey, RuntimeState, Source};

fn candidate(state: RuntimeState) -> Candidate {
    Candidate {
        state,
        message: None,
        visible_blocker: false,
    }
}

fn blocker() -> Candidate {
    Candidate {
        state: RuntimeState::Idle,
        message: None,
        visible_blocker: true,
    }
}

fn key(kind: &str, pid: u32) -> ProcessKey {
    ProcessKey::new(kind, pid)
}

/// A VISIBLE BLOCKER PROMPT OUTRANKS AN INTEGRATION that is not a
/// full-lifecycle authority: a prompt on screen is direct evidence a human is
/// being waited on, and an integration reporting `working` through that is
/// reporting on a screen the user is already answering.
#[test]
fn a_visible_blocker_prompt_corrects_a_non_lifecycle_integration() {
    let mut occupancy = Occupancy::new();
    let process = key("other-agent", 100);
    occupancy.observe(
        process.clone(),
        Source::Integration,
        candidate(RuntimeState::Working),
    );
    assert_eq!(
        occupancy.get(&process).expect("an occupant").state,
        RuntimeState::Working
    );

    occupancy.observe(process.clone(), Source::Screen, blocker());
    assert_eq!(
        occupancy.get(&process).expect("an occupant").state,
        RuntimeState::Blocked,
        "an on-screen prompt is direct evidence and outranks the integration"
    );
}

/// A screen observation that is NOT a blocker does not override the
/// integration. A terminal that merely has text on it proves nothing about what
/// the agent is doing.
#[test]
fn an_ordinary_screen_observation_does_not_override_the_integration() {
    let mut occupancy = Occupancy::new();
    let process = key("other-agent", 100);
    occupancy.observe(
        process.clone(),
        Source::Integration,
        candidate(RuntimeState::Blocked),
    );
    occupancy.observe(
        process.clone(),
        Source::Screen,
        candidate(RuntimeState::Working),
    );
    assert_eq!(
        occupancy.get(&process).expect("an occupant").state,
        RuntimeState::Blocked,
        "text on a screen is not evidence that the agent is working"
    );
}

/// A dead occupant cannot back a prompt proof. A prompt on screen for a process
/// that has already finished is the previous run's prompt, and reporting it as
/// a live block would strand a session on a question nobody is there to answer.
#[test]
fn a_dead_occupant_cannot_be_revived_by_a_screen_signal() {
    let mut occupancy = Occupancy::new();
    let process = key("omp", 100);
    occupancy.observe(
        process.clone(),
        Source::Integration,
        candidate(RuntimeState::Working),
    );
    occupancy.lose(&process, Loss::Exited);
    let completion = occupancy.get(&process).expect("held").completed_revision;

    // A leftover prompt for the same pid.
    occupancy.observe(process.clone(), Source::Screen, blocker());
    let occupant = occupancy.get(&process).expect("still held");
    assert_eq!(
        occupant.completed_revision, completion,
        "a dead occupant is not brought back by a screen signal"
    );
    assert_eq!(
        occupant.state,
        RuntimeState::Idle,
        "and it does not become blocked again"
    );
}
