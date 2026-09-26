//! The coordinator-facing half of a deploy: whether the target's keeper may be
//! carried across, the journaled action that does it, and the proof that it
//! did. Called by the deploy command; depends on the coordinator's own admission
//! contract and on the connect client's generated types, and on nothing else in
//! the deploy group.
//!
//! The two halves are deliberately asymmetric. The action is applied BEFORE the
//! definition is replaced and proved AFTER it, because the action is what frees
//! the PTYs the new release needs to take over, and because a proof taken before
//! the replacement would prove the state the deploy is about to change. The
//! baselines recorded at apply time — the heartbeat and reconciliation stamps
//! the worker was showing when the action was asked for — are what make "after"
//! checkable: without them a keeper that had already converged before the deploy
//! started would satisfy every term of the proof and prove nothing.

use roost_protocol::keeper_update::{
    JournaledKeeperUpdateV1, KeeperContractV1, PRESERVE, keeper_update_outcome_matches_action,
};
use tracing::info;

use crate::command_error::CommandFailure;
use crate::deploy::admission;
use crate::deploy::codes;
use crate::deploy::convergence::{self, Direction};
use crate::deploy::identity_env::Ambient;
use crate::deploy::keeper_client::{self, CoordinatorLink};
use crate::deploy::run::KEEPER_ATTEMPTS;
use crate::deploy::target_evidence::{self, EvidenceVerdict, TargetEvidence};
use crate::deploy::{DeployArgs, ssh};
use crate::services::service_environment::ENV_BOOTSTRAP_TOKEN;
use crate::status::report::WorkerStatus;

/// What the coordinator said this deploy may do to the target's keeper, and the
/// baselines a later proof needs.
#[derive(Debug, Clone)]
pub struct KeeperPlan {
    /// The action to apply through the coordinator's fence, when there is one.
    pub update: Option<JournaledKeeperUpdateV1>,
    /// The worker the action is addressed to.
    pub fingerprint: Option<String>,
    /// The heartbeat stamp the worker was showing when the action was applied.
    pub heartbeat_baseline_ms: i64,
    /// The keeper reconciliation stamp the worker was showing then.
    pub reconciliation_baseline_ms: i64,
}

/// Decide the keeper action, and apply it through the coordinator's fence.
///
/// Every refusal here happens BEFORE the target's definition is replaced, so a
/// machine whose keeper cannot be safely carried across is left exactly as it
/// was. That ordering is the reason this is a separate function: the answers are
/// worth having early, and the action is worth having late.
pub async fn plan_and_apply(
    args: &DeployArgs,
    ambient: &Ambient,
    _installed: &crate::status::service_definition::InstalledEnvironment,
    target_contract: &KeeperContractV1,
    coordinator_url: &str,
    staged_program: &str,
) -> Result<KeeperPlan, CommandFailure> {
    let link = match CoordinatorLink::new(coordinator_url, coordinator_token(ambient)) {
        Ok(link) => Some(link),
        // No usable coordinator address means no proof can exist for this
        // worker, and demanding one anyway would make a fresh target
        // permanently undeployable. What the target itself says is then the
        // whole decision, which is what the evidence probe below is for.
        Err(failure) => {
            info!(reason = %failure.message, "no coordinator is addressed for keeper admission");
            None
        }
    };
    let inventory = match &link {
        None => Vec::new(),
        Some(_) => local_inventory(crate::wall_clock::now_ms()).await?,
    };
    let bootstrap_allowed =
        args.allow_unpublished_local || ambient.contains_key(ENV_BOOTSTRAP_TOKEN);
    let staging = admission::keeper_admission_staging(
        &platform_label(),
        admission::direct_keeper_update_admission(
            &args.host,
            target_contract,
            bootstrap_allowed,
            &inventory,
        )?,
    );
    if staging.keeper_update.is_none()
        && let Some(refusal) = staging.installed_service_refusal.clone()
    {
        permit_or_refuse(&args.host, &refusal, staged_program).await?;
    }
    let Some(update) = staging.keeper_update else {
        return Ok(KeeperPlan {
            update: None,
            fingerprint: None,
            heartbeat_baseline_ms: 0,
            reconciliation_baseline_ms: 0,
        });
    };
    let Some(fingerprint) = staging.worker_fingerprint.clone() else {
        return Err(codes::refuse(
            codes::KEEPER_NOT_ADOPTABLE,
            "the keeper admission produced an action with no worker to address it to",
        ));
    };
    let link = link.ok_or_else(|| {
        codes::refuse(
            codes::KEEPER_NOT_ADOPTABLE,
            format!(
                "{fingerprint} needs a keeper action and this deploy has no coordinator to fence it \
                 with; set {} and re-run",
                keeper_client::COORD_URL_ENV
            ),
        )
    })?;
    let observed = apply_through_the_fence(&link, &fingerprint, &update, &inventory).await?;
    // A preserve is proven against the identity the coordinator's drain settled
    // on, not against the one recorded before the drain — the drain is allowed
    // to restart the keeper, and a proof that refused the common case would be a
    // proof nobody runs.
    let proven = if update.admission.required_action == PRESERVE {
        convergence::with_observed_identity(
            &update,
            observed.keeper_pid,
            &observed.keeper_epoch,
            &observed.binding_digest,
        )
        .map_err(|error| {
            codes::refuse(
                codes::KEEPER_NOT_ADOPTABLE,
                format!("the keeper identity the coordinator returned is not admissible: {error}"),
            )
        })?
    } else {
        update
    };
    info!(
        worker = %fingerprint,
        action = %proven.admission.required_action,
        "keeper action applied through the coordinator's fence"
    );
    Ok(KeeperPlan {
        update: Some(proven),
        fingerprint: Some(fingerprint),
        heartbeat_baseline_ms: observed.last_seen_ms,
        reconciliation_baseline_ms: observed.reconciled_at_ms,
    })
}

