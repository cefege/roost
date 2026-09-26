//! The target side of a deploy: the hidden `roost __remote-apply` that runs ON
//! the machine being changed, from the staged release, with the machine
//! transaction held. Called by the deploying box over ssh and by nothing else;
//! depends on the services group's own deploy transaction, which owns the
//! journal, the rollback and the settlement.
//!
//! This is deliberately the half that runs on the target rather than the half
//! that runs on the deploying box. Everything that decides what the target's
//! service definition says has to be decided against the target's own installed
//! definition, the target's own release root and the target's own journal — and
//! an ssh round trip that recomputed any of those on the far side would be a
//! second answer to each of them. So the far side sends a manifest and this code
//! answers with a report.
//!
//! Three refusals are load-bearing and each is a class of bug that has already
//! happened:
//!
//! - No machine transaction held means nothing serialized this against the other
//!   things that mutate a machine. The apply refuses rather than running
//!   unserialized, so the only way to reach it is through a deploy that took the
//!   lock.
//! - A release whose digest does not match the manifest is a release nobody
//!   proved. It is refused before the definition is touched.
//! - A deploy another run left in flight is resolved FIRST, by putting back the
//!   definition that was working. A deploy that overwrote a retained journal
//!   with a new one has destroyed the only record of the swap still in flight.

use std::path::{Path, PathBuf};

use roost_host::EnvSource;
use tracing::{info, warn};

use crate::deploy::apply_release::{
    RELEASE_BIN_DIR, ROOST_PROGRAM, install_environment, install_release, process_environment,
    read_installed, reject_staged_path, release_root_for,
};
use crate::deploy::installed::installed_release_dir;
use crate::deploy::machine_txn;
use crate::deploy::manifest::{ApplyManifest, ApplyOutcome, ApplyReport};
use crate::deploy::release::release_digest;
use crate::deploy::retire::retire_prior_release;
use crate::services::deploy_journal::DeployJournal;
use crate::services::deploy_transaction::{
    DeployError, RollbackOutcome, deploy_service_definition, resolve_interrupted_deploy,
};
use crate::services::service_control::PlatformServiceManager;
use crate::services::service_spec::{ServiceRole, ServiceSpec};

/// Run the apply, and return the report the deploying box reads.
///
/// Every path out of this function is a report: there is no "no report" state,
/// because a target that changed its machine and then failed to say so is the one
/// situation the deploying box cannot reason about.
pub async fn run(manifest_bytes: &[u8], env: &dyn EnvSource) -> ApplyReport {
    match apply(manifest_bytes, env).await {
        Ok(report) => report,
        Err(report) => {
            warn!(
                outcome = ?report.outcome,
                detail = %report.detail,
                "remote apply did not settle"
            );
            report
        }
    }
}

/// Run the apply against this process's own environment.
pub async fn run_here(manifest_bytes: &[u8]) -> ApplyReport {
    run(manifest_bytes, process_environment()).await
}

