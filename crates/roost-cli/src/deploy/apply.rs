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
use roost_host::coord_config_loader::ENV_WEB_DIST_PATH;
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
use crate::services::service_control::{PlatformServiceManager, ServiceManager};
use crate::services::service_spec::{ServiceRole, ServiceSpec};
use crate::services::web_bundle;

/// Run the apply against this process's own environment, driving the machine's
/// real service manager.
pub async fn run(manifest_bytes: &[u8], env: &dyn EnvSource) -> ApplyReport {
    let platform = match roost_host::supported_host_platform() {
        Ok(platform) => platform,
        Err(error) => {
            return ApplyReport::new(
                ApplyOutcome::Refused,
                format!("this machine's platform cannot be resolved: {error}"),
            );
        }
    };
    run_with(manifest_bytes, env, &mut PlatformServiceManager::new(platform)).await
}

/// Run the apply, and return the report the deploying box reads.
///
/// Every path out of this function is a report: there is no "no report" state,
/// because a target that changed its machine and then failed to say so is the one
/// situation the deploying box cannot reason about.
///
/// The service manager is a parameter rather than a discovery because it is the
/// one collaborator that can change the machine outside a file write, and
/// everything else here — the bundle install, the `ROOST_WEB_DIST_PATH` stamp
/// and the definition that names both — is otherwise untestable end to end: a
/// test that cannot stand in for `systemctl` can only assert on the parts that
/// never reach it. The production caller passes this machine's own manager; a
/// test passes one that answers.
pub async fn run_with(
    manifest_bytes: &[u8],
    env: &dyn EnvSource,
    manager: &mut dyn ServiceManager,
) -> ApplyReport {
    match apply(manifest_bytes, env, manager).await {
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

async fn apply(
    manifest_bytes: &[u8],
    env: &dyn EnvSource,
    manager: &mut dyn ServiceManager,
) -> Result<ApplyReport, ApplyReport> {
    let manifest = ApplyManifest::decode(manifest_bytes)
        .map_err(|cause| ApplyReport::new(ApplyOutcome::Refused, cause))?;
    // The manager's own answer, not a fresh discovery. A deploy and the
    // definition it just wrote must never disagree about which platform they are
    // about, and the manager is the one that will act on it.
    let platform = manager.platform();
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
    if let Err(error) = resolve_interrupted_deploy(&service_dir, manager) {
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

    // The bundle, then the path that names it. Decided HERE rather than carried
    // in the manifest, because only this machine can say where its own release
    // root is: a path decided on the deploying box is a path into ITS version
    // tree, and the value would survive a settlement that deletes it. This is
    // the re-stamp, not a preservation.
    let mut decided = manifest.environment.clone();
    let mut installed_web = None;
    if let Some(staged_web) = staged_web(&staged) {
        let web_dir = web_bundle::release_web_dir(&bin_dir);
        let bundle = web_bundle::install_from_dir(&staged_web, &web_dir)
            .map_err(|cause| ApplyReport::new(ApplyOutcome::Refused, cause.to_string()))?;
        info!(
            release = %release_dir.display(),
            files = bundle.files,
            "remote apply installed the staged web bundle"
        );
        installed_web = Some(web_dir);
    }
    // A manifest from any other deploying box can carry a dist path; a deploy
    // that shipped no bundle must not leave a definition naming one, because
    // the next settlement deletes the release that path points into.
    decided.remove(ENV_WEB_DIST_PATH);

    let install_env = install_environment(env, &decided);
    let mut spec = match ServiceSpec::resolve(
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
    // The dist path is stamped onto the RESOLVED spec, not into the decided map
    // the resolve reads. It is deliberately not in the chosen-entries list —
    // that list means "an operator's answer a redeploy must keep", and a path
    // into a release directory is not one, because a later deploy retires that
    // release. Putting it in `decided` alone installs the bundle and then drops
    // the pointer on the floor: the worker comes up healthy, serves nothing, and
    // `roost status` on the coordinator says the same either way.
    if let Some(web_dir) = &installed_web {
        spec = spec.with_setting(ENV_WEB_DIST_PATH, web_dir.display().to_string());
    }
    let spec = spec.with_decided_one_shots(&decided);
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
        match deploy_service_definition(&spec, platform, &service_dir, manager) {
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

/// The staged `web/` directory, when this deploy shipped one.
fn staged_web(staged: &Path) -> Option<PathBuf> {
    let candidate = staged.join(web_bundle::WEB_DIR_NAME);
    candidate
        .join(web_bundle::WEB_INDEX)
        .is_file()
        .then_some(candidate)
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
