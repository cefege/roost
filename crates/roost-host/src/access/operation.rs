//! Private durable receipts for independently supervised access changes.
//!
//! Operation identifiers name records in the coordinator's own state directory;
//! neither the browser nor a helper supplies a filesystem path.

use crate::{EnvSource, HostPlatform};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Debug, thiserror::Error)]
pub enum OperationError {
    #[error("invalid access operation identifier")]
    InvalidId,
    #[error("access operation state is unavailable or insecure")]
    Storage,
    #[error("access operation record is invalid")]
    InvalidRecord,
    #[error("another access operation is active")]
    Busy,
    #[error("access operation could not be scheduled")]
    Schedule,
    #[error(
        "access helper scheduling is uncertain for operation {id}; inspect its status before retrying roost __access-service-apply {id}"
    )]
    ScheduleUncertain { id: String },
    #[error("Tailscale preflight task could not be started or completed")]
    PreflightTask,
    #[error(transparent)]
    Tailscale(#[from] super::tailscale::TailscaleError),
    #[error("Tailscale access requires an IPv4 loopback coordinator listener")]
    LoopbackRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Scheduled,
    Applying,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessOperation {
    pub id: String,
    pub definition_sha256: String,
    pub browser_origin: String,
    pub worker_origin: Option<String>,
    pub retained_frontdoor_origins: Vec<String>,
    pub cors_allowed_origins: Vec<String>,
    pub tailscale_action: Option<super::tailscale::TailscalePlanAction>,
    pub state: OperationState,
    pub detail: Option<String>,
}

fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn directory(env: &dyn EnvSource, platform: HostPlatform) -> Result<PathBuf, OperationError> {
    crate::coord_data_dir(env, platform)
        .map(|path| path.join("access-operations"))
        .map_err(|_| OperationError::Storage)
}

fn verify_directory(path: &Path) -> Result<(), OperationError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| OperationError::Storage)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(OperationError::Storage);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(OperationError::Storage);
        }
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(|_| OperationError::Storage)?;
        let suffix: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        let probe = path.join(format!(".owner-{suffix}"));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&probe)
            .map_err(|_| OperationError::Storage)?;
        let uid = file.metadata().map_err(|_| OperationError::Storage)?.uid();
        fs::remove_file(probe).map_err(|_| OperationError::Storage)?;
        if uid != metadata.uid() {
            return Err(OperationError::Storage);
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        Err(OperationError::Storage)
    }
}

fn read_record(path: &Path) -> Result<AccessOperation, OperationError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| OperationError::Storage)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 16384 {
        return Err(OperationError::InvalidRecord);
    }
    let bytes = fs::read(path).map_err(|_| OperationError::Storage)?;
    let record: AccessOperation =
        serde_json::from_slice(&bytes).map_err(|_| OperationError::InvalidRecord)?;
    if !valid_id(&record.id)
        || record.definition_sha256.len() != 64
        || !record
            .definition_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(OperationError::InvalidRecord);
    }
    Ok(record)
}

fn write_record(directory: &Path, record: &AccessOperation) -> Result<(), OperationError> {
    let bytes = serde_json::to_vec(record).map_err(|_| OperationError::InvalidRecord)?;
    let temporary = directory.join(format!(".{}.{}.tmp", record.id, std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|_| OperationError::Storage)?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| OperationError::Storage)?;
    drop(file);
    fs::rename(&temporary, directory.join(format!("{}.json", record.id)))
        .map_err(|_| OperationError::Storage)?;
    File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(|_| OperationError::Storage)
}

pub fn load_operation(
    env: &dyn EnvSource,
    platform: HostPlatform,
    id: &str,
) -> Result<AccessOperation, OperationError> {
    if !valid_id(id) {
        return Err(OperationError::InvalidId);
    }
    let directory = directory(env, platform)?;
    verify_directory(&directory)?;
    let record = read_record(&directory.join(format!("{id}.json")))?;
    if record.id != id {
        return Err(OperationError::InvalidRecord);
    }
    Ok(record)
}

/// Save the digest-bound original service definition before claiming an operation.
pub fn save_original_definition(
    env: &dyn EnvSource,
    platform: HostPlatform,
    id: &str,
    definition: &[u8],
) -> Result<(), OperationError> {
    let record = load_operation(env, platform, id)?;
    if definition.len() > 1_048_576
        || hex::encode(Sha256::digest(definition)) != record.definition_sha256
    {
        return Err(OperationError::InvalidRecord);
    }
    if record.state != OperationState::Scheduled {
        return Err(OperationError::InvalidRecord);
    }
    if load_original_definition(env, platform, id).is_ok() {
        return Ok(());
    }
    let directory = directory(env, platform)?;
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce).map_err(|_| OperationError::Storage)?;
    let temporary = directory.join(format!(".{id}.{}.definition.tmp", hex::encode(nonce)));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|_| OperationError::Storage)?;
    file.write_all(definition)
        .and_then(|()| file.sync_all())
        .map_err(|_| OperationError::Storage)?;
    drop(file);
    fs::rename(&temporary, directory.join(format!("{id}.definition")))
        .map_err(|_| OperationError::Storage)?;
    File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(|_| OperationError::Storage)
}