async fn apply(manifest_bytes: &[u8], env: &dyn EnvSource) -> Result<ApplyReport, ApplyReport> {
    let manifest = ApplyManifest::decode(manifest_bytes)
        .map_err(|cause| ApplyReport::new(ApplyOutcome::Refused, cause))?;
    let platform = roost_host::supported_host_platform()
        .map_err(|error| ApplyReport::new(ApplyOutcome::Refused, error.to_string()))?;
    let service_dir = roost_host::roost_service_dir(env, platform)
        .map_err(|error| ApplyReport::new(ApplyOutcome::Refused, error.to_string()))?;
    let definition_path = ServiceRole::Worker
        .definition_path(env, platform)
        .map_err(|error| ApplyReport::new(ApplyOutcome::Refused, error.to_string()))?;
    let definition_path_text = definition_path.display().to_string();

    // The machine transaction is a precondition, not a courtesy. Reading it
    // rather than taking it here is what makes "run the apply by hand" a
    // refusal instead of an unsynchronized mutation of a live machine.
    let held = machine_txn::active_transaction(&machine_txn::lock_path(&service_dir))
        .await
        .map_err(|error| ApplyReport::new(ApplyOutcome::Refused, error.to_string()))?;
    if held.is_none() {
        return Err(ApplyReport::new(
            ApplyOutcome::Refused,
            "no machine transaction is held on this machine; a deploy must take one before it \
             applies a release",
        ));
    }

    let staged = PathBuf::from(&manifest.staged_dir);
    reject_staged_path(&staged)
        .map_err(|failure| ApplyReport::new(ApplyOutcome::Refused, failure.message))?;
    let digest = release_digest(&staged.join(RELEASE_BIN_DIR))
        .map_err(|error| ApplyReport::new(ApplyOutcome::Refused, error.to_string()))?;
    if digest != manifest.release_digest {
        return Err(ApplyReport::new(
            ApplyOutcome::Refused,
            format!(
                "the staged release at {} hashes to {digest}, not the {} this deploy was prepared \
                 for; nothing on this machine was changed",
                staged.display(),
                manifest.release_digest
            ),
        ));
    }

    // An earlier run's unfinished swap is resolved before anything else, so the
    // definition this deploy replaces is one the machine can actually run.
    let mut manager = PlatformServiceManager::new(platform);
    if let Err(error) = resolve_interrupted_deploy(&service_dir, &mut manager) {
        return Err(ApplyReport::new(
            ApplyOutcome::Refused,
            format!("an unfinished deploy on this machine could not be resolved: {error}"),
        ));
    }
    let journal = DeployJournal::load(&service_dir)
        .map_err(|cause| ApplyReport::new(ApplyOutcome::Refused, cause))?;
    let recovered = journal.is_some();

    let prior_definition = read_installed(&definition_path);
    let prior_release_dir = prior_definition
        .as_deref()
        .and_then(|definition| installed_release_dir(definition, platform))
        .and_then(|release| release.parent().map(Path::to_path_buf));

    let release_root = release_root_for(env, platform, prior_release_dir.as_deref())
        .map_err(|cause| ApplyReport::new(ApplyOutcome::Refused, cause))?;
    let release_dir = release_root.join(&manifest.git_sha);
    let bin_dir = release_dir.join(RELEASE_BIN_DIR);
    install_release(&staged.join(RELEASE_BIN_DIR), &bin_dir)
        .map_err(|cause| ApplyReport::new(ApplyOutcome::Refused, cause))?;

    let install_env = install_environment(env, &manifest.environment);
    let spec = match ServiceSpec::resolve(
        ServiceRole::Worker,
        &install_env,
        platform,
        &bin_dir.join(ROOST_PROGRAM),
    ) {
        Ok(spec) => spec,
        Err(error) => {
            return Err(ApplyReport::new(
                ApplyOutcome::Refused,
                format!("this machine cannot resolve a worker definition: {error}"),
            ));
        }
    };
    crate::services::install::ensure_service_directories(&spec)
        .map_err(|error| ApplyReport::new(ApplyOutcome::Refused, error.to_string()))?;

    info!(
        label = %spec.label,
        release = %release_dir.display(),
        "remote apply installing the staged release"
    );
    let prior_release_text = prior_release_dir
        .as_ref()
        .map(|path| path.display().to_string());
    Ok(
        match deploy_service_definition(&spec, platform, &service_dir, &mut manager) {
            Ok(deployed) => settle_report(
                ApplyReport {
                    definition_changed: deployed.definition_changed,
                    definition_path: deployed.definition_path.display().to_string(),
                    release_dir: Some(release_dir.display().to_string()),
                    prior_release_dir: prior_release_text,
                    ..ApplyReport::new(
                        if recovered {
                            ApplyOutcome::Recovered
                        } else {
                            ApplyOutcome::Settled
                        },
                        format!("{} is running the staged release", spec.label),
                    )
                },
                &release_root,
                prior_release_dir.as_deref(),
            ),
            Err(DeployError::Activation { rolled_back, .. })
            | Err(DeployError::Refused { rolled_back, .. }) => ApplyReport {
                definition_path: definition_path_text,
                prior_release_dir: prior_release_text,
                ..ApplyReport::new(
                    if rolled_back == RollbackOutcome::Failed {
                        ApplyOutcome::Unsettled
                    } else {
                        ApplyOutcome::RolledBack
                    },
                    format!(
                        "{} did not come up on the staged release; {}",
                        spec.label,
                        rolled_back.display_name()
                    ),
                )
            },
            Err(error) => ApplyReport {
                definition_path: definition_path_text,
                ..ApplyReport::new(
                    ApplyOutcome::Refused,
                    format!("nothing was changed on this machine: {error}"),
                )
            },
        },
    )
}

/// Retire the release the definition used to point at, once the new one is proven
/// up — and only then.
///
/// A retirement that ran on the failure path would leave a machine whose service
/// definition points at a release that no longer exists, which is the one outcome
/// worse than a failed deploy. So retirement happens here, on the settled arm
/// only, and inside the install's own release root.
fn settle_report(
    mut report: ApplyReport,
    release_root: &Path,
    prior_release_dir: Option<&Path>,
) -> ApplyReport {
    let Some(prior) = prior_release_dir else {
        return report;
    };
    if prior.parent() != Some(release_root) {
        warn!(
            prior = %prior.display(),
            root = %release_root.display(),
            "the prior release is not inside this install's release root, so it was not retired"
        );
        return report;
    }
    if let Err(cause) = retire_prior_release(release_root, prior) {
        // Retirement is cleanup, not the deploy. A prior release that could not
        // be removed is worth saying out loud and is not worth failing a deploy
        // that already proved the new release is serving.
        report.detail = format!(
            "{}; the prior release {} was not retired: {cause}",
            report.detail,
            prior.display()
        );
    }
    report
}
