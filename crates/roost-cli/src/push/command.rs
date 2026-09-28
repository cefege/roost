//! `roost push` itself: prove the commit, publish it, decide what the fleet can
//! converge, hold the local coordinator, and hand the transaction to the
//! decision boundary. Called by the crate's dispatcher; depends on the deploy
//! group's identity proofs and on the coordinator's roster, and on the rest of
//! the push group.
//!
//! The order is the safety property and it is worth stating once. Nothing is
//! mutated until the commit is proved and published, the registry's identity is
//! whole, every participant is classified and at least one of them is safe to
//! touch. After that the local coordinator is held — journal written, release
//! installed, definition swapped and proven up — and only then does the rollout
//! converge a single participant. Every refusal above the hold leaves the fleet
//! exactly where it was.
//!
//! The answer an operator asked for is the one line on stdout. Everything the
//! command is doing on the way there is stderr, and everything the FLEET did is
//! a `tracing` event.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use roost_host::{EnvSource, HostPlatform, ProcessEnv};
use roost_protocol::keeper_update::KeeperContractV1;
use tracing::info;

use crate::command_error::CommandFailure;
use crate::deploy::DeployArgs;
use crate::deploy::codes;
use crate::deploy::identity::{self, DIRTY_SUFFIX};
use crate::deploy::identity_env;
use crate::deploy::invocation;
use crate::deploy::release;
use crate::deploy::ssh;
use crate::ops::reset::coordinator_database;
use crate::push::PushArgs;
use crate::push::admission::{self, FleetKeeperAdmission, FleetRolloutWorker};
use crate::push::coordinator::{self, CoordinatorLocation};
use crate::push::journal::FleetJournal;
use crate::push::plan::{
    self, DeferredFleetWorker, FleetRolloutPartition, FleetRolloutTarget, PushRefusal,
};
use crate::push::rollout::convergence;
use crate::push::rollout::{self, FleetRolloutPlan, FleetRuntime};
use crate::push::runtime::FleetRuntime as PushRuntime;
use crate::push::source::{self, RollbackCheckout};
use crate::status::collect::{StatusContext, collect};
use crate::status::report::WorkerStatus;
use crate::wall_clock::now_ms;

/// Run `roost push`. It takes no arguments: a push with a flag is a push that
/// was asked for something other than the whole fleet, and the fleet is the
/// unit the transaction commits or does not.
pub async fn push(_args: &PushArgs) -> Result<std::process::ExitCode, CommandFailure> {
    let env = ProcessEnv::new();
    let platform = roost_host::supported_host_platform()?;
    let location = coordinator::locate(&env, platform)?;
    // A definition swap a previous run left in flight is resolved before this
    // one writes a journal of its own over the top of it. The coordinator's
    // rollback point is the machine journal's, and push has no other.
    coordinator::resolve_interrupted(&location)?;

    let source_root = deploy_source_root()?;
    let target_sha = prove_and_publish(&source_root).await?;

    let installed = coordinator::installed_environment(&location)?;
    let (prior_sha, prior_release) = coordinator::installed_release(&installed, &location)?;
    if prior_sha == target_sha {
        return report_settled_coordinator(&target_sha);
    }

    let roster = read_roster(&env, platform).await?;
    let candidates = plan::resolve_push_targets(&roster).map_err(PushRefusal::into_failure)?;
    let routable = routable_fingerprints(&candidates).await;
    let partition = plan::partition_fleet_for_rollout(&candidates, &roster, &routable, &prior_sha);
    refuse_if_nobody_can_move(&partition, &prior_sha, &target_sha)?;

    let admitted = admit_keepers(&source_root, &partition, &roster, &target_sha).await?;
    let deferred = [partition.deferred, admitted.deferred].concat();
    if admitted.workers.is_empty() {
        return Err(refusal_with_deferred(
            &format!("no registered worker can be updated safely; zero mutation at {target_sha}"),
            &deferred,
        ));
    }
    if already_converged(&admitted.workers, &roster, &target_sha) {
        return report_already_satisfied(&admitted.workers, &deferred, &target_sha);
    }

    let plan = FleetRolloutPlan {
        rollout_id: rollout_id(),
        admission_recorded_at_ms: now_ms(),
        prior_sha: prior_sha.clone(),
        target_sha: target_sha.clone(),
        workers: admitted.workers,
    };
    let converged = hold_and_converge(
        &env,
        &location,
        &source_root,
        &plan,
        &target_sha,
        &prior_sha,
        &prior_release,
    )
    .await;
    match converged {
        Ok(()) => {
            println!(
                ">> push complete — coordinator and {} workers report {target_sha}",
                plan.workers.len()
            );
            for line in plan::deferred_fleet_report_lines(&deferred) {
                println!("{line}");
            }
            Ok(std::process::ExitCode::SUCCESS)
        }
        Err(failure) => {
            for line in plan::deferred_fleet_report_lines(&deferred) {
                eprintln!("{line}");
            }
            Err(failure)
        }
    }
}