/// Read only a private backup whose content still matches the approved digest.
pub fn load_original_definition(
    env: &dyn EnvSource,
    platform: HostPlatform,
    id: &str,
) -> Result<Vec<u8>, OperationError> {
    let record = load_operation(env, platform, id)?;
    let path = directory(env, platform)?.join(format!("{id}.definition"));
    let metadata = fs::symlink_metadata(&path).map_err(|_| OperationError::Storage)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 1_048_576 {
        return Err(OperationError::Storage);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(OperationError::Storage);
        }
    }
    let bytes = fs::read(path).map_err(|_| OperationError::Storage)?;
    if hex::encode(Sha256::digest(&bytes)) != record.definition_sha256 {
        return Err(OperationError::InvalidRecord);
    }
    Ok(bytes)
}

pub fn remove_original_definition(
    env: &dyn EnvSource,
    platform: HostPlatform,
    id: &str,
) -> Result<(), OperationError> {
    load_operation(env, platform, id)?;
    let directory = directory(env, platform)?;
    match fs::remove_file(directory.join(format!("{id}.definition"))) {
        Ok(()) => File::open(directory)
            .and_then(|file| file.sync_all())
            .map_err(|_| OperationError::Storage),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(OperationError::Storage),
    }
}

pub fn set_operation_state(
    env: &dyn EnvSource,
    platform: HostPlatform,
    id: &str,
    state: OperationState,
    detail: Option<&str>,
) -> Result<(), OperationError> {
    let mut record = load_operation(env, platform, id)?;
    record.state = state;
    record.detail = detail.map(str::to_string);
    write_record(&directory(env, platform)?, &record)
}

pub fn latest_operation(
    env: &dyn EnvSource,
    platform: HostPlatform,
) -> Result<Option<AccessOperation>, OperationError> {
    let directory = directory(env, platform)?;
    if !directory.exists() {
        return Ok(None);
    }
    verify_directory(&directory)?;
    let mut latest: Option<(std::time::SystemTime, AccessOperation)> = None;

    for entry in fs::read_dir(&directory).map_err(|_| OperationError::Storage)? {
        let entry = entry.map_err(|_| OperationError::Storage)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(id) = name.strip_suffix(".json").filter(|id| valid_id(id)) else {
            continue;
        };
        let record = load_operation(env, platform, id)?;
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .map_err(|_| OperationError::Storage)?;
        if latest
            .as_ref()
            .is_none_or(|(previous, _)| modified > *previous)
        {
            latest = Some((modified, record));
        }
    }
    Ok(latest.map(|(_, record)| record))
}
pub(super) fn create_operation(
    env: &dyn EnvSource,
    platform: HostPlatform,
    definition_sha256: String,
    proposal: &super::controller::AccessProposal,
    tailscale_action: Option<super::tailscale::TailscalePlanAction>,
) -> Result<AccessOperation, OperationError> {
    let directory = directory(env, platform)?;
    if !directory.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&directory)
                .map_err(|_| OperationError::Storage)?;
        }
        #[cfg(not(unix))]
        return Err(OperationError::Storage);
    }
    verify_directory(&directory)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join("operations.lock"))
        .map_err(|_| OperationError::Storage)?;
    lock.try_lock().map_err(|_| OperationError::Busy)?;
    if latest_operation(env, platform)?.is_some_and(|previous| {
        matches!(
            previous.state,
            OperationState::Scheduled | OperationState::Applying
        )
    }) {
        return Err(OperationError::Busy);
    }
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|_| OperationError::Storage)?;
    let id = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let record = AccessOperation {
        id,
        definition_sha256,
        browser_origin: proposal.browser_origin.clone(),
        worker_origin: proposal.worker_origin.clone(),
        retained_frontdoor_origins: proposal.retained_frontdoor_origins.clone(),
        cors_allowed_origins: proposal.cors_allowed_origins.clone(),
        tailscale_action,
        state: OperationState::Scheduled,
        detail: None,
    };
    write_record(&directory, &record)?;
    Ok(record)
}
