//! One runtime's loader directory through a commit: proven or created, given a
//! private stage directory inside its canonical path, holding the staged assets,
//! and cleaned up afterwards without following a directory that moved. Ports the
//! directory half of v2 `apps/worker/src/agents/integration-install-transaction.ts`
//! (`prepareDirectory`, `durableWriteFile` staging, `cleanupStages`,
//! `cleanupCreatedDirectories`); driven by `agents::install_transaction`.

use std::fs::{self, DirBuilder, OpenOptions};
use std::hash::{BuildHasher, Hasher, RandomState};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use roost_platform::HostPlatform;

use crate::agents::install_proof::{
    IntegrationDirectoryPlan, IntegrationDirectorySnapshot, IntegrationInstallError,
    assert_integration_directory_snapshot, inspect_integration_directory,
    integration_lstat_if_present, integration_path_comparison_key, same_integration_identity,
};
use crate::agents::install_transaction::IntegrationAssetInstallPlan;
use crate::agents::integration_assets::AgentIntegrationRuntime;

const PRIVATE_FILE_MODE: u32 = 0o600;
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const STAGE_PREFIX: &str = ".roost-integration-stage-";
/// Each attempt either wins a fresh random name or finds something there.
const STAGE_ATTEMPTS: u32 = 64;

/// An asset written into its runtime's stage directory, and the identity the
/// installed hard link must still have.
#[derive(Debug)]
pub(crate) struct StagedAsset {
    pub(crate) path: PathBuf,
    pub(crate) device: u64,
    pub(crate) inode: u64,
}

/// A loader directory proven, created if it was absent, and given a private
/// stage directory inside its canonical path.
#[derive(Debug)]
pub(crate) struct PreparedDirectory<'p> {
    pub(crate) runtime: AgentIntegrationRuntime,
    pub(crate) plan: &'p IntegrationDirectoryPlan,
    pub(crate) snapshot: IntegrationDirectorySnapshot,
    pub(crate) created: bool,
    pub(crate) stage_path: PathBuf,
    pub(crate) stage_device: u64,
    pub(crate) stage_inode: u64,
}

impl PreparedDirectory<'_> {
    pub(crate) fn assert_stable(
        &self,
        platform: HostPlatform,
    ) -> Result<(), IntegrationInstallError> {
        assert_integration_directory_snapshot(self.plan, &self.snapshot, platform)
    }
}

pub(crate) fn prepare_directory(
    runtime: AgentIntegrationRuntime,
    plan: &IntegrationDirectoryPlan,
    platform: HostPlatform,
) -> Result<PreparedDirectory<'_>, IntegrationInstallError> {
    let changed = || {
        IntegrationInstallError::refused(format!(
            "agent integration loader changed before installation: {}",
            plan.path.display()
        ))
    };
    let created = match &plan.initial_snapshot {
        None => {
            if integration_lstat_if_present(&plan.path)?.is_some() {
                return Err(changed());
            }
            DirBuilder::new()
                .recursive(true)
                .mode(PRIVATE_DIRECTORY_MODE)
                .create(&plan.path)
                .map_err(IntegrationInstallError::io("mkdir", &plan.path))?;
            tracing::info!(
                runtime = runtime.as_str(),
                path = %plan.path.display(),
                "agent integration loader directory created"
            );
            true
        }
        Some(initial) => {
            assert_integration_directory_snapshot(plan, initial, platform)?;
            false
        }
    };
    let snapshot = inspect_integration_directory(&plan.path)?;
    if (plan.initial_snapshot.is_none() && snapshot.entry_is_symlink)
        || integration_path_comparison_key(&snapshot.canonical_path, platform)
            != integration_path_comparison_key(&plan.canonical_path, platform)
    {
        return Err(changed());
    }
    let stage_path = create_stage_directory(&snapshot.canonical_path)?;
    let stage = fs::symlink_metadata(&stage_path)
        .map_err(IntegrationInstallError::io("lstat", &stage_path))?;
    Ok(PreparedDirectory {
        runtime,
        plan,
        snapshot,
        created,
        stage_path,
        stage_device: stage.dev(),
        stage_inode: stage.ino(),
    })
}

