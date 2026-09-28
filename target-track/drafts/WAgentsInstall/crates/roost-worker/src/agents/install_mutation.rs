//! The reversible filesystem mutations one agent-integration commit applies —
//! an asset hard-linked into place with any prior owned file moved aside, and a
//! retired owned file moved aside — and the rollback that restores them. Ports
//! the mutation and rollback half of v2
//! `apps/worker/src/agents/integration-install-transaction.ts`; driven by
//! `agents::install_transaction::commit_integration_install`.

use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use roost_platform::HostPlatform;

use crate::agents::install_proof::{
    IntegrationFileSnapshot, IntegrationInstallError, has_integration_ownership,
    inspect_integration_target, integration_lstat_if_present, same_integration_identity,
};
use crate::agents::install_stage::{PreparedDirectory, StagedAsset, sync_directory};
use crate::agents::install_transaction::{IntegrationAssetInstallPlan, IntegrationRetirementPlan};

/// One asset install. `old_moved` and `installed` record how far it got, so a
/// rollback undoes exactly the steps that happened.
#[derive(Debug)]
pub(crate) struct AssetMutation<'m> {
    directory: &'m PreparedDirectory<'m>,
    target: PathBuf,
    staged: StagedAsset,
    backup: Option<PathBuf>,
    old_moved: bool,
    installed: bool,
}

/// One retired file moved out of the loader directory.
#[derive(Debug)]
pub(crate) struct RetirementMutation<'m> {
    directory: &'m PreparedDirectory<'m>,
    target: PathBuf,
    backup: PathBuf,
    old_moved: bool,
}

