//! The single decision boundary of a fleet rollout: converge every participant,
//! take the durable global commit decision, or roll the whole fleet back. Called
//! by the push command; depends on the journal's phase vocabulary and on this
//! module's own convergence proofs, and on nothing else in the push group.
//!
//! The proofs themselves are in `convergence.rs`. They are a sibling rather than
//! part of this file because they are a different question — "is the fleet
//! where it must be" — and this file is "what does a failure here mean".
//!
//! Everything the boundary needs from the world arrives through
//! [`FleetRuntime`], so the one question this file answers — is a failure before
//! or after the durable global commit decision? — is a property of the code
//! rather than of whichever machines happen to be up. That is why it is a trait
//! and not a struct of ssh calls: the branch between "roll the whole fleet back"
//! and "there is no way back from here" is the safety property of `roost push`,
//! and a test has to be able to fail each side of it on demand.
//!
//! The order is: converge every participant and PROVE it, then take the durable
//! decision, then finish. Before the decision a failure rolls the whole fleet
//! back and re-proves it at the prior commit. After it there is nothing to roll
//! back to, and the command exits 8.

pub mod convergence;

use std::collections::BTreeSet;

use crate::command_error::CommandFailure;
use crate::deploy::codes;
use crate::push::admission::FleetRolloutWorker;
use crate::push::plan::FleetRolloutTarget;
use crate::services::deploy_journal::DeployPhase;

/// What the rollout is asking of one machine at one moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RolloutAction {
    /// Move the machine onto the target commit. Everything a rollback could undo
    /// is still undoable.
    Hold,
    /// Move the machine back onto the commit the fleet is leaving.
    Rollback,
    /// The fleet is committed; finish this machine.
    Finalize,
}

impl RolloutAction {
    /// The word the journal, the logs and an operator's readout use.
    pub const fn as_str(self) -> &'static str {
        match self {
            RolloutAction::Hold => "hold",
            RolloutAction::Rollback => "rollback",
            RolloutAction::Finalize => "finalize",
        }
    }
}

/// One fleet transaction: the machines it holds, and both ends of the rollout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetRolloutPlan {
    pub rollout_id: String,
    pub admission_recorded_at_ms: i64,
    pub prior_sha: String,
    pub target_sha: String,
    pub workers: Vec<FleetRolloutWorker>,
}

/// Everything the decision boundary needs from the world.
/// **WHY `Sync` IS A SUPERTRAIT AND NOT A `where` ON EACH METHOD.** Six
/// `async fn` in a public trait cannot carry an auto-trait bound, so the
/// futures these methods return have no nameable `Send`, and the lint saying
/// so is not asking for a suppression — it is saying the bound is real and
/// unexpressed. Every method holds `&self` across an await, so `Send` requires
/// `&Self: Send`, which requires `Self: Sync`.
///
/// It is a supertrait rather than six `where Self: Sync` clauses because the
/// bound is the TRAIT's: a caller holding a `&dyn FleetRuntime` has to know it
/// before it calls anything, and six scattered clauses are six chances to
/// forget one.
///
/// THE VERIFICATION IS AN ASSERTION, NOT A COMMENT. The production runtime was
/// NOT `Sync` before this: its single non-`Sync` field was `&'a dyn EnvSource`,
/// and `dyn EnvSource` was not `Sync` because the trait did not say so. One
/// supertrait on `roost_host::EnvSource` fixed the whole chain, since
/// `&T: Sync` whenever `T: Sync`. `runtime.rs` and the test fake both carry a
/// `const _: () = { assert_sync::<…>() }`, so a field that breaks the claim
/// later is a compile failure rather than a comment that decays.
pub trait FleetRuntime: Sync {
    /// Move one machine, and report what went wrong if it did not.
    fn settle_worker(
        &self,
        worker: &FleetRolloutWorker,
        action: RolloutAction,
    ) -> impl Future<Output = Result<(), String>> + Send;

    /// Every way the fleet is not yet where this action says it must be.
    fn prove_fleet(
        &self,
        expected_sha: &str,
        action: RolloutAction,
    ) -> impl Future<Output = Vec<String>> + Send;

