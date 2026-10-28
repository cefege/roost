//! `roost __self-update --to <tag>`: move this worker onto a tagged release.
//!
//! The worker starts this process outside its own unit when the coordinator
//! reports a different release. It is the install `roost deploy --release <tag>`
//! makes, without the ssh hop: the release is fetched by explicit tag, checked
//! against its own claim about itself, installed under `versions/<tag>/`, and the
//! service definition is rewritten and restarted by the same apply that deploy
//! uses, under this machine's transaction. A failed activation rolls back to the
//! release it left. The keeper is not replaced here; the staged keeper must speak
//! the protocol of the running one.

mod gate;
mod keeper_pid;
mod running_release;

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use clap::Args;
use roost_host::{EnvSource, HostPlatform, ProcessEnv, ROOST_PROGRAM_FILE};
use tracing::{error, info, warn};

use crate::command_error::CommandFailure;
use crate::deploy::apply;
use crate::deploy::apply_release::{process_environment, read_installed};
use crate::deploy::identity_env::worker_install_environment;
use crate::deploy::machine_txn::{self, TransactionKind};
use crate::deploy::manifest::{ApplyManifest, ApplyOutcome, ApplyReport, REPORT_PREFIX};
use crate::deploy::release::{StagedRelease, read_keeper_contract};
use crate::deploy::release_fetch::fetch_release;
use crate::services::deploy_journal::DeployJournal;
use crate::services::service_control::PlatformServiceManager;
use crate::services::service_spec::ServiceRole;
use crate::status::service_definition::parse_installed_environment;

#[derive(Debug, Args)]
#[command(about = "Move this worker onto a tagged release")]
pub struct SelfUpdateArgs {
    /// The release tag to move to, such as `v3.0.0-rc.37`.
    #[arg(long)]
    pub to: String,
}

/// Fetch, verify and activate `args.to`, then print the apply report.
pub async fn run(args: &SelfUpdateArgs) -> Result<ExitCode, CommandFailure> {
    let platform = roost_host::supported_host_platform()?;
    gate::check_tag(&args.to).map_err(CommandFailure::usage)?;
    let identity = roost_host::build_identity(&ProcessEnv::new());
    if !identity.is_compiled {
        return Err(CommandFailure::usage(
            "this roost is a source checkout, not a release, so it cannot update itself",
        ));
    }
    if identity.artifact_version == args.to {
        println!("RoostSelfUpdate=current {}", args.to);
        return Ok(ExitCode::SUCCESS);
    }

    let env = process_environment();
    let definition_path = ServiceRole::Worker
        .definition_path(env, platform)
        .map_err(|error| CommandFailure::generic(error.to_string()))?;
    let installed = read_installed(&definition_path)
        .map(|definition| parse_installed_environment(&definition, platform))
        .unwrap_or_default();
    let pid_file =
        keeper_pid::pid_file(env, &installed, platform).map_err(CommandFailure::generic)?;
    let staged = fetch_release(env, &args.to, platform, std::env::consts::ARCH).await?;
    let staging = staged
        .local_dir
        .parent()
        .map_or_else(|| staged.local_dir.clone(), Path::to_path_buf);
    info!(tag = %args.to, staging = %staging.display(), "release fetched; installing");

    let keeper_before = keeper_pid::read(&pid_file);
    let report = install(env, platform, &args.to, &staged, &staging).await;
    if let Err(error) = std::fs::remove_dir_all(&staging) {
        warn!(path = %staging.display(), error = %error, "could not remove the release staging directory");
    }
    let report = report?;
    println!(
        "{REPORT_PREFIX}{}",
        report.encode().map_err(CommandFailure::generic)?
    );
    if !matches!(report.outcome, ApplyOutcome::Settled) {
        return Err(CommandFailure::generic(report.detail));
    }
    let keeper_after = keeper_pid::read(&pid_file);
    if let Err(detail) = keeper_pid::keeper_survived(&keeper_before, &keeper_after) {
        error!(%detail, "self-update left the keeper in a different state");
        return Err(CommandFailure::generic(detail));
    }
    let label = ServiceRole::Worker
        .service_label(env, platform)
        .map_err(|error| CommandFailure::generic(error.to_string()))?;
    if let Err(detail) = running_release::check(&label, platform, &args.to) {
        error!(%detail, "self-update settled but the worker is not running the release");
        return Err(CommandFailure::generic(detail));
    }
    Ok(ExitCode::SUCCESS)
}