/// The keeper identity and reconciliation stamps as the coordinator reported
/// them, with the recorded values as the fallback for a field it left unset.
struct FencedOutcome {
    keeper_pid: i64,
    keeper_epoch: String,
    binding_digest: String,
    reconciled_at_ms: i64,
    last_seen_ms: i64,
}

/// Ask the coordinator to fence the action, and check that the outcome it
/// reports is one the recorded action permits.
///
/// That check is not bookkeeping. An outcome this build does not recognise must
/// never be read as "the keeper was handled": `keeper_update_outcome_matches_action`
/// routes an unrecognised action into the shutdown branch precisely so a future
/// coordinator cannot get here by accident.
async fn apply_through_the_fence(
    link: &CoordinatorLink,
    fingerprint: &str,
    update: &JournaledKeeperUpdateV1,
    inventory: &[WorkerStatus],
) -> Result<FencedOutcome, CommandFailure> {
    let recorded = update.admission.clone();
    let encoded = serde_json::to_string(update)
        .map_err(|error| CommandFailure::generic(error.to_string()))?;
    let mut last = CommandFailure::generic("the keeper action was not attempted".to_string());
    for attempt in 0..KEEPER_ATTEMPTS {
        if attempt > 0 {
            tokio::time::sleep(crate::deploy::run::KEEPER_RETRY).await;
        }
        let Some(worker) = inventory
            .iter()
            .find(|worker| worker.fingerprint == fingerprint)
        else {
            last = CommandFailure::generic(format!(
                "{fingerprint}: the coordinator no longer lists this worker"
            ));
            continue;
        };
        let Some(reconciled_at_ms) = worker
            .keeper_runtime
            .as_ref()
            .map(|runtime| runtime.reconciled_at_ms)
        else {
            last = CommandFailure::generic(format!(
                "{}: the worker has reported no keeper reconciliation, so there is no baseline to \
                 prove a convergence against",
                worker.label
            ));
            continue;
        };
        let outcome = link
            .prepare_keeper_action(
                fingerprint,
                &encoded,
                Direction::Target.as_str(),
                false,
                false,
            )
            .await?;
        if !keeper_update_outcome_matches_action(&recorded.required_action, &outcome.outcome) {
            last = CommandFailure::generic(format!(
                "the worker returned outcome {} for a {} keeper action",
                outcome.outcome, recorded.required_action
            ));
            continue;
        }
        return Ok(FencedOutcome {
            keeper_pid: outcome.keeper_pid.unwrap_or(recorded.expected_keeper_pid),
            keeper_epoch: outcome
                .keeper_epoch
                .unwrap_or_else(|| recorded.expected_keeper_epoch.clone()),
            binding_digest: outcome
                .binding_digest
                .unwrap_or_else(|| recorded.expected_binding_digest.clone()),
            reconciled_at_ms,
            last_seen_ms: worker.last_seen_ms,
        });
    }
    Err(last)
}

/// Ask the target whether it is safe to stage, and refuse when it is not.
///
/// The refusal is a claim about the COORDINATOR's registry; only the target can
/// decide whether it stands, so it is decided here rather than where it was
/// raised. Every unknown on the far side is a refusal, because an unreachable
/// service manager reads exactly like a stopped one.
pub async fn permit_or_refuse(
    host: &str,
    refusal: &str,
    staged_program: &str,
) -> Result<(), CommandFailure> {
    let evidence = evidence_on(host, staged_program).await?;
    decide(refusal, host, evidence)
}

