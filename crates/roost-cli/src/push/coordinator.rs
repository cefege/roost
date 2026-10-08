//! The local POSIX coordinator's half of a fleet push: stage the release, take a
//! rollback point, and leave the coordinator running the target commit with the
//! fleet journal still held. Called by the push runtime; depends on the services
//! group's install and deploy transaction, which own the machine journal and the
//! coordinator's own rollback, and on nothing else in the push group.
//!
//! "Local" is the whole point. The deploying box IS the coordinator's host in
//! the supported arrangement, so the coordinator leg is the only one in this
//! command that does not go over ssh — and that is why it is a separate module
//! from the participant leg rather than a special case of it. It ends by
//! restarting the coordinator, because a push whose coordinator was left
//! installed-but-not-restarted has left the fleet's answer behind.

use std::path::{Path, PathBuf};

use roost_host::build_identity::{GIT_SHA_ENV, ROOST_GIT_SHA_ENV};
use roost_host::{EnvSource, HostPlatform, roost_service_dir, roost_versions_dir};
use tracing::{info, warn};

use crate::command_error::CommandFailure;
use crate::deploy::apply_release::RELEASE_BIN_DIR;
use crate::deploy::release;
use crate::deploy::retire;
use crate::services::deploy_transaction::{self, DeployError};
use crate::services::install;
use crate::services::service_argv::ServiceAction;
use crate::services::service_control::{PlatformServiceManager, ServiceManager};
use crate::services::service_spec::{ServiceRole, ServiceSpec, ServiceTarget};
use crate::status::service_definition::{InstalledEnvironment, parse_installed_environment};
use roost_host::ROOST_PROGRAM_FILE;

/// Everything the coordinator leg needs to know about this machine, resolved
/// once so a decision cannot be made against one path and acted on another.
#[derive(Debug, Clone)]
pub struct CoordinatorLocation {
    pub platform: HostPlatform,
    /// The service directory the machine journal, the machine transaction and
    /// the fleet journal all live in.
    pub service_dir: PathBuf,
    /// The root releases install under; a release is one child of it.
    pub release_root: PathBuf,
    /// The installed service definition, read for the commit it is running.
    pub definition_path: PathBuf,
    pub label: String,
}

/// Resolve the coordinator's location, and prove an install is there to replace.
pub fn locate(
    env: &dyn EnvSource,
    platform: HostPlatform,
) -> Result<CoordinatorLocation, CommandFailure> {
    let label = roost_host::coord_service_label(env, platform)?;
    let definition_path = roost_host::coord_service_path(env, platform)?;
    if !definition_path.is_file() {
        return Err(CommandFailure::generic(format!(
            "no coordinator is installed at {}: `roost push` rolls a fleet, and there is no \
             coordinator on this machine to roll",
            definition_path.display()
        )));
    }
    Ok(CoordinatorLocation {
        platform,
        service_dir: roost_service_dir(env, platform)?,
        release_root: roost_versions_dir(env, platform)?,
        definition_path,
        label,
    })
}

/// The environment the installed coordinator definition carries.
pub fn installed_environment(
    location: &CoordinatorLocation,
) -> Result<InstalledEnvironment, CommandFailure> {
    let text = std::fs::read_to_string(&location.definition_path).map_err(|error| {
        CommandFailure::generic(format!(
            "cannot read the installed coordinator definition {}: {error}",
            location.definition_path.display()
        ))
    })?;
    Ok(parse_installed_environment(&text, location.platform))
}

/// The commit the installed coordinator is running, and the release directory it
/// is running from. A definition that proves neither cannot be rolled back to,
/// so it is refused before anything is staged.
pub fn installed_release(
    installed: &InstalledEnvironment,
    location: &CoordinatorLocation,
) -> Result<(String, PathBuf), CommandFailure> {
    let sha = crate::status::service_definition::declared_value(installed, GIT_SHA_ENV)
        .or_else(|| crate::status::service_definition::declared_value(installed, ROOST_GIT_SHA_ENV))
        .ok_or_else(|| {
            CommandFailure::generic(format!(
                "the installed coordinator definition at {} does not name the commit it is \
                 running; there is no prior release to roll back to",
                location.definition_path.display()
            ))
        })?
        .to_ascii_lowercase();
    let release = location.release_root.join(&sha);
    Ok((sha, release))
}