/// A fresh private directory, created exclusively, so nothing another process
/// placed under a guessed name is ever written through.
fn create_stage_directory(parent: &Path) -> Result<PathBuf, IntegrationInstallError> {
    let names = RandomState::new();
    for attempt in 0..STAGE_ATTEMPTS {
        let mut hasher = names.build_hasher();
        hasher.write_u32(attempt);
        let candidate = parent.join(format!("{STAGE_PREFIX}{:016x}", hasher.finish()));
        match DirBuilder::new()
            .mode(PRIVATE_DIRECTORY_MODE)
            .create(&candidate)
        {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(IntegrationInstallError::io("mkdir", &candidate)(error)),
        }
    }
    Err(IntegrationInstallError::refused(format!(
        "no agent integration stage name was free in {}",
        parent.display()
    )))
}

/// Write one asset, private and flushed, into its runtime's stage directory.
pub(crate) fn stage_asset(
    directory: &PreparedDirectory<'_>,
    asset: &IntegrationAssetInstallPlan,
) -> Result<StagedAsset, IntegrationInstallError> {
    let path = directory
        .stage_path
        .join(format!("asset-{}", asset.id.as_str()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(PRIVATE_FILE_MODE)
        .open(&path)
        .map_err(IntegrationInstallError::io("create", &path))?;
    file.write_all(asset.content.as_bytes())
        .and_then(|()| file.set_permissions(fs::Permissions::from_mode(PRIVATE_FILE_MODE)))
        .and_then(|()| file.sync_all())
        .map_err(IntegrationInstallError::io("write", &path))?;
    drop(file);
    sync_directory(&directory.stage_path)?;
    let metadata =
        fs::symlink_metadata(&path).map_err(IntegrationInstallError::io("lstat", &path))?;
    Ok(StagedAsset {
        path,
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

/// Remove each stage directory — but only while its canonical parent and the
/// stage itself are still the ones this commit created. A changed canonical
/// directory is never followed during cleanup.
pub(crate) fn cleanup_stages(prepared: &[PreparedDirectory<'_>]) {
    for directory in prepared {
        let (Ok(parent), Ok(stage)) = (
            fs::symlink_metadata(&directory.snapshot.canonical_path),
            fs::symlink_metadata(&directory.stage_path),
        ) else {
            continue;
        };
        let unmoved = same_integration_identity(
            &parent,
            directory.snapshot.directory_device,
            directory.snapshot.directory_inode,
        ) && stage.is_dir()
            && !stage.file_type().is_symlink()
            && same_integration_identity(&stage, directory.stage_device, directory.stage_inode);
        if !unmoved {
            tracing::warn!(
                stage = %directory.stage_path.display(),
                "agent integration stage directory moved under the installer; left in place"
            );
            continue;
        }
        if let Err(error) = fs::remove_dir_all(&directory.stage_path) {
            tracing::warn!(
                stage = %directory.stage_path.display(),
                error = %error,
                "agent integration stage directory could not be removed"
            );
        }
    }
}

/// Remove, newest first, the loader directories this commit created. A
/// non-empty or raced directory belongs to its current owner and stays.
pub(crate) fn cleanup_created_directories(
    prepared: &[PreparedDirectory<'_>],
    platform: HostPlatform,
) {
    for directory in prepared.iter().rev().filter(|directory| directory.created) {
        if directory.assert_stable(platform).is_ok()
            && fs::remove_dir(&directory.snapshot.canonical_path).is_ok()
        {
            tracing::info!(
                runtime = directory.runtime.as_str(),
                path = %directory.snapshot.canonical_path.display(),
                "agent integration loader directory removed after a failed install"
            );
        }
    }
}

/// Flush a directory so a create, rename or unlink in it survives a power cut.
pub(crate) fn sync_directory(path: &Path) -> Result<(), IntegrationInstallError> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(IntegrationInstallError::io("fsync", path))
}