/// The decision, as a pure function of the evidence so it can be proven without
/// a target.
pub fn decide(refusal: &str, host: &str, evidence: TargetEvidence) -> Result<(), CommandFailure> {
    match target_evidence::installed_service_verdict(refusal, host, evidence) {
        EvidenceVerdict::Refused(refusal) => {
            Err(codes::refuse(codes::KEEPER_NOT_ADOPTABLE, refusal))
        }
        EvidenceVerdict::Permitted(evidence) => {
            eprintln!(">> keeper admission on {host}: staging permitted because {evidence}.");
            Ok(())
        }
    }
}

/// The evidence block for a target, read by the release this deploy staged.
///
/// The probe is that release's own code rather than a shell string assembled
/// here, so the label and the definition path it asks about are the ones the
/// install actually uses. A probe that guessed them would be a probe of a
/// definition that may not exist.
pub async fn evidence_on(
    host: &str,
    staged_program: &str,
) -> Result<TargetEvidence, CommandFailure> {
    let command = format!(
        "{quoted} __remote-evidence",
        quoted = roost_platform::posix_shell_quote(staged_program)
    );
    let outcome = ssh::exec(host, &command).await?;
    Ok(target_evidence::parse_target_evidence(
        outcome.exit,
        &outcome.stdout,
        roost_host::supported_host_platform()?,
    ))
}

/// Prove the keeper converged, and refuse when it did not.
///
/// Bounded and then honest: after the last attempt the message is what was still
/// unobserved, because "the deploy timed out" tells an operator nothing about
/// which term of the proof was missing.
#[allow(clippy::too_many_arguments)]
pub async fn prove_convergence(
    coordinator_url: &str,
    host: &str,
    fingerprint: &str,
    update: &JournaledKeeperUpdateV1,
    heartbeat_baseline_ms: i64,
    reconciliation_baseline_ms: i64,
    expected_worker_sha: &str,
) -> Result<(), CommandFailure> {
    let _link = CoordinatorLink::new(coordinator_url, None)?;
    let mut problem = format!("{fingerprint}: keeper convergence was not observed");
    for attempt in 0..KEEPER_ATTEMPTS {
        if attempt > 0 {
            tokio::time::sleep(crate::deploy::run::KEEPER_RETRY).await;
        }
        let inventory = local_inventory(crate::wall_clock::now_ms()).await?;
        let Some(worker) = inventory
            .iter()
            .find(|worker| worker.fingerprint == fingerprint)
        else {
            problem = format!("{fingerprint}: the coordinator no longer lists this worker");
            continue;
        };
        if let Some(worker_sha) = &worker.git_sha
            && !worker_sha.eq_ignore_ascii_case(expected_worker_sha)
        {
            problem = format!(
                "{}: the worker is running {}, not the {} this deploy installed",
                worker.label,
                short_sha(worker_sha),
                short_sha(expected_worker_sha)
            );
            continue;
        }
        match convergence::convergence_problem(
            worker,
            update,
            Direction::Target,
            Some(heartbeat_baseline_ms),
            Some(reconciliation_baseline_ms),
        ) {
            None => return Ok(()),
            Some(observed) => problem = observed,
        }
    }
    Err(codes::refuse(
        codes::KEEPER_NOT_ADOPTABLE,
        format!("keeper update convergence proof on {host} failed: {problem}"),
    ))
}

fn short_sha(sha: &str) -> String {
    sha.chars().take(8).collect()
}

/// The coordinator's own roster, read from this box's coordinator database.
///
/// Read-only, from the installed path, exactly as `roost status` reads it: the
/// deploying box is the coordinator's host in the supported arrangement, and a
/// second way to ask a coordinator for its roster would be a second answer to
/// "what does the fleet look like right now".
async fn local_inventory(now_ms: i64) -> Result<Vec<WorkerStatus>, CommandFailure> {
    let database = local_coordinator_database()?;
    if !database.exists() {
        return Ok(Vec::new());
    }
    keeper_client::worker_inventory(&database, now_ms).await
}

fn local_coordinator_database() -> Result<std::path::PathBuf, CommandFailure> {
    let platform = roost_host::supported_host_platform()?;
    let data_dir = crate::services::service_spec::ServiceRole::Coordinator
        .data_dir(&roost_host::ProcessEnv::new(), platform)?;
    Ok(data_dir.join(roost_host::coord_config::COORD_DB_FILE_NAME))
}

fn platform_label() -> String {
    roost_host::supported_host_platform()
        .map(|platform| platform.display_name().to_string())
        .unwrap_or_else(|_| "POSIX".to_string())
}

fn coordinator_token(ambient: &Ambient) -> Option<String> {
    ambient.get(keeper_client::CLI_TOKEN_ENV).cloned()
}