/// The checkout to publish and build from: the operator's working tree, or the
/// one this binary was built from — the same default `roost deploy` resolves.
fn deploy_source_root() -> Result<PathBuf, CommandFailure> {
    invocation::source_root(&DeployArgs {
        host: String::new(),
        label: None,
        reachable_addr: None,
        source_root: None,
        expected_sha: None,
        expected_manifest_sha256: None,
        allow_unpublished_local: false,
        coordinator_release: false,
        force_live: false,
        // `roost push` always builds from its own checkout, so neither of the
        // flags that redirect an install at something else is ever set here.
        web_dist: None,
        release: None,
    })
}

/// Prove the commit is clean, publish it, and prove it landed. Every refusal
/// here is exit 7, and every one of them happens before anything is mutated.
async fn prove_and_publish(source_root: &Path) -> Result<String, CommandFailure> {
    let sha = identity::local_git_sha_or_die(source_root).await?;
    if sha.ends_with(DIRTY_SUFFIX) || !is_full_commit(&sha) {
        return Err(codes::refuse(
            codes::IDENTITY_UNPROVED,
            "roost push requires a clean full Git commit: a fleet is rolled onto the exact tree \
             every machine will then run, and a working tree with uncommitted changes in it is \
             not a commit any machine can be checked out of",
        ));
    }
    eprintln!(">> git push");
    source::publish(source_root).await?;
    identity::published_git_sha_or_die(source_root, Some(&sha)).await
}

