//! Whether the fleet is where the rollout says it must be. Called by the push
//! runtime through the decision boundary in `rollout.rs`; depends on the deploy
//! group's own keeper convergence proof and on the roster, and on nothing else
//! in the push group.
//!
//! These are the same questions a single-host deploy asks of one machine, asked
//! of a set: each participant must be at the requested commit, and its keeper
//! must have converged on the side of the rollout that action is about. What is
//! fleet-shaped is the two additions — a machine outside the journal is a
//! DEFERRED machine and never a failure, and a rollback additionally proves that
//! no machine it never journalled was dragged along.

use std::collections::BTreeSet;

use roost_protocol::keeper_update::PRESERVE;

use crate::deploy::convergence::{self, Direction};
use crate::push::admission::FleetRolloutWorker;
use crate::push::rollout::RolloutAction;
use crate::status::report::WorkerStatus;

/// Every way the fleet is not yet where the rollout says it must be.
///
/// A worker outside the journal is a deferred machine, never a participant: one
/// that registers or returns mid-rollout must not abort the rollout, so the
/// per-participant proofs below are the only set comparison. The extra check on
/// the rollback arm is the one that catches a rollout dragging a worker it never
/// journalled — a deferred machine legitimately reports either end of the
/// rollout, and only a third SHA is evidence of an unjournaled mutation.
pub fn fleet_convergence_problems(
    workers: &[WorkerStatus],
    targets: &[FleetRolloutWorker],
    expected_sha: &str,
    action: RolloutAction,
    routable: Option<&BTreeSet<String>>,
    rollout_target_sha: Option<&str>,
) -> Vec<String> {
    let mut problems = Vec::new();
    for target in targets {
        let Some(worker) = workers
            .iter()
            .find(|worker| worker.fingerprint == target.fingerprint)
        else {
            problems.push(format!(
                "{}: missing from the coordinator worker inventory",
                target.fingerprint
            ));
            continue;
        };
        if let Some(routable) = routable
            && !routable.contains(&target.fingerprint)
        {
            problems.push(format!("{}: worker is not coordinator-routable", worker.label));
            continue;
        }
        if let Some(problem) = worker_problem(worker, target, expected_sha, action) {
            problems.push(problem);
        }
    }
    if action == RolloutAction::Rollback {
        problems.extend(unjournalled_problems(
            workers,
            targets,
            expected_sha,
            rollout_target_sha,
        ));
    }
    problems
}

/// One machine's own proof: it is at the requested commit, and its keeper
/// converged on the side of the rollout this action is about. A preserve is
/// proven against the target contract and a replace against the source, because
/// those are the two facts that are opposites if the deploy did not land.
fn worker_problem(
    worker: &WorkerStatus,
    target: &FleetRolloutWorker,
    expected_sha: &str,
    action: RolloutAction,
) -> Option<String> {
    if worker.git_sha.as_deref() != Some(expected_sha) {
        return Some(format!(
            "{}: reports {}, expected {}",
            worker.label,
            worker.git_sha.as_deref().unwrap_or("no SHA"),
            expected_sha
        ));
    }
    let direction = if action == RolloutAction::Rollback {
        Direction::Source
    } else {
        Direction::Target
    };
    convergence::convergence_problem(worker, &target.keeper_update, direction, None, None)
}

/// The machines this rollout never touched, checked for having been touched
/// anyway. A machine that never reported a SHA is unproven, not mutated, and a
/// deferred machine keeps its own SHA, so only a third one is a finding.
fn unjournalled_problems(
    workers: &[WorkerStatus],
    targets: &[FleetRolloutWorker],
    expected_sha: &str,
    rollout_target_sha: Option<&str>,
) -> Vec<String> {
    let participants: BTreeSet<&str> = targets
        .iter()
        .map(|target| target.fingerprint.as_str())
        .collect();
    let mut allowed = vec![expected_sha.to_string()];
    if let Some(other) = rollout_target_sha {
        allowed.push(other.to_string());
    }
    workers
        .iter()
        .filter(|worker| !participants.contains(worker.fingerprint.as_str()))
        .filter_map(|worker| {
            let sha = worker.git_sha.as_deref()?;
            (!allowed.iter().any(|allowed| allowed == sha)).then(|| {
                format!(
                    "{}: unjournaled worker reports {}, outside this rollout",
                    worker.label,
                    sha.chars().take(8).collect::<String>()
                )
            })
        })
        .collect()
}

/// Whether every participant's recorded decision leaves its keeper alone, which
/// is the only state in which "already converged" is a true statement.
pub fn participants_need_no_keeper_work(targets: &[FleetRolloutWorker]) -> bool {
    targets
        .iter()
        .all(|target| target.keeper_update.admission.required_action == PRESERVE)
}
