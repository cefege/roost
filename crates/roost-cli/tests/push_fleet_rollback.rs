//! The single decision boundary: what a fleet failure means before the durable
//! commit, and what it means after it.
//!
//! The runtime is a fake on purpose. The one property this file protects is a
//! BRANCH — "roll the whole fleet back" against "there is no way back from here"
//! — and a branch taken from ssh calls can only be reached by breaking real
//! machines. The fake records what the boundary asked the world to do, so the
//! assertions are about the observable sequence an operator would see: which
//! machines were moved back, in what order relative to the commit decision, and
//! what the process exits with.

// A test asserts with `expect`: the message IS the hypothesis about the setup,
// and the workspace lint table denies `expect_used` in every target. The
// production half of this slice contains no `expect` at all.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod push_fixture;

use std::collections::BTreeSet;

use push_fixture::fake_world::{Answers, Asked, FakeWorld};
use push_fixture::{contract, journaled_update, observation, participant};

use roost_cli::command_error::CommandFailure;
use roost_cli::deploy::codes;
use roost_cli::push::journal::FleetJournal;
use roost_cli::push::plan::FleetRolloutTarget;
use roost_cli::push::rollout::{
    FleetRolloutPlan, InterruptedFleetRecovery, RolloutAction, converge_atomic_fleet,
    interrupted_fleet_recovery, same_rollout_target,
};
use roost_cli::services::deploy_journal::{DEPLOY_JOURNAL_SCHEMA, DeployPhase};

const PRIOR: &str = "1111111111111111111111111111111111111111";
const TARGET: &str = "2222222222222222222222222222222222222222";

fn plan_with(hosts: &[&str]) -> FleetRolloutPlan {
    let update = journaled_update(
        &observation("keeper-a", 0),
        &contract("keeper-a"),
        &BTreeSet::new(),
    );
    FleetRolloutPlan {
        rollout_id: "rollout-1".to_string(),
        admission_recorded_at_ms: 1_700_000_000_000,
        prior_sha: PRIOR.to_string(),
        target_sha: TARGET.to_string(),
        workers: hosts
            .iter()
            .map(|host| participant(host, host, update.clone()))
            .collect(),
    }
}

fn world(answers: Answers) -> FakeWorld {
    FakeWorld::new(answers)
}