/// Verify the staged release, then install and activate it under this machine's
/// transaction. Refusals before the apply return `Err`; the apply's own outcome,
/// including a rollback, comes back as `Ok`.
async fn install(
    env: &dyn EnvSource,
    platform: HostPlatform,
    tag: &str,
    staged: &StagedRelease,
    staging: &Path,
) -> Result<ApplyReport, CommandFailure> {
    let service_dir = roost_host::roost_service_dir(env, platform)
        .map_err(|error| CommandFailure::generic(error.to_string()))?;
    let definition_path = ServiceRole::Worker
        .definition_path(env, platform)
        .map_err(|error| CommandFailure::generic(error.to_string()))?;
    let (version, commit) = gate::staged_identity(&staged.local_dir.join(ROOST_PROGRAM_FILE))?;
    gate::check_identity(tag, &version, &commit).map_err(CommandFailure::generic)?;
    let running_program = std::env::current_exe().map_err(|error| {
        CommandFailure::generic(format!("cannot locate this roost binary: {error}"))
    })?;
    let running = read_keeper_contract(&running_program)?;
    gate::check_keeper(&staged.keeper_contract, &running).map_err(CommandFailure::generic)?;

    let journal = DeployJournal::path_in(&service_dir);
    let transaction = machine_txn::MachineTransaction::acquire(
        &machine_txn::lock_path(&service_dir),
        TransactionKind::Deploy,
        &journal,
        crate::wall_clock::now_ms(),
    )
    .await
    .map_err(|error| {
        CommandFailure::generic(format!("cannot take this machine's transaction: {error}"))
    })?;
    let applied = apply_held(&Held {
        env,
        platform,
        tag,
        commit: &commit,
        digest: &staged.digest,
        staging,
        definition_path: &definition_path,
    })
    .await;
    if let Err(error) = transaction.release().await {
        warn!(error = %error, "could not release this machine's transaction");
    }
    applied
}

/// What the apply needs, gathered once so the call stays readable.
struct Held<'a> {
    env: &'a dyn EnvSource,
    platform: HostPlatform,
    tag: &'a str,
    commit: &'a str,
    digest: &'a str,
    staging: &'a Path,
    definition_path: &'a Path,
}

/// Build the apply manifest from the installed definition and run the apply the
/// deploy path runs. The release directory is named by its tag (`versions/<tag>/`);
/// the commit travels in the environment as `GIT_SHA`, so status and the definition
/// show a commit rather than a tag.
async fn apply_held(held: &Held<'_>) -> Result<ApplyReport, CommandFailure> {
    let definition = read_installed(held.definition_path).ok_or_else(|| {
        CommandFailure::generic(format!(
            "no worker is installed here ({}); install it with `roost deploy` first",
            held.definition_path.display()
        ))
    })?;
    let installed = parse_installed_environment(&definition, held.platform);
    let environment =
        worker_install_environment(&installed, &BTreeMap::new(), held.commit, &BTreeMap::new());
    let manifest = ApplyManifest::new(
        held.tag,
        &held.staging.to_string_lossy(),
        held.digest,
        environment,
    );
    let bytes = manifest.encode().map_err(CommandFailure::generic)?;
    let mut manager = PlatformServiceManager::new(held.platform);
    Ok(apply::run_with(&bytes, held.env, &mut manager).await)
}
