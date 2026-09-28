//! The per-runtime staging directory an integration install writes into
//! before anything reaches a loader directory, and the cleanup that never
//! follows a directory that moved. Ports the prepare/stage/cleanup half of v2
//! `apps/worker/src/agents/integration-install-transaction.ts` (with the
//! durable write of `@roost/host/durability`); called by
//! [`super::install_transaction`] and [`super::install_mutation`].

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use roost_platform::HostPlatform;

use super::install_proof::{
    IntegrationDirectoryPlan, IntegrationDirectorySnapshot, assert_integration_directory_snapshot,
    inspect_integration_directory, integration_lstat_if_present, integration_path_comparison_key,
    refusal, same_integration_identity,
};
use super::integration_assets::ByRuntime;

const STAGE_PREFIX: &str = ".roost-integration-stage-";
const LOADER_DIRECTORY_MODE: u32 = 0o700;
const ASSET_FILE_MODE: u32 = 0o600;

/// A loader directory proven stable, with the private stage inside it.
#[derive(Debug)]
pub(super) struct PreparedDirectory {
    pub plan: IntegrationDirectoryPlan,
    pub snapshot: IntegrationDirectorySnapshot,
    pub created: bool,
    pub stage_path: PathBuf,
    stage_device: u64,
    stage_inode: u64,
}

/// A staged asset file and the inode the install will hard-link into place.
#[derive(Debug, Clone)]
pub(super) struct StagedFile {
    pub path: PathBuf,
    pub device: u64,
    pub inode: u64,
}

/// Create the loader directory when planning found none, or prove it is the
/// one planning saw, then open a fresh stage inside its canonical path.
pub(super) fn prepare_directory(
    plan: &IntegrationDirectoryPlan,
    platform: HostPlatform,
) -> io::Result<PreparedDirectory> {
    let before_installation = || {
        refusal(format!(
            "agent integration loader changed before installation: {}",
            plan.path.display()
        ))
    };
    let created = match &plan.initial_snapshot {
        None => {
            if integration_lstat_if_present(&plan.path)?.is_some() {
                return Err(before_installation());
            }
            DirBuilder::new()
                .recursive(true)
                .mode(LOADER_DIRECTORY_MODE)
                .create(&plan.path)?;
            tracing::info!(path = %plan.path.display(), "agent integration loader directory created");
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
        return Err(before_installation());
    }
    let stage_path = create_stage_directory(&snapshot.canonical_path)?;
    let stage = fs::symlink_metadata(&stage_path)?;
    Ok(PreparedDirectory {
        plan: plan.clone(),
        snapshot,
        created,
        stage_path,
        stage_device: stage.dev(),
        stage_inode: stage.ino(),
    })
}

pub(super) fn assert_prepared_directory_stable(
    prepared: &PreparedDirectory,
    platform: HostPlatform,
) -> io::Result<()> {
    assert_integration_directory_snapshot(&prepared.plan, &prepared.snapshot, platform)
}

pub(super) fn assert_prepared_directories_distinct(
    prepared: &ByRuntime<PreparedDirectory>,
    platform: HostPlatform,
) -> io::Result<()> {
    if integration_path_comparison_key(&prepared.omp.snapshot.canonical_path, platform)
        == integration_path_comparison_key(&prepared.pi.snapshot.canonical_path, platform)
    {
        return Err(refusal(
            "refusing colliding OMP and Pi integration directories; configure distinct roots"
                .to_owned(),
        ));
    }
    Ok(())
}

/// Write one asset into the stage, owner-only and flushed with its directory
/// entry. The stage is fresh and private, so the name cannot already exist.
pub(super) fn stage_file(
    directory: &PreparedDirectory,
    name: &str,
    content: &str,
) -> io::Result<StagedFile> {
    let path = directory.stage_path.join(name);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(ASSET_FILE_MODE)
        .open(&path)?;
    file.write_all(content.as_bytes())?;
    // The creation mode is filtered by the umask; the installed file is
    // owner-only whatever the umask is.
    file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(
        ASSET_FILE_MODE,
    ))?;
    file.sync_all()?;
    sync_directory(&directory.stage_path)?;
    let metadata = fs::symlink_metadata(&path)?;
    Ok(StagedFile {
        path,
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

/// Remove a file and flush the directory that held it.
pub(super) fn durable_remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    match path.parent() {
        Some(parent) => sync_directory(parent),
        None => Ok(()),
    }
}

/// Remove each stage, but only while its parent and the stage itself are the
/// inodes this install created: a changed directory is never followed.
pub(super) fn cleanup_stages(prepared: &[&PreparedDirectory]) {
    for directory in prepared {
        let (Ok(parent), Ok(stage)) = (
            fs::symlink_metadata(&directory.snapshot.canonical_path),
            fs::symlink_metadata(&directory.stage_path),
        ) else {
            continue;
        };
        if same_integration_identity(
            &parent,
            directory.snapshot.directory_device,
            directory.snapshot.directory_inode,
        ) && stage.is_dir()
            && same_integration_identity(&stage, directory.stage_device, directory.stage_inode)
            && let Err(error) = fs::remove_dir_all(&directory.stage_path)
        {
            tracing::warn!(stage = %directory.stage_path.display(), %error, "agent integration stage left behind");
        }
    }
}

/// Remove a loader directory this install created, if it is still the same
/// directory and empty. Non-empty or raced directories belong to their owner.
pub(super) fn cleanup_created_directories(prepared: &[&PreparedDirectory], platform: HostPlatform) {
    for directory in prepared.iter().rev() {
        if !directory.created || assert_prepared_directory_stable(directory, platform).is_err() {
            continue;
        }
        if fs::remove_dir(&directory.snapshot.canonical_path).is_ok() {
            tracing::info!(path = %directory.plan.path.display(), "agent integration loader directory removed after a failed install");
        }
    }
}

/// `mkdtemp`: a new owner-only directory whose name no one else holds.
fn create_stage_directory(parent: &Path) -> io::Result<PathBuf> {
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or(0);
    let mut attempt: u32 = 0;
    loop {
        let suffix = format!("{:x}{:x}", std::process::id(), seed.wrapping_add(attempt));
        let path = parent.join(format!("{STAGE_PREFIX}{suffix}"));
        match DirBuilder::new().mode(LOADER_DIRECTORY_MODE).create(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists && attempt < 64 => {
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}