    /// Take the durable global commit decision. After it returns, there is no
    /// rollback.
    fn begin_finalization(&self) -> impl Future<Output = Result<(), String>> + Send;

    /// Whether the coordinator can still be restored. False once it has settled.
    fn coordinator_can_roll_back(&self) -> impl Future<Output = Result<bool, String>> + Send;

    /// The coordinator is committed: retire what it replaced and restart it.
    fn finalize_coordinator(&self) -> impl Future<Output = Result<(), String>> + Send;

    /// Put the coordinator back on the prior commit.
    fn rollback_coordinator(&self) -> impl Future<Output = Result<(), String>> + Send;
}

/// What an interrupted run of this rollout must do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptedFleetRecovery {
    /// Resume converging this rollout's participants.
    ConvergeTarget,
    /// This rollout is not what is being asked for, or it had already chosen to
    /// go back; put the whole fleet back.
    RollbackFleet,
}

/// Which recovery an interrupted transaction gets.
///
/// There is no "restore the coordinator" arm because the coordinator's own
/// definition swap has its own machine journal, which
/// `resolve_interrupted_deploy` has already put back before this is consulted.
/// And there is no "finish it" arm because the finalizing checkpoint REMOVES
/// the journal: a run that finds no journal found a committed fleet, and there is
/// nothing left to finish.
pub fn interrupted_fleet_recovery(
    phase: DeployPhase,
    requested_target_matches: bool,
) -> InterruptedFleetRecovery {
    match phase {
        DeployPhase::RollingBack => InterruptedFleetRecovery::RollbackFleet,
        DeployPhase::Swapping if requested_target_matches => {
            InterruptedFleetRecovery::ConvergeTarget
        }
        DeployPhase::Swapping => InterruptedFleetRecovery::RollbackFleet,
    }
}

/// Does an interrupted journal describe the rollout this push is asking for?
///
/// The journal holds that rollout's PARTICIPANTS, a subset of today's
/// candidates, so a machine deferred then — or one that has since returned —
/// must not turn a resume of the same commit into a fleet-wide rollback.
pub fn same_rollout_target(
    plan: &FleetRolloutPlan,
    target_sha: &str,
    candidates: &[FleetRolloutTarget],
) -> bool {
    if plan.target_sha != target_sha || plan.workers.is_empty() {
        return false;
    }
    let requested: BTreeSet<&str> = candidates
        .iter()
        .map(|candidate| candidate.fingerprint.as_str())
        .collect();
    plan.workers
        .iter()
        .all(|worker| requested.contains(worker.fingerprint.as_str()))
}

/// Converge every held machine, take the durable decision, then finish them all.
pub async fn converge_atomic_fleet<R: FleetRuntime>(
    plan: &FleetRolloutPlan,
    runtime: &R,
) -> Result<(), CommandFailure> {
    if let Err(forward) =
        settle_and_prove(plan, RolloutAction::Hold, &plan.target_sha, runtime).await
    {
        return recover_from(plan, runtime, forward, false, true).await;
    }
    // Between the proof and the decision the fleet is converged but not
    // committed, so every failure here is still a rollback. `decision_pending`
    // says so: the decision has not been taken, and it may not be un-taken.
    if let Err(cause) = runtime.begin_finalization().await {
        // The decision is the boundary, and failing to record it is a
        // SETTLEMENT failure rather than a generic one: the fleet is
        // converged on disk with no record of which commit that was, and a
        // wrapper that reads exit 1 as "failed, try again" would retry a
        // transaction the same way. Exit 8 is the code for a transaction that
        // reached its irreversible point and could not be settled.
        let undecided = codes::refuse(
            codes::SETTLEMENT_FAILED,
            format!("the durable commit decision could not be recorded: {cause}"),
        );
        return recover_from(plan, runtime, undecided, false, true).await;
    }
    match finish_atomic_fleet_finalization(plan, runtime).await {
        Ok(()) => Ok(()),
        // Past the decision there is no rollback. A failure here is the one
        // situation the exit code 8 exists for.
        Err(settlement) => Err(settlement),
    }
}