fn settled(asked: &[Asked], action: RolloutAction) -> Vec<String> {
    asked
        .iter()
        .filter_map(|entry| match entry {
            Asked::Settle(host, moved) if *moved == action => Some(host.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_fleet_that_converges_commits_the_decision_before_it_touches_a_machine_again() {
    let plan = plan_with(&["studio.example.test", "loft.example.test"]);
    let world = world(Answers::default());

    let outcome = converge_atomic_fleet(&plan, &world).await;
    assert!(outcome.is_ok(), "a clean fleet settles: {outcome:?}");

    let asked = world.asked();
    let commit_at = asked.iter().position(|entry| *entry == Asked::Commit);
    let commit_at = commit_at.expect("the durable decision is taken exactly once");
    let finalized_at = asked
        .iter()
        .position(|entry| *entry == Asked::FinalizeCoordinator);
    let finalized_at = finalized_at.expect("the coordinator is settled");
    assert!(
        commit_at < finalized_at,
        "the decision is durable BEFORE the irreversible step: {asked:?}"
    );
    assert_eq!(
        settled(&asked, RolloutAction::Hold),
        vec!["studio.example.test", "loft.example.test"],
        "every participant is moved before the decision"
    );
    assert_eq!(
        settled(&asked, RolloutAction::Finalize),
        vec!["studio.example.test", "loft.example.test"],
        "every participant is finished after it"
    );
    assert!(
        settled(&asked, RolloutAction::Rollback).is_empty(),
        "a fleet that converged never moves a machine back"
    );
    assert!(
        !world.journal_present(),
        "the journal is gone once the fleet is committed, so nothing can roll it back"
    );
}

#[tokio::test]
async fn a_failure_before_the_decision_moves_every_machine_back_and_keeps_the_rollback_point() {
    let plan = plan_with(&[
        "studio.example.test",
        "loft.example.test",
        "shed.example.test",
    ]);
    let world = world(Answers {
        fail_settle: Some(("loft.example.test".to_string(), RolloutAction::Hold)),
        ..Answers::default()
    });

    let failure: CommandFailure = converge_atomic_fleet(&plan, &world)
        .await
        .expect_err("a participant that will not move stops the fleet");
    assert_eq!(
        failure.code,
        codes::SETTLEMENT_FAILED,
        "a fleet that could not converge is exit 8, never a generic 1: {}",
        failure.message
    );
    assert!(
        failure.message.contains("loft.example.test")
            && failure.message.contains(&format!("re-proven at {PRIOR}")),
        "the operator is told which machine failed and that the fleet is back: {}",
        failure.message
    );

    let asked = world.asked();
    assert_eq!(
        settled(&asked, RolloutAction::Rollback),
        vec![
            "studio.example.test",
            "loft.example.test",
            "shed.example.test"
        ],
        "a rollback is EXHAUSTIVE: the machine that failed and the two that had already moved \
         all go back, because a partial rollback leaves a fleet on two commits: {asked:?}"
    );
    let rollback_coordinator_at = asked
        .iter()
        .position(|entry| *entry == Asked::RollbackCoordinator);
    let last_rollback = asked
        .iter()
        .rposition(|entry| matches!(entry, Asked::Settle(_, RolloutAction::Rollback)));
    assert!(
        rollback_coordinator_at > last_rollback,
        "the coordinator is restored only after every machine is back: {asked:?}"
    );
    assert!(
        asked
            .iter()
            .any(|entry| *entry == Asked::Prove(PRIOR.to_string(), RolloutAction::Rollback)),
        "the rollback is proved at the prior commit, not assumed: {asked:?}"
    );
    assert!(
        !asked.contains(&Asked::Commit),
        "the durable decision is never taken on the failure path: {asked:?}"
    );
    assert!(
        !world.journal_present(),
        "the rollback proved the prior commit, so the journal is settled"
    );
}

#[tokio::test]
async fn a_failure_after_the_decision_exits_eight_and_never_attempts_a_rollback() {
    let plan = plan_with(&["studio.example.test", "loft.example.test"]);
    let world = world(Answers {
        fail_finalize_coordinator: true,
        ..Answers::default()
    });

    let failure = converge_atomic_fleet(&plan, &world)
        .await
        .expect_err("a coordinator that will not settle is a settlement failure");
    assert_eq!(failure.code, codes::SETTLEMENT_FAILED);
    assert!(
        failure.message.contains("did not restart"),
        "the operator is told what did not happen: {}",
        failure.message
    );

    let asked = world.asked();
    assert!(asked.contains(&Asked::Commit), "{asked:?}");
    assert!(
        settled(&asked, RolloutAction::Rollback).is_empty()
            && !asked.contains(&Asked::RollbackCoordinator),
        "past the durable decision there is nothing to go back to, and pretending otherwise \
         would move machines onto a commit the coordinator no longer runs: {asked:?}"
    );
}

#[tokio::test]
async fn a_decision_that_could_not_be_recorded_is_not_rolled_back_when_the_coordinator_is_gone() {
    let plan = plan_with(&["studio.example.test"]);
    let world = world(Answers {
        fail_commit: true,
        coordinator_can_roll_back: Some(false),
        ..Answers::default()
    });

    let failure = converge_atomic_fleet(&plan, &world)
        .await
        .expect_err("a transaction that cannot record its decision has not settled");
    assert_eq!(failure.code, codes::SETTLEMENT_FAILED);
    assert!(
        failure.message.contains("could not be recorded"),
        "the operator is told the decision itself failed, not that a machine did: {}",
        failure.message
    );

    let asked = world.asked();
    assert!(asked.contains(&Asked::CanRollBack), "{asked:?}");
    assert!(
        settled(&asked, RolloutAction::Rollback).is_empty()
            && !asked.contains(&Asked::RollbackCoordinator),
        "a coordinator that cannot be restored is not a reason to move every machine back: \
         {asked:?}"
    );
    assert!(
        world.journal_present(),
        "the transaction record survives an unsettled run, so the next push can recover it"
    );
}

#[tokio::test]
async fn a_decision_that_could_not_be_recorded_is_rolled_back_while_the_coordinator_can_still_go_back()
 {
    let plan = plan_with(&["studio.example.test", "loft.example.test"]);
    let world = world(Answers {
        fail_commit: true,
        ..Answers::default()
    });

    let failure = converge_atomic_fleet(&plan, &world)
        .await
        .expect_err("a transaction that cannot record its decision has not settled");
    assert_eq!(failure.code, codes::SETTLEMENT_FAILED);

    let asked = world.asked();
    assert!(asked.contains(&Asked::CanRollBack), "{asked:?}");
    assert_eq!(
        settled(&asked, RolloutAction::Rollback),
        vec!["studio.example.test", "loft.example.test"],
        "the fleet converges in one action and goes back in one, with no machine left on the \
         target commit: {asked:?}"
    );
    assert!(
        !world.journal_present(),
        "a proven rollback settles the transaction"
    );
}

#[test]
fn an_interrupted_transaction_is_resumed_only_when_it_is_the_same_rollout() {
    let plan = plan_with(&["studio.example.test", "loft.example.test"]);
    let candidates: Vec<FleetRolloutTarget> = plan
        .workers
        .iter()
        .map(|worker| FleetRolloutTarget {
            fingerprint: worker.fingerprint.clone(),
            host: worker.host.clone(),
        })
        .collect();
    let with_a_newcomer: Vec<FleetRolloutTarget> = {
        let mut all = candidates.clone();
        all.push(FleetRolloutTarget {
            fingerprint: push_fixture::digest("fingerprint-shed"),
            host: "shed.example.test".to_string(),
        });
        all
    };

    assert!(
        same_rollout_target(&plan, TARGET, &candidates),
        "the same commit over a superset of candidates is a resume"
    );
    assert!(
        same_rollout_target(&plan, TARGET, &with_a_newcomer),
        "a machine that has since returned must not turn a resume into a rollback"
    );
    assert!(
        !same_rollout_target(&plan, PRIOR, &candidates),
        "a different commit is a different rollout"
    );

    assert_eq!(
        interrupted_fleet_recovery(DeployPhase::Swapping, true),
        InterruptedFleetRecovery::ConvergeTarget
    );
    assert_eq!(
        interrupted_fleet_recovery(DeployPhase::Swapping, false),
        InterruptedFleetRecovery::RollbackFleet
    );
    assert_eq!(
        interrupted_fleet_recovery(DeployPhase::RollingBack, true),
        InterruptedFleetRecovery::RollbackFleet,
        "a transaction that had already chosen to go back must be finished going back"
    );
}

#[test]
fn the_journal_records_the_participants_and_loses_the_rollback_point_only_on_settlement() {
    let plan = plan_with(&["studio.example.test", "loft.example.test"]);
    let journal = FleetJournal::open(&plan, plan.admission_recorded_at_ms);
    assert_eq!(journal.schema, DEPLOY_JOURNAL_SCHEMA);
    assert_eq!(journal.phase, DeployPhase::Swapping);
    assert!(
        journal.can_roll_back(),
        "an in-flight transaction can be restored"
    );
    assert_eq!(journal.participants.len(), 2);

    let resumed = journal.plan();
    assert_eq!(resumed.prior_sha, PRIOR);
    assert_eq!(resumed.target_sha, TARGET);
    assert_eq!(
        resumed
            .workers
            .iter()
            .map(|w| w.host.as_str())
            .collect::<Vec<_>>(),
        vec!["studio.example.test", "loft.example.test"],
        "a resumed rollout must address the machines the journal named, not the roster of the day"
    );
    for (journalled, worker) in journal.participants.iter().zip(&plan.workers) {
        assert_eq!(journalled.fingerprint, worker.fingerprint);
        assert_eq!(
            journalled.keeper_update, worker.keeper_update,
            "the keeper decision is durable, so a rollback proves against the same admission"
        );
    }

    let rolling_back = journal.rolling_back();
    assert_eq!(rolling_back.phase, DeployPhase::RollingBack);
    assert!(
        !rolling_back.can_roll_back(),
        "a transaction already going back is not a choice anyone may make again"
    );
}