/// The release directory for a commit on this machine.
pub fn release_bin_dir(location: &CoordinatorLocation, git_sha: &str) -> PathBuf {
    location.release_root.join(git_sha).join(RELEASE_BIN_DIR)
}

/// The program a service definition runs for a commit on this machine.
pub fn release_program(location: &CoordinatorLocation, git_sha: &str) -> PathBuf {
    release_bin_dir(location, git_sha).join(ROOST_PROGRAM_FILE)
}

/// Install a built release into this machine's release root under `git_sha`, and
/// return the directory the service definition will run from.
///
/// The keeper is installed beside `roost` because the keeper contract this
/// release ships is computed from that sibling's bytes. A release installed
/// without it describes a keeper this build cannot start.
pub fn install_staged_release(
    location: &CoordinatorLocation,
    staged: &release::StagedRelease,
    git_sha: &str,
) -> Result<PathBuf, CommandFailure> {
    let bin_dir = release_bin_dir(location, git_sha);
    let roost_source = staged.local_dir.join(ROOST_PROGRAM_FILE);
    let keeper_source = staged.local_dir.join(release::RELEASE_PROGRAMS[1]);
    install::install_release_programs(&roost_source, Some(&keeper_source), &bin_dir)
        .map_err(|error| CommandFailure::generic(error.to_string()))?;
    Ok(bin_dir)
}

/// The service definition for the coordinator running `git_sha` from `program`.
///
/// The two build stamps are set explicitly rather than taken from this
/// process: the coordinator's readout and every worker's heartbeat compare those
/// values, and a definition that stamped the CLI's own build would report a
/// machine as running something it is not.
pub fn coordinator_spec(
    env: &dyn EnvSource,
    location: &CoordinatorLocation,
    git_sha: &str,
    program: &Path,
) -> Result<ServiceSpec, CommandFailure> {
    Ok(
        ServiceSpec::resolve(ServiceRole::Coordinator, env, location.platform, program)?
            .with_setting(GIT_SHA_ENV, git_sha)
            .with_setting(ROOST_GIT_SHA_ENV, git_sha),
    )
}

/// Deploy `spec` as a transaction on this machine, rolling back on its own if
/// the coordinator does not come up.
pub fn deploy_definition(
    location: &CoordinatorLocation,
    spec: &ServiceSpec,
) -> Result<(), CommandFailure> {
    let mut manager = PlatformServiceManager::new(location.platform);
    deploy_transaction::deploy_service_definition(
        spec,
        location.platform,
        &location.service_dir,
        &mut manager,
    )
    .map(|_| ())
    .map_err(transaction_failure)
}

/// Resolve any definition swap a previous run left in flight, before this one
/// writes a journal of its own over the top of it.
pub fn resolve_interrupted(location: &CoordinatorLocation) -> Result<(), CommandFailure> {
    let mut manager = PlatformServiceManager::new(location.platform);
    deploy_transaction::resolve_interrupted_deploy(&location.service_dir, &mut manager)
        .map_err(transaction_failure)
}

/// Restart the coordinator onto the definition just installed, and prove it
/// came up. A push that left the coordinator installed-but-not-running has
/// left the fleet's own answer behind, so this is part of the command rather
/// than a nicety at the end of it.
pub fn kickstart(location: &CoordinatorLocation) -> Result<(), CommandFailure> {
    let target = ServiceTarget {
        label: location.label.clone(),
        definition_path: location.definition_path.clone(),
    };
    let mut manager = PlatformServiceManager::new(location.platform);
    manager
        .apply(&target, ServiceAction::Restart)
        .map_err(|error| CommandFailure::generic(error.to_string()))?;
    if !manager.await_active(&target) {
        return Err(CommandFailure::generic(format!(
            "the coordinator {} did not come up on the release this push installed",
            location.label
        )));
    }
    info!(label = %location.label, "coordinator restarted on the pushed release");
    Ok(())
}

