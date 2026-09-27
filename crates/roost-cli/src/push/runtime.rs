//! The real world behind the rollout's decision boundary: a local coordinator,
//! a set of remote participants, and the roster both are read back through.
//! Called by the push command; depends on the coordinator leg, the participant
//! leg, the fleet journal and the roster — and on nothing else in the push
//! group.
//!
//! The journal is read for the ONE fact only it can answer, which is whether the
//! coordinator may still be restored. Everything else this runtime proves is
//! proved against the live roster, because a journal records what was decided
//! and a roster records what is true; a boundary that decided on the first and
//! reported on the second would be a boundary whose answer could not change.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use roost_host::EnvSource;
use tracing::{info, warn};

use crate::command_error::CommandFailure;
use crate::deploy::identity_env::Ambient;
use crate::deploy::keeper_client;
use crate::deploy::run::KEEPER_ATTEMPTS;
use crate::deploy::DeployArgs;
use crate::push::admission::FleetRolloutWorker;
use crate::push::coordinator::{self, CoordinatorLocation};
use crate::push::journal::FleetJournal;
use crate::push::participant;
use crate::push::rollout::convergence::fleet_convergence_problems;
use crate::push::rollout::{FleetRolloutPlan, RolloutAction};
use crate::push::rollout::FleetRuntime as Runtime;
use crate::push::source::RollbackCheckout;
use crate::status::report::WorkerStatus;

/// The gap between two reads of the roster while a fleet is converging.
const POLL: Duration = crate::deploy::run::KEEPER_RETRY;

/// The rollout, bound to this machine's coordinator and its participants.
pub struct FleetRuntime<'a> {
    env: &'a dyn EnvSource,
    location: CoordinatorLocation,
    database: PathBuf,
    plan: FleetRolloutPlan,
    /// The checkout the forward direction builds from.
    source_root: PathBuf,
    /// The checkout the rollback direction builds from, created on demand.
    rollback: RollbackCheckout,
    ambient: Ambient,
    prior_release: PathBuf,
    snapshot_dir: PathBuf,
    /// The heartbeat stamp each participant was showing when it was last moved,
    /// which is what makes "awaiting a post-rollout heartbeat" checkable.
    baselines: Mutex<BTreeMap<String, i64>>,
}

/// Hand-written because the environment is a trait object, which is not `Debug`
/// and whose contents say nothing about which rollout is in flight. The plan and
/// the coordinator's identity do, and those are what a `roost doctor` reader or
/// a panic message needs.
impl std::fmt::Debug for FleetRuntime<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FleetRuntime")
            .field("coordinator", &self.location.label)
            .field("rollout", &self.plan.rollout_id)
            .field("prior_sha", &self.plan.prior_sha)
            .field("target_sha", &self.plan.target_sha)
            .field("participants", &self.plan.workers.len())
            .finish()
    }
}

impl<'a> FleetRuntime<'a> {
    /// Bind a plan to this machine. Nothing is touched until a boundary calls a
    /// method, so a push that never converges anything costs nothing here.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        env: &'a dyn EnvSource,
        location: CoordinatorLocation,
        database: PathBuf,
        plan: FleetRolloutPlan,
        source_root: &Path,
        rollback: RollbackCheckout,
        ambient: Ambient,
        prior_release: &Path,
        snapshot_dir: &Path,
    ) -> Self {
        Self {
            env,
            location,
            database,
            plan,
            source_root: source_root.to_path_buf(),
            rollback,
            ambient,
            prior_release: prior_release.to_path_buf(),
            snapshot_dir: snapshot_dir.to_path_buf(),
            baselines: Mutex::new(BTreeMap::new()),
        }
    }

    /// The arguments one participant is deployed with.
    ///
    /// `force_live` is never set: a push rolls a whole fleet, and a fleet-wide
    /// authorisation to destroy every PTY on every machine is not something an
    /// operator typed once and meant for all of them. Every fleet member's
    /// keeper is therefore carried across, or the machine is deferred.
    fn deploy_args(&self, host: &str, git_sha: &str) -> DeployArgs {
        DeployArgs {
            host: host.to_string(),
            label: None,
            reachable_addr: None,
            source_root: Some(self.source_root.clone()),
            expected_sha: Some(git_sha.to_string()),
            expected_manifest_sha256: None,
            allow_unpublished_local: false,
            coordinator_release: false,
            force_live: false,
        }
    }

    /// Which commit a move is in: a rollback ships the commit the fleet is
    /// leaving, and everything else ships the one it is moving to.
    fn commit_for(&self, action: RolloutAction) -> String {
        match action {
            RolloutAction::Rollback => self.plan.prior_sha.clone(),
            RolloutAction::Hold | RolloutAction::Finalize => self.plan.target_sha.clone(),
        }
    }

    /// The roster, read the way `roost status` reads it: this box's own
    /// coordinator database, read-only, at the instant of the read.
    async fn roster(&self) -> Result<Vec<WorkerStatus>, String> {
        keeper_client::worker_inventory(&self.database, crate::wall_clock::now_ms())
            .await
            .map_err(|failure| failure.message)
    }

    /// Record what a participant was last seen at, so the proof that follows can
    /// tell "converged" from "was already converged before the rollout started".
    async fn record_baseline(&self, worker: &FleetRolloutWorker) {
        let Ok(roster) = self.roster().await else {
            return;
        };
        let Ok(mut baselines) = self.baselines.lock() else {
            warn!("the rollout baseline table is poisoned; heartbeat proofs are weaker");
            return;
        };
        if let Some(row) = roster
            .iter()
            .find(|row| row.fingerprint == worker.fingerprint)
        {
            baselines.insert(worker.fingerprint.clone(), row.last_seen_ms);
        }
    }

    /// Remove the rollback checkout, if a rollback ever made one. Called whether
    /// the transaction settled or not: a worktree left on disk is a stale
    /// administrative record `git worktree prune` would have to guess about.
    pub async fn discard_checkout(&self) {
        self.rollback.discard().await;
    }

    async fn prove_fleet_once(
        &self,
        expected_sha: &str,
        action: RolloutAction,
    ) -> Result<Vec<String>, String> {
        let roster = self.roster().await?;
        // Routability is not re-proved here: it was settled when the fleet was
        // partitioned, and a machine that has since gone away is caught by the
        // staleness term every per-participant proof already carries.
        Ok(fleet_convergence_problems(
            &roster,
            &self.plan.workers,
            expected_sha,
            action,
            None,
            Some(&self.plan.target_sha),
        ))
    }
}