#[derive(Debug)]
pub(crate) enum InstallMutation<'m> {
    Asset(AssetMutation<'m>),
    Retirement(RetirementMutation<'m>),
}

impl<'m> AssetMutation<'m> {
    /// The mutation targets the CANONICAL directory the commit proved, never
    /// the configured path, so a loader symlink swapped after the proof is not
    /// followed.
    pub(crate) fn new(
        directory: &'m PreparedDirectory<'m>,
        plan: &IntegrationAssetInstallPlan,
        staged: StagedAsset,
    ) -> Self {
        Self {
            directory,
            target: in_canonical_directory(directory, &plan.target),
            staged,
            backup: plan.existing.as_ref().map(|_| {
                directory
                    .stage_path
                    .join(format!("backup-{}", plan.id.as_str()))
            }),
            old_moved: false,
            installed: false,
        }
    }

    pub(crate) fn apply(
        &mut self,
        plan: &IntegrationAssetInstallPlan,
    ) -> Result<(), IntegrationInstallError> {
        let changed = || {
            IntegrationInstallError::refused(format!(
                "agent integration target changed during installation: {}",
                plan.target.display()
            ))
        };
        if let Some(backup) = &self.backup {
            fs::rename(&self.target, backup)
                .map_err(IntegrationInstallError::io("rename", &self.target))?;
            self.old_moved = true;
            let moved = inspect_integration_target(backup, "agent integration backup")?;
            if !is_owned_original(
                moved.as_ref(),
                plan.existing.as_ref(),
                plan.ownership_marker,
            ) {
                return Err(changed());
            }
        }
        if let Err(error) = fs::hard_link(&self.staged.path, &self.target) {
            if error.kind() == io::ErrorKind::AlreadyExists {
                return Err(IntegrationInstallError::refused(format!(
                    "agent integration target appeared during installation: {}",
                    plan.target.display()
                )));
            }
            return Err(IntegrationInstallError::io("link", &self.target)(error));
        }
        self.installed = true;
        let installed = fs::symlink_metadata(&self.target)
            .map_err(IntegrationInstallError::io("lstat", &self.target))?;
        if !same_integration_identity(&installed, self.staged.device, self.staged.inode) {
            return Err(changed());
        }
        tracing::info!(
            asset = plan.id.as_str(),
            path = %self.target.display(),
            replaced = self.old_moved,
            "agent integration installed"
        );
        Ok(())
    }
}

impl<'m> RetirementMutation<'m> {
    pub(crate) fn new(
        directory: &'m PreparedDirectory<'m>,
        plan: &IntegrationRetirementPlan,
    ) -> Self {
        let name = plan.target.file_name().unwrap_or_default();
        let mut backup_name = std::ffi::OsString::from("retired-");
        backup_name.push(name);
        Self {
            directory,
            target: in_canonical_directory(directory, &plan.target),
            backup: directory.stage_path.join(backup_name),
            old_moved: false,
        }
    }

    pub(crate) fn apply(
        &mut self,
        plan: &IntegrationRetirementPlan,
    ) -> Result<(), IntegrationInstallError> {
        fs::rename(&self.target, &self.backup)
            .map_err(IntegrationInstallError::io("rename", &self.target))?;
        self.old_moved = true;
        let moved = inspect_integration_target(&self.backup, "retired agent integration backup")?;
        if !is_owned_original(
            moved.as_ref(),
            plan.existing.as_ref(),
            plan.ownership_marker,
        ) {
            return Err(IntegrationInstallError::refused(format!(
                "retired agent integration changed during installation: {}",
                plan.target.display()
            )));
        }
        tracing::info!(path = %self.target.display(), "retired agent integration removed");
        Ok(())
    }
}

/// Undo `mutations` newest first. Returns whether every one was fully undone;
/// a step that finds a file it did not put there leaves it to its owner and
/// reports the rollback incomplete, so the caller keeps the stage directories
/// that still hold the moved-aside originals.
pub(crate) fn rollback_mutations(
    mutations: &[InstallMutation<'_>],
    platform: HostPlatform,
) -> bool {
    let mut complete = true;
    for mutation in mutations.iter().rev() {
        match mutation.roll_back(platform) {
            Ok(true) => {
                tracing::info!(path = %mutation.target().display(), "agent integration mutation rolled back");
            }
            Ok(false) => {
                tracing::warn!(
                    path = %mutation.target().display(),
                    "agent integration rollback found a file it did not install and left it in place"
                );
                complete = false;
            }
            Err(error) => {
                tracing::warn!(
                    path = %mutation.target().display(),
                    error = %error,
                    "agent integration rollback step failed"
                );
                complete = false;
            }
        }
    }
    complete
}

impl InstallMutation<'_> {
    fn target(&self) -> &Path {
        match self {
            Self::Asset(mutation) => &mutation.target,
            Self::Retirement(mutation) => &mutation.target,
        }
    }

    fn roll_back(&self, platform: HostPlatform) -> Result<bool, IntegrationInstallError> {
        let (directory, backup, old_moved) = match self {
            Self::Asset(mutation) => (
                mutation.directory,
                mutation.backup.as_deref(),
                mutation.old_moved,
            ),
            Self::Retirement(mutation) => (
                mutation.directory,
                Some(mutation.backup.as_path()),
                mutation.old_moved,
            ),
        };
        let target = self.target();
        directory.assert_stable(platform)?;
        let mut restored = true;
        if let Self::Asset(mutation) = self
            && mutation.installed
        {
            match integration_lstat_if_present(target)? {
                Some(installed)
                    if same_integration_identity(
                        &installed,
                        mutation.staged.device,
                        mutation.staged.inode,
                    ) =>
                {
                    durable_remove(target)?;
                }
                Some(_) => restored = false,
                None => {}
            }
        }
        if old_moved && let Some(backup) = backup {
            match integration_lstat_if_present(target)? {
                None => fs::hard_link(backup, target)
                    .map_err(IntegrationInstallError::io("link", target))?,
                Some(current) => {
                    let original = fs::symlink_metadata(backup)
                        .map_err(IntegrationInstallError::io("lstat", backup))?;
                    if !same_integration_identity(&current, original.dev(), original.ino()) {
                        restored = false;
                    }
                }
            }
        }
        Ok(restored)
    }
}

/// The moved-aside file is the one planning read, and it is still Roost's.
fn is_owned_original(
    moved: Option<&IntegrationFileSnapshot>,
    planned: Option<&IntegrationFileSnapshot>,
    ownership_marker: &str,
) -> bool {
    moved == planned
        && moved.is_some_and(|moved| has_integration_ownership(&moved.content, ownership_marker))
}

fn in_canonical_directory(directory: &PreparedDirectory<'_>, target: &Path) -> PathBuf {
    directory
        .snapshot
        .canonical_path
        .join(target.file_name().unwrap_or_default())
}

fn durable_remove(path: &Path) -> Result<(), IntegrationInstallError> {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(IntegrationInstallError::io("unlink", path)(error)),
    }
    match path.parent() {
        Some(parent) => sync_directory(parent),
        None => Ok(()),
    }
}