/// Retire the release the settled coordinator replaced, and record the reason it
/// was safe to.
pub fn retire_prior(location: &CoordinatorLocation, prior: &Path) -> Result<(), CommandFailure> {
    if !prior.is_dir() {
        return Ok(());
    }
    let prior_sha = prior
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let retirement = retire::retire_prior_release(&location.release_root, prior)
        .map_err(CommandFailure::generic)?;
    info!(
        release = %prior_sha,
        how = %retirement.display_name(),
        "retired the release the pushed coordinator replaced"
    );
    Ok(())
}

/// Copy the coordinator's database triad beside the transaction, so a rollback
/// restores state and not only files.
///
/// All three files, for the reason `roost reset` deletes all three: a snapshot
/// without the write-ahead log is a database whose next open replays a write the
/// rollback was supposed to undo.
pub fn snapshot_database(
    location: &CoordinatorLocation,
    database: &Path,
    rollout_id: &str,
) -> Result<PathBuf, CommandFailure> {
    let snapshot_dir = location
        .service_dir
        .join(format!("coordinator-rollback-{rollout_id}"));
    std::fs::create_dir_all(&snapshot_dir).map_err(|error| {
        CommandFailure::generic(format!(
            "cannot create the coordinator rollback directory {}: {error}",
            snapshot_dir.display()
        ))
    })?;
    for source in database_triad(database) {
        if !source.exists() {
            continue;
        }
        let name = source
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "coordinator.db".to_string());
        std::fs::copy(&source, snapshot_dir.join(&name)).map_err(|error| {
            CommandFailure::generic(format!("cannot snapshot {}: {error}", source.display()))
        })?;
    }
    Ok(snapshot_dir)
}

/// Put the database triad back from a snapshot.
///
/// A write-ahead log the snapshot did not carry is REMOVED rather than left
/// alone. A log written after the snapshot describes transactions against a
/// database that is no longer there, and the next open would replay them — which
/// is the one outcome a rollback exists to prevent, in the coordinator's own
/// state instead of its files.
pub fn restore_database(database: &Path, snapshot_dir: &Path) -> Result<(), CommandFailure> {
    for (index, target) in database_triad(database).into_iter().enumerate() {
        let name = target
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let source = snapshot_dir.join(&name);
        if !source.exists() {
            if index > 0 {
                remove_quietly(&target);
            }
            continue;
        }
        std::fs::copy(&source, &target).map_err(|error| {
            CommandFailure::generic(format!(
                "cannot restore {} from {}: {error}",
                target.display(),
                source.display()
            ))
        })?;
        info!(path = %target.display(), "restored the coordinator database from its snapshot");
    }
    Ok(())
}

/// Remove the snapshot a settled or rolled-back transaction no longer needs.
///
/// Retained past its transaction it is a stale copy of the coordinator's fleet
/// state sitting in a service directory, and nobody would ever know to delete it.
pub fn discard_snapshot(snapshot_dir: &Path) {
    if let Err(error) = std::fs::remove_dir_all(snapshot_dir)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        warn!(
            path = %snapshot_dir.display(),
            reason = %error,
            "a coordinator rollback snapshot is still on disk"
        );
    }
}

fn remove_quietly(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {
            info!(path = %path.display(), "removed a write-ahead log the snapshot did not carry")
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            warn!(path = %path.display(), reason = %error, "could not remove a stale write-ahead log")
        }
    }
}

/// The database, its write-ahead log and its shared-memory index.
pub fn database_triad(database: &Path) -> [PathBuf; 3] {
    let mut wal = database.as_os_str().to_os_string();
    wal.push("-wal");
    let mut shm = database.as_os_str().to_os_string();
    shm.push("-shm");
    [
        database.to_path_buf(),
        PathBuf::from(wal),
        PathBuf::from(shm),
    ]
}

fn transaction_failure(error: DeployError) -> CommandFailure {
    warn!(reason = %error, "the coordinator deploy transaction did not settle");
    CommandFailure::generic(error.to_string())
}
