//! Putting one Roost service definition on THIS machine, under the machine
//! transaction, and proving the service actually came up. Called by
//! `quickstart` and `join`; depends on the services group's own install and
//! deploy transaction and on the deploy group's machine lock, and on nothing
//! else in either group.
//!
//! Every step here is a call into a group that already owns it: the
//! definitions are rendered and installed by `services::install`, the swap with
//! its journal and its rollback is `services::deploy_transaction`, and the
//! serialisation against a concurrent deploy is `deploy::machine_txn`. What
//! this file adds is the lock, because the ssh deploy holds the machine
//! transaction on the far side of a connection and a local install has no
//! connection to hold it over: a coordinator install and a concurrent
//! `roost deploy localhost` would otherwise both compute a rollback point
//! against the same file. What [`deploy_local_definition`] itself does, in
//! order, is:
//!
//! 1. take the machine transaction, and release it whether the swap settled or
//!    not — a transaction left held after a refusal makes the NEXT install
//!    fail for a reason that has nothing to do with it;
//! 2. call `services::deploy_transaction::deploy_service_definition`, which
//!    resolves an unfinished deploy, journals the previous definition, swaps,
//!    proves the service came up and rolls back if it did not.
//!
//! Steps this file offers the CALLER, and which the caller owes in that order
//! because this module does not do them: install the release's programs, and
//! create the directories the service needs. Both are separate functions here
//! rather than folded into the deploy, so a caller can be seen doing them
//! before anything it deploys names them.

use std::path::{Path, PathBuf};

use roost_host::{EnvSource, HostPlatform};
use tracing::info;

use crate::command_error::CommandFailure;
use crate::deploy::codes;
use crate::deploy::machine_txn::{self, MachineTransaction, TransactionKind};
use crate::services::deploy_journal::DeployJournal;
use crate::services::deploy_transaction::{DeployOutcome, deploy_service_definition};
use crate::services::install::{
    InstallOutcome, ensure_service_directories, install_release_programs,
};
use crate::services::logrotate::{RotationOutcome, install_rotation};
use crate::services::service_control::PlatformServiceManager;
use crate::services::service_spec::{ServiceRole, ServiceSpec};
use crate::wall_clock;

/// The programs a local install puts in place, when the command can find them.
///
/// `roost` is this process's own executable: a first-run command installs the
/// build the operator just ran, because asking an operator to have built a
/// release before they may install one is how quickstart never gets run. The
/// keeper is looked for beside it and is optional, so a development build that
/// ships no separate keeper binary still installs a `roost` and says so.
#[derive(Debug)]
pub struct LocalPrograms {
    /// The `roost` this process is running from.
    pub roost: PathBuf,
    /// The `roost-keeper` beside it, when this build ships one.
    pub keeper: Option<PathBuf>,
}

impl LocalPrograms {
    /// This process's own executable and the keeper beside it.
    ///
    /// The keeper is OPTIONAL for a source build, which is the case the field
    /// was optional for: a development tree ships no separate keeper binary and
    /// still installs a working `roost`. It is NOT optional for a release, and
    /// the reason is the caller's behaviour rather than the file's absence:
    /// `roost join` filters a missing keeper out rather than refusing, so a
    /// release that found none would enroll a machine, report success, and
    /// serve no terminal — the "roster looks converged, terminal serves
    /// nothing" outcome. Refusing here is the only place it can be caught.
    pub fn of_this_process(env: &dyn EnvSource) -> Result<Self, CommandFailure> {
        let roost = std::env::current_exe().map_err(|error| {
            CommandFailure::generic(format!(
                "this command cannot locate its own executable to install: {error}"
            ))
        })?;
        let keeper = roost
            .parent()
            .map(|directory| directory.join(crate::deploy::apply_release::KEEPER_PROGRAM))
            .filter(|candidate| candidate.is_file());
        let identity = roost_host::build_identity(env);
        require_keeper_for_release(keeper.as_deref(), &roost, &identity.artifact_version)?;
        Ok(Self { roost, keeper })
    }
}

/// Whether a build with no keeper beside its `roost` may install anyway.
///
/// Split out of [`LocalPrograms::of_this_process`] because
/// `artifact_version` is a COMPILE-TIME constant
/// (`build_identity.rs:42-45`), so a test binary is always `dev` and the
/// release refusal cannot be reached through the constructor at all. The
/// decision is here so the refusal is a thing tests can exercise; the
/// constructor is only the thing that supplies its inputs.
///
/// Optional for a source build, which is the case the field was optional for:
/// a development tree ships no separate keeper binary and still installs a
/// working `roost`. NOT optional for a release, and the reason is the caller's
/// behaviour rather than the file's absence — `roost join` filters a missing
/// keeper out rather than refusing, so a release that found none would enroll a
/// machine, report success, and serve no terminal.
pub fn require_keeper_for_release(
    keeper: Option<&Path>,
    roost: &Path,
    artifact_version: &str,
) -> Result<(), CommandFailure> {
    if keeper.is_some() || artifact_version == roost_host::DEV_BUILD_STAMP {
        return Ok(());
    }
    let directory = roost.parent().unwrap_or_else(|| Path::new("."));
    Err(CommandFailure::generic(format!(
        "no {} beside {}, and this is a {artifact_version} build. A joined machine needs \
         both programs: the worker cannot run a session without its keeper. The release \
         publishes it in the same directory as this binary.",
        crate::deploy::apply_release::KEEPER_PROGRAM,
        directory.display()
    )))
}