impl Runtime for FleetRuntime<'_> {
    async fn settle_worker(
        &self,
        worker: &FleetRolloutWorker,
        action: RolloutAction,
    ) -> Result<(), String> {
        let git_sha = self.commit_for(action);
        let source_root = match action {
            RolloutAction::Rollback => self.rollback.ensure().await.map_err(text)?,
            RolloutAction::Hold | RolloutAction::Finalize => self.source_root.clone(),
        };
        self.record_baseline(worker).await;
        info!(
            host = %worker.host,
            sha = %git_sha,
            action = action.as_str(),
            "moving a fleet participant"
        );
        let outcome = participant::deploy_participant(
            &self.deploy_args(&worker.host, &git_sha),
            &self.ambient,
            &source_root,
            &git_sha,
        )
        .await
        .map_err(text)?;
        eprintln!(
            ">> {}: {}",
            worker.host,
            if outcome.changed {
                "definition replaced"
            } else {
                "definition already current"
            }
        );
        Ok(())
    }

    async fn prove_fleet(&self, expected_sha: &str, action: RolloutAction) -> Vec<String> {
        let mut problems = Vec::new();
        for _ in 0..KEEPER_ATTEMPTS {
            match self.prove_fleet_once(expected_sha, action).await {
                Ok(settled) if settled.is_empty() => {
                    info!(
                        sha = %expected_sha,
                        action = action.as_str(),
                        "fleet proved at the requested commit"
                    );
                    return Vec::new();
                }
                Ok(observed) => problems = observed,
                Err(cause) => {
                    return vec![format!("the fleet proof could not be read: {cause}")];
                }
            }
            tokio::time::sleep(POLL).await;
        }
        problems
    }

    async fn begin_finalization(&self) -> Result<(), String> {
        // The durable global commit decision. There is no phase after this one
        // and no file left that says a rollback is available, which is exactly
        // why a failure from here on is exit 8 and never a rollback.
        FleetJournal::clear(&self.location.service_dir).map_err(io_text)?;
        info!(rollout = %self.plan.rollout_id, "fleet commit decision recorded");
        Ok(())
    }

    async fn coordinator_can_roll_back(&self) -> Result<bool, String> {
        match FleetJournal::load(&self.location.service_dir) {
            Ok(Some(journal)) => Ok(journal.can_roll_back()),
            Ok(None) => Ok(false),
            Err(cause) => Err(cause),
        }
    }

    async fn finalize_coordinator(&self) -> Result<(), String> {
        coordinator::retire_prior(&self.location, &self.prior_release).map_err(text)?;
        coordinator::kickstart(&self.location).map_err(text)?;
        coordinator::discard_snapshot(&self.snapshot_dir);
        Ok(())
    }

    async fn rollback_coordinator(&self) -> Result<(), String> {
        coordinator::restore_database(&self.database, &self.snapshot_dir).map_err(text)?;
        let program = coordinator::release_program(&self.location, &self.plan.prior_sha);
        let spec =
            coordinator::coordinator_spec(self.env, &self.location, &self.plan.prior_sha, &program)
                .map_err(text)?;
        coordinator::deploy_definition(&self.location, &spec).map_err(text)?;
        coordinator::kickstart(&self.location).map_err(text)?;
        FleetJournal::clear(&self.location.service_dir).map_err(io_text)?;
        coordinator::discard_snapshot(&self.snapshot_dir);
        Ok(())
    }
}

fn text(failure: CommandFailure) -> String {
    failure.message
}

fn io_text(error: std::io::Error) -> String {
    error.to_string()
}