fn is_full_commit(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The fleet as the running coordinator reports it right now, with the whole
/// registry's identity proved whole.
async fn read_roster(
    env: &dyn EnvSource,
    platform: HostPlatform,
) -> Result<Vec<WorkerStatus>, CommandFailure> {
    let collected = collect(&StatusContext {
        env,
        platform,
        now_ms: now_ms(),
        endpoint_override: None,
    })
    .await?;
    if !collected.report.coord.reachable {
        return Err(codes::refuse(
            codes::SETTLEMENT_FAILED,
            "the local coordinator is not answering its own identity RPC; a fleet cannot be \
             rolled against a coordinator that cannot say what it is",
        ));
    }
    let problems = plan::fleet_worker_identity_problems(&collected.report.workers);
    if !problems.is_empty() {
        return Err(codes::refuse(
            codes::SETTLEMENT_FAILED,
            format!(
                "coordinator worker identity is not provable; zero mutation:\n{}",
                problems.join("\n")
            ),
        ));
    }
    Ok(collected.report.workers)
}

/// The machines this box can actually reach right now, proved by asking.
///
/// Not inferred from a heartbeat: a worker that was fresh five minutes ago and a
/// worker on a laptop that went to sleep produce the same row, and only the
/// second of those may be deferred.
async fn routable_fingerprints(candidates: &[FleetRolloutTarget]) -> BTreeSet<String> {
    let mut routable = BTreeSet::new();
    for candidate in candidates {
        if ssh::require_reachable(&candidate.host).await.is_ok() {
            routable.insert(candidate.fingerprint.clone());
        }
    }
    routable
}

/// Refuse a push with nothing to converge, and say why each machine was not
/// converged rather than leaving an operator to guess.
fn refuse_if_nobody_can_move(
    partition: &FleetRolloutPartition,
    prior_sha: &str,
    target_sha: &str,
) -> Result<(), CommandFailure> {
    if !partition.participants.is_empty() {
        return Ok(());
    }
    Err(refusal_with_deferred(
        &format!(
            "no registered worker is reachable and on the prior commit {prior_sha}; zero mutation \
             towards {target_sha}"
        ),
        &partition.deferred,
    ))
}

/// Classify every candidate's keeper against the contract its release ships.
async fn admit_keepers(
    source_root: &Path,
    partition: &FleetRolloutPartition,
    roster: &[WorkerStatus],
    target_sha: &str,
) -> Result<FleetKeeperAdmission, CommandFailure> {
    let mut contracts: BTreeMap<String, KeeperContractV1> = BTreeMap::new();
    for candidate in &partition.participants {
        if contracts.contains_key(&candidate.fingerprint) {
            continue;
        }
        if let Ok(contract) = target_contract_for(source_root, &candidate.host, target_sha).await {
            contracts.insert(candidate.fingerprint.clone(), contract);
        }
    }
    Ok(admission::classify_fleet_keeper_updates(
        &partition.participants,
        roster,
        &contracts,
    ))
}

/// The contract the release a machine will run ships, read from its staged bytes.
///
/// Read by running the staged `roost` and never from this process: in this
/// process `roost-keeper` is whatever release the CLI was built from, and an
/// admission decided against those bytes is a decision about the wrong program.
async fn target_contract_for(
    source_root: &Path,
    host: &str,
    target_sha: &str,
) -> Result<KeeperContractV1, CommandFailure> {
    let platform = ssh::remote_platform(host).await?;
    let arch = ssh::remote_arch(host).await?;
    let triple = release::target_triple(platform, &arch)?;
    let staged = release::build_release(source_root, triple, None).await?;
    invocation::target_contract(&staged.keeper_contract, target_sha)
}

/// Whether every participant is provably on the target with no keeper work left.
fn already_converged(
    workers: &[FleetRolloutWorker],
    roster: &[WorkerStatus],
    target_sha: &str,
) -> bool {
    convergence::participants_need_no_keeper_work(workers)
        && workers.iter().all(|worker| {
            roster.iter().any(|row| {
                row.fingerprint == worker.fingerprint && row.git_sha.as_deref() == Some(target_sha)
            })
        })
}

/// Hold the local coordinator at the fleet-converging point, then converge.
async fn hold_and_converge(
    env: &dyn EnvSource,
    location: &CoordinatorLocation,
    source_root: &Path,
    plan: &FleetRolloutPlan,
    target_sha: &str,
    prior_sha: &str,
    prior_release: &Path,
) -> Result<(), CommandFailure> {
    let database = coordinator_database(env, location.platform)?;
    let rollback = RollbackCheckout::plan(&location.service_dir, source_root, prior_sha);
    let snapshot_dir = coordinator::snapshot_database(location, &database, &plan.rollout_id)?;
    let runtime = PushRuntime::new(
        env,
        location.clone(),
        database,
        plan.clone(),
        source_root,
        rollback,
        identity_env::ambient_environment(),
        prior_release,
        &snapshot_dir,
    );

    let triple = release::target_triple(location.platform, std::env::consts::ARCH)?;
    let staged = release::build_release(source_root, triple, None).await?;
    eprintln!(">> stage the coordinator release {target_sha}");
    coordinator::install_staged_release(location, &staged, target_sha)?;
    // The durable transaction, written before the first byte of the
    // coordinator's own machine is replaced. Nothing above this line mutates
    // anything, so every refusal so far left the fleet exactly where it was.
    FleetJournal::open(plan, plan.admission_recorded_at_ms)
        .write(&location.service_dir)
        .map_err(|error| {
            CommandFailure::generic(format!("cannot record the fleet transaction: {error}"))
        })?;
    let program = coordinator::release_program(location, target_sha);
    let spec = coordinator::coordinator_spec(env, location, target_sha, &program)?;
    if let Err(forward) = coordinator::deploy_definition(location, &spec) {
        // The fleet journal stays on disk unless the coordinator itself comes
        // back, so an interrupted hold is resumed rather than mistaken for a
        // push that never happened.
        return match runtime.rollback_coordinator().await {
            Ok(()) => Err(forward),
            Err(rollback) => Err(codes::refuse(
                codes::SETTLEMENT_FAILED,
                format!(
                    "the coordinator could not be held and could not be restored; the fleet \
                     transaction journal is retained:\n{}\n{rollback}",
                    forward.message
                ),
            )),
        };
    }
    eprintln!(">> converging {} participants", plan.workers.len());
    let outcome = rollout::converge_atomic_fleet(plan, &runtime).await;
    runtime.discard_checkout().await;
    outcome
}

/// A refusal that names every machine it left alone, one per line.
fn refusal_with_deferred(headline: &str, deferred: &[DeferredFleetWorker]) -> CommandFailure {
    if deferred.is_empty() {
        return codes::refuse(codes::SETTLEMENT_FAILED, headline);
    }
    let details: Vec<String> = deferred
        .iter()
        .map(|machine| format!("   {}: {}", machine.label, machine.reason))
        .collect();
    codes::refuse(
        codes::SETTLEMENT_FAILED,
        format!("{headline}\n{}", details.join("\n")),
    )
}

/// The fleet is already on this commit: there is nothing to roll, and saying so
/// is the whole answer.
fn report_settled_coordinator(target_sha: &str) -> Result<std::process::ExitCode, CommandFailure> {
    info!(
        sha = %target_sha,
        "the coordinator already runs the requested commit"
    );
    println!(">> push complete — coordinator already reports {target_sha}");
    Ok(std::process::ExitCode::SUCCESS)
}

/// The coordinator, every participant and every keeper already satisfy the
/// commit, and the machines this push left alone are printed beside that.
fn report_already_satisfied(
    workers: &[FleetRolloutWorker],
    deferred: &[DeferredFleetWorker],
    target_sha: &str,
) -> Result<std::process::ExitCode, CommandFailure> {
    println!(
        ">> push complete — coordinator, {} workers, and keepers already satisfy {target_sha}",
        workers.len()
    );
    for line in plan::deferred_fleet_report_lines(deferred) {
        println!("{line}");
    }
    Ok(std::process::ExitCode::SUCCESS)
}

/// The identity one transaction's participants, journal and rollback checkout
/// share. Two rollbacks in one process must never collide, and a pid alone is
/// reused.
fn rollout_id() -> String {
    format!("{}-{}", std::process::id(), now_ms())
}