/// Deploy one definition on this machine, holding the machine transaction for
/// the whole of it and releasing it whatever happens.
pub async fn deploy_local_definition(
    spec: &ServiceSpec,
    platform: HostPlatform,
    service_dir: &Path,
) -> Result<DeployOutcome, CommandFailure> {
    let transaction = MachineTransaction::acquire(
        &machine_txn::lock_path(service_dir),
        TransactionKind::Deploy,
        &DeployJournal::path_in(service_dir),
        wall_clock::now_ms(),
    )
    .await
    .map_err(|error| {
        codes::refuse(
            codes::REMOTE_LOST,
            format!("this machine cannot be locked for a definition swap: {error}"),
        )
    })?;
    let mut manager = PlatformServiceManager::new(platform);
    let deployed = deploy_service_definition(spec, platform, service_dir, &mut manager)
        .map_err(|error| codes::refuse(codes::SETTLEMENT_FAILED, error.to_string()));
    // The machine is released whether the definition settled or not. A
    // transaction left held after a refusal makes the NEXT install fail for a
    // reason that has nothing to do with it.
    let released = transaction.release().await;
    let outcome = deployed?;
    released.map_err(|error| {
        codes::refuse(
            codes::REMOTE_LOST,
            format!("the machine lock could not be released: {error}"),
        )
    })?;
    Ok(outcome)
}

/// Install the release's programs into `bin_dir`, and say what changed.
///
/// The programs go in before any definition names them: a definition that
/// points at a program the install has not written yet is a definition whose
/// first activation runs a path that does not exist, and the service manager
/// reports that as a service that "failed to start" with no clue why.
pub fn install_programs(
    programs: &LocalPrograms,
    bin_dir: &Path,
) -> Result<Vec<InstallOutcome>, CommandFailure> {
    install_release_programs(&programs.roost, programs.keeper.as_deref(), bin_dir).map_err(
        |error| {
            codes::refuse(
                codes::BUILD_FAILED,
                format!(
                    "the release could not be installed into {}: {error}",
                    bin_dir.display()
                ),
            )
        },
    )
}

/// Create every directory a service needs before its definition is written.
pub fn prepare_service_directories(spec: &ServiceSpec) -> Result<Vec<PathBuf>, CommandFailure> {
    ensure_service_directories(spec).map_err(|error| {
        codes::refuse(
            codes::NO_REMOTE_RUNTIME,
            format!(
                "the directories {} needs could not be created: {error}",
                spec.label
            ),
        )
    })
}

/// One line about what an install changed, for the operator watching it happen.
pub fn report_change(outcome: &DeployOutcome, action: &'static str) {
    info!(
        label = %outcome.label,
        definition = %outcome.definition_path.display(),
        changed = outcome.definition_changed,
        "{action}"
    );
}

/// Install one role's log rotation and say what happened, on stderr.
///
/// Shared by quickstart and join because both are first installs on a machine
/// with nothing of ours on it, and a rotation written by one and not the other
/// is a machine whose logs grow until something else stops them. A skip is
/// reported rather than swallowed: the operator is the only one who can install
/// `logrotate`, and silence here reads as "rotated".
pub fn report_rotation(role: ServiceRole, env: &dyn EnvSource, platform: HostPlatform) {
    let rotation = match install_rotation(env, platform, role) {
        Ok(rotation) => rotation,
        Err(error) => {
            eprintln!(
                "WARN: the {}'s log rotation could not be written: {error}",
                role.display_name()
            );
            return;
        }
    };
    match rotation {
        RotationOutcome::Installed(files) => info!(
            role = %role.display_name(),
            files = files.len(),
            "installed the log rotation"
        ),
        RotationOutcome::Skipped(reason) => {
            eprintln!(
                "WARN: no log rotation for the {}: {reason}",
                role.display_name()
            );
        }
    }
}

/// The service directory this install keeps its release versions and its
/// deploy journal in.
pub fn service_dir(env: &dyn EnvSource, platform: HostPlatform) -> Result<PathBuf, CommandFailure> {
    roost_host::roost_service_dir(env, platform).map_err(Into::into)
}
