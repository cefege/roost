//! The two mutations an integration install commits — put an asset in place,
//! move a retired asset aside — and the rollback that undoes them in reverse
//! without touching a file someone else put there since. Ports the mutation
//! half of v2 `apps/worker/src/agents/integration-install-transaction.ts`;
//! called by [`super::install_transaction`].

use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use roost_platform::HostPlatform;

use super::install_proof::{
    IntegrationFileSnapshot, has_integration_ownership, inspect_integration_target,
    integration_lstat_if_present, refusal, same_integration_file_snapshot,
    same_integration_identity,
};
use super::install_stage::{
    PreparedDirectory, StagedFile, assert_prepared_directory_stable, durable_remove,
};

/// What a committed mutation displaced and installed, so rollback can prove
/// every file it restores or removes is still the one it moved.
#[derive(Debug)]
pub(super) struct InstallMutation<'a> {
    pub directory: &'a PreparedDirectory,
    /// The target inside the canonical loader directory.
    pub target: PathBuf,
    /// Where the displaced file went; `None` for an asset with no predecessor.
    pub backup: Option<PathBuf>,
    /// The staged inode hard-linked into place; `None` for a retirement.
    pub staged: Option<StagedFile>,
    pub old_moved: bool,
    pub installed: bool,
}

impl<'a> InstallMutation<'a> {
    pub fn asset(
        directory: &'a PreparedDirectory,
        target_name: &Path,
        staged: StagedFile,
        backup_name: Option<String>,
    ) -> Self {
        Self {
            directory,
            target: directory.snapshot.canonical_path.join(target_name),
            backup: backup_name.map(|name| directory.stage_path.join(name)),
            staged: Some(staged),
            old_moved: false,
            installed: false,
        }
    }

    pub fn retirement(directory: &'a PreparedDirectory, target_name: &Path) -> Self {
        let mut backup_name = std::ffi::OsString::from("retired-");
        backup_name.push(target_name);
        Self {
            directory,
            target: directory.snapshot.canonical_path.join(target_name),
            backup: Some(directory.stage_path.join(backup_name)),
            staged: None,
            old_moved: false,
            installed: false,
        }
    }
}

/// Move the owned predecessor aside (proving it is the file planning saw),
/// then hard-link the staged inode into the now-free target name.
pub(super) fn apply_asset_mutation(
    mutation: &mut InstallMutation<'_>,
    planned_target: &Path,
    existing: Option<&IntegrationFileSnapshot>,
    ownership_marker: &str,
) -> io::Result<()> {
    let changed = || {
        refusal(format!(
            "agent integration target changed during installation: {}",
            planned_target.display()
        ))
    };
    if let Some(backup) = mutation.backup.clone()
        && !move_aside(
            mutation,
            &backup,
            "agent integration backup",
            existing,
            ownership_marker,
        )?
    {
        return Err(changed());
    }
    let Some(staged) = mutation.staged.clone() else {
        return Err(changed());
    };
    if let Err(error) = fs::hard_link(&staged.path, &mutation.target) {
        if error.kind() == io::ErrorKind::AlreadyExists {
            return Err(refusal(format!(
                "agent integration target appeared during installation: {}",
                planned_target.display()
            )));
        }
        return Err(error);
    }
    mutation.installed = true;
    let installed = fs::symlink_metadata(&mutation.target)?;
    if !same_integration_identity(&installed, staged.device, staged.inode) {
        return Err(changed());
    }
    tracing::info!(target = %mutation.target.display(), "agent integration asset installed");
    Ok(())
}

/// Move an owned retired asset into the stage, where cleanup deletes it.
pub(super) fn apply_retirement_mutation(
    mutation: &mut InstallMutation<'_>,
    planned_target: &Path,
    existing: Option<&IntegrationFileSnapshot>,
    ownership_marker: &str,
) -> io::Result<()> {
    let Some(backup) = mutation.backup.clone() else {
        return Err(refusal(format!(
            "retired agent integration changed during installation: {}",
            planned_target.display()
        )));
    };
    let description = "retired agent integration backup";
    if !move_aside(mutation, &backup, description, existing, ownership_marker)? {
        return Err(refusal(format!(
            "retired agent integration changed during installation: {}",
            planned_target.display()
        )));
    }
    tracing::info!(target = %mutation.target.display(), "retired agent integration removed");
    Ok(())
}

/// Undo committed mutations newest first. `false` when any file could not be
/// proven ours to restore or remove; the stage is then left for inspection.
pub(super) fn rollback_mutations(
    mutations: &[InstallMutation<'_>],
    platform: HostPlatform,
) -> bool {
    let mut complete = true;
    for mutation in mutations.iter().rev() {
        match rollback_one(mutation, platform) {
            Ok(true) => {}
            Ok(false) | Err(_) => complete = false,
        }
    }
    tracing::warn!(
        mutations = mutations.len(),
        complete,
        "agent integration install rolled back"
    );
    complete
}

fn rollback_one(mutation: &InstallMutation<'_>, platform: HostPlatform) -> io::Result<bool> {
    assert_prepared_directory_stable(mutation.directory, platform)?;
    let mut complete = true;
    if mutation.installed
        && let Some(staged) = &mutation.staged
        && let Some(installed) = integration_lstat_if_present(&mutation.target)?
    {
        if same_integration_identity(&installed, staged.device, staged.inode) {
            durable_remove(&mutation.target)?;
        } else {
            complete = false;
        }
    }
    if mutation.old_moved {
        let Some(backup) = &mutation.backup else {
            return Ok(false);
        };
        match integration_lstat_if_present(&mutation.target)? {
            None => fs::hard_link(backup, &mutation.target)?,
            Some(target) => {
                let backup_metadata = fs::symlink_metadata(backup)?;
                if !same_integration_identity(&target, backup_metadata.dev(), backup_metadata.ino())
                {
                    complete = false;
                }
            }
        }
    }
    Ok(complete)
}

/// Rename the target into the stage; `true` when the moved file is the owned
/// file planning inspected.
fn move_aside(
    mutation: &mut InstallMutation<'_>,
    backup: &Path,
    description: &str,
    existing: Option<&IntegrationFileSnapshot>,
    ownership_marker: &str,
) -> io::Result<bool> {
    fs::rename(&mutation.target, backup)?;
    mutation.old_moved = true;
    let moved = inspect_integration_target(backup, description)?;
    let owned = moved
        .as_ref()
        .is_some_and(|moved| has_integration_ownership(&moved.content, ownership_marker));
    Ok(owned && same_integration_file_snapshot(moved.as_ref(), existing))
}