/// Roll every machine back before restoring the coordinator, then prove the
/// fleet at the prior commit again after the restore.
pub async fn rollback_atomic_fleet<R: FleetRuntime>(
    plan: &FleetRolloutPlan,
    runtime: &R,
) -> Result<(), CommandFailure> {
    let problems = settle_every_worker(plan, RolloutAction::Rollback, runtime).await;
    if !problems.is_empty() {
        return Err(settlement_failure(RolloutAction::Rollback, &problems));
    }
    if let Err(cause) = runtime.rollback_coordinator().await {
        return Err(settlement_failure(
            RolloutAction::Rollback,
            &[format!("the coordinator could not be restored: {cause}")],
        ));
    }
    let restored = runtime
        .prove_fleet(&plan.prior_sha, RolloutAction::Rollback)
        .await;
    if restored.is_empty() {
        return Ok(());
    }
    Err(settlement_failure(RolloutAction::Rollback, &restored))
}

/// Finish the durable global commit decision. This path can never switch to a
/// rollback, which is the entire reason it is a separate function.
pub async fn finish_atomic_fleet_finalization<R: FleetRuntime>(
    plan: &FleetRolloutPlan,
    runtime: &R,
) -> Result<(), CommandFailure> {
    if let Err(cause) = runtime.finalize_coordinator().await {
        return Err(settlement_failure(
            RolloutAction::Finalize,
            &[format!("the coordinator could not be settled: {cause}")],
        ));
    }
    settle_and_prove(plan, RolloutAction::Finalize, &plan.target_sha, runtime).await
}

/// One failure on the way to a committed fleet, resolved against the boundary.
async fn recover_from<R: FleetRuntime>(
    plan: &FleetRolloutPlan,
    runtime: &R,
    forward: CommandFailure,
    finalizing: bool,
    decision_pending: bool,
) -> Result<(), CommandFailure> {
    if decision_pending {
        match runtime.coordinator_can_roll_back().await {
            Ok(true) => {}
            // Either the coordinator cannot be restored, or nobody can say. In
            // both cases the forward failure is the honest answer: inventing a
            // rollback the machine cannot perform would be a worse lie.
            Ok(false) | Err(_) => return Err(forward),
        }
    }
    if finalizing {
        return Err(forward);
    }
    if let Err(rollback) = rollback_atomic_fleet(plan, runtime).await {
        return Err(codes::refuse(
            codes::SETTLEMENT_FAILED,
            format!(
                "fleet rollout failed and the full rollback is incomplete:\n{}\n{}",
                forward.message, rollback.message
            ),
        ));
    }
    Err(codes::refuse(
        codes::SETTLEMENT_FAILED,
        format!(
            "fleet rollout failed; prior worker and keeper convergence was re-proven at {}:\n{}",
            plan.prior_sha, forward.message
        ),
    ))
}

async fn settle_and_prove<R: FleetRuntime>(
    plan: &FleetRolloutPlan,
    action: RolloutAction,
    expected_sha: &str,
    runtime: &R,
) -> Result<(), CommandFailure> {
    let mut problems = settle_every_worker(plan, action, runtime).await;
    problems.extend(runtime.prove_fleet(expected_sha, action).await);
    if problems.is_empty() {
        return Ok(());
    }
    Err(settlement_failure(action, &problems))
}

/// Every machine's outcome, exhaustively, rather than the first failure.
///
/// A fleet rollback that stopped at the first machine it could not reach would
/// leave the rest of the fleet on the target commit and call the rollback
/// complete; the operator's only way to learn which machines are where is a
/// summary that names all of them.
async fn settle_every_worker<R: FleetRuntime>(
    plan: &FleetRolloutPlan,
    action: RolloutAction,
    runtime: &R,
) -> Vec<String> {
    let mut problems = Vec::new();
    for worker in &plan.workers {
        if let Err(cause) = runtime.settle_worker(worker, action).await {
            problems.push(format!("{} ({}): {cause}", worker.fingerprint, worker.host));
        }
    }
    problems
}

fn settlement_failure(action: RolloutAction, problems: &[String]) -> CommandFailure {
    codes::refuse(
        codes::SETTLEMENT_FAILED,
        format!(
            "fleet {} did not settle every worker:\n{}",
            action.as_str(),
            problems.join("\n")
        ),
    )
}
