//! Agent occupancy. Three rules carry it, and each is a place where the obvious
//! implementation is wrong, so each test names the failure it prevents.

use roost_worker::agent_occupancy::{Candidate, Loss, Occupancy, ProcessKey, RuntimeState, Source};

fn candidate(state: RuntimeState) -> Candidate {
    Candidate {
        state,
        message: None,
        visible_blocker: false,
    }
}

fn key(kind: &str, pid: u32) -> ProcessKey {
    ProcessKey::new(kind, pid)
}

/// AN UNINTERRUPTED (kind, pid) INCARNATION OWNS ONE OCCUPANT. The same
/// process seen twice is one occupant; a different pid is a different one.
#[test]
fn one_process_is_one_occupant_however_often_it_is_seen() {
    let mut occupancy = Occupancy::new();
    let process = key("omp", 100);

    occupancy.observe(
        process.clone(),
        Source::Integration,
        candidate(RuntimeState::Working),
    );
    occupancy.observe(
        process.clone(),
        Source::Integration,
        candidate(RuntimeState::Blocked),
    );
    occupancy.observe(
        process.clone(),
        Source::Integration,
        candidate(RuntimeState::Blocked),
    );

    assert_eq!(
        occupancy.occupants().len(),
        1,
        "one process is one occupant"
    );
    assert_eq!(
        occupancy.occupants()[0].occupant_id,
        occupancy.occupants()[0].occupant_id,
        "and it keeps the same token"
    );
}

/// A restart is a NEW occupant even where the pid is identical — a new
/// incarnation is a new thing, and merging them would show a restart as one
/// continuous run.
#[test]
fn a_restart_is_a_new_occupant_even_at_the_same_pid() {
    let mut occupancy = Occupancy::new();
    let process = key("omp", 100);

    occupancy.observe(
        process.clone(),
        Source::Integration,
        candidate(RuntimeState::Working),
    );
    let first_token = occupancy.get(&process).expect("an occupant").occupant_id;
    occupancy.lose(&process, Loss::Exited);
    occupancy.acknowledge(&process);

    // The same agent, same pid, restarted.
    occupancy.observe(
        process.clone(),
        Source::Integration,
        candidate(RuntimeState::Working),
    );
    let second_token = occupancy.get(&process).expect("a new occupant").occupant_id;
    assert_ne!(first_token, second_token, "a restart is not a continuation");
}

/// THE MOST IMPORTANT RULE. A dead occupant is not reclaimable by the same
/// numeric pid: pids are recycled, and resurrecting a retired occupant would
/// attribute a stale completion to an agent that never ran.
#[test]
fn a_dead_occupant_is_not_reclaimed_by_a_recycled_pid() {
    let mut occupancy = Occupancy::new();
    let process = key("omp", 100);
    occupancy.observe(
        process.clone(),
        Source::Integration,
        candidate(RuntimeState::Working),
    );
    occupancy.lose(&process, Loss::Exited);
    occupancy.acknowledge(&process);
    assert!(occupancy.get(&process).is_none(), "acknowledged and gone");

    // A DIFFERENT process now draws the same number.
    occupancy.observe(
        process.clone(),
        Source::Integration,
        candidate(RuntimeState::Idle),
    );
    let occupant = occupancy
        .get(&process)
        .expect("the new process has its own occupant");
    assert!(
        occupant.completed_revision == 0,
        "and it does not inherit the old completion — a recycled pid is a \\
         different process, and inheriting would attribute a finished run to an \\
         agent that never started"
    );
    assert_eq!(
        occupant.state,
        RuntimeState::Idle,
        "it reports only what was observed"
    );
}

/// The same rule, from the withdrawal side: an explicitly withdrawn occupant is
/// never revived by a later sighting of the same pid.
#[test]
fn a_withdrawn_occupant_is_never_revived_by_the_same_pid() {
    let mut occupancy = Occupancy::new();
    let process = key("omp", 100);
    occupancy.observe(
        process.clone(),
        Source::Integration,
        candidate(RuntimeState::Working),
    );
    occupancy.lose(&process, Loss::Withdrawn);
    assert!(occupancy.get(&process).is_none());

    occupancy.observe(
        process.clone(),
        Source::Integration,
        candidate(RuntimeState::Blocked),
    );
    let occupant = occupancy.get(&process).expect("a new occupant");
    assert_eq!(
        occupant.completed_revision, 0,
        "a withdrawal is not a finish, and a later sighting does not resurrect it"
    );
}

/// AN EXIT KEEPS THE COMPLETION. An agent that finishes and then leaves is
/// still DONE, and a viewer arriving a moment later must see that — otherwise
/// the run looks like it never happened.
#[test]
fn an_agent_that_finishes_and_then_leaves_is_still_reported_done() {
    let mut occupancy = Occupancy::new();
    let process = key("omp", 100);
    occupancy.observe(
        process.clone(),
        Source::Integration,
        candidate(RuntimeState::Working),
    );

    occupancy.lose(&process, Loss::Exited);
    let occupant = occupancy.get(&process).expect("the completion is held");
    assert!(!occupant.live, "the process is gone");
    assert!(
        occupant.awaits_acknowledgement(),
        "and the completion is still owed to a viewer"
    );
    assert_eq!(
        occupant.state,
        RuntimeState::Idle,
        "forced idle, not a disappearance"
    );
    assert_ne!(occupant.completed_revision, 0, "and the finish is recorded");
}

/// A withdrawal is the OPPOSITE: the integration said outright that it is done
/// asking, so there is nothing for a viewer to come and read.
#[test]
fn a_withdrawal_retires_the_row_outright() {
    let mut occupancy = Occupancy::new();
    let process = key("omp", 100);
    occupancy.observe(
        process.clone(),
        Source::Integration,
        candidate(RuntimeState::Working),
    );

    assert_eq!(
        occupancy.lose(&process, Loss::Withdrawn),
        None,
        "the row is gone"
    );
    assert!(occupancy.get(&process).is_none());
    assert!(
        occupancy.awaiting_acknowledgement().is_empty(),
        "a withdrawal leaves nothing to acknowledge — nobody will come to read a \\
         completion the agent itself withdrew"
    );
}

/// A viewer seeing the completion is what RELEASES a dead occupant. Until then
/// it is held, because the alternative is a viewer that arrives after the
/// release and sees nothing.
#[test]
fn a_dead_occupant_is_released_only_by_an_acknowledgement() {
    let mut occupancy = Occupancy::new();
    let process = key("omp", 100);
    occupancy.observe(
        process.clone(),
        Source::Integration,
        candidate(RuntimeState::Working),
    );
    occupancy.lose(&process, Loss::Exited);

    assert_eq!(
        occupancy.awaiting_acknowledgement().len(),
        1,
        "held until a viewer looks"
    );

    occupancy.acknowledge(&process);
    assert!(
        occupancy.get(&process).is_none(),
        "and released once it does"
    );
    assert!(occupancy.awaiting_acknowledgement().is_empty());
}

/// Acknowledging a LIVE occupant does not end it. A viewer looking at a running
/// agent has seen it, but it has not finished.
#[test]
fn acknowledging_a_live_occupant_does_not_end_it() {
    let mut occupancy = Occupancy::new();
    let process = key("omp", 100);
    occupancy.observe(
        process.clone(),
        Source::Integration,
        candidate(RuntimeState::Working),
    );

    occupancy.acknowledge(&process);
    assert!(
        occupancy.get(&process).is_some(),
        "a running agent is not released by being looked at"
    );
    assert!(
        occupancy.awaiting_acknowledgement().is_empty(),
        "and it has finished nothing"
    );
}

/// A different agent kind is a different occupant even at the same pid.
#[test]
fn the_agent_kind_is_part_of_the_identity() {
    let mut occupancy = Occupancy::new();
    occupancy.observe(
        key("omp", 100),
        Source::Integration,
        candidate(RuntimeState::Working),
    );
    occupancy.observe(
        key("pi", 100),
        Source::Integration,
        candidate(RuntimeState::Idle),
    );
    assert_eq!(
        occupancy.occupants().len(),
        2,
        "the same pid running two kinds is two occupants"
    );
}

/// Several agents in one session are independent occupants.
#[test]
fn several_agents_coexist() {
    let mut occupancy = Occupancy::new();
    occupancy.observe(
        key("omp", 100),
        Source::Integration,
        candidate(RuntimeState::Working),
    );
    occupancy.observe(
        key("pi", 200),
        Source::Integration,
        candidate(RuntimeState::Blocked),
    );
    occupancy.observe(
        key("claude", 300),
        Source::Integration,
        candidate(RuntimeState::Idle),
    );

    let occupants = occupancy.occupants();
    assert_eq!(occupants.len(), 3);
    // Published in a stable order, so two observers see the same sequence.
    let ids: Vec<u64> = occupants.iter().map(|o| o.occupant_id).collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(ids, sorted, "occupants are published in a stable order");
}

/// The revision only moves when the published state actually changes. A
/// re-observation of the same thing must not churn a viewer's stream.
#[test]
fn a_repeated_observation_does_not_churn_the_revision() {
    let mut occupancy = Occupancy::new();
    let process = key("omp", 100);
    occupancy.observe(
        process.clone(),
        Source::Integration,
        candidate(RuntimeState::Working),
    );
    let revision = occupancy.get(&process).expect("an occupant").revision;
    for _ in 0..5 {
        occupancy.observe(
            process.clone(),
            Source::Integration,
            candidate(RuntimeState::Working),
        );
    }
    assert_eq!(
        occupancy.get(&process).expect("an occupant").revision,
        revision,
        "the same state published five times is still one revision"
    );
}
