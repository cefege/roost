//! Short-lived host-owned proof that a candidate route reaches this coordinator.
//!
//! The file is private and names one validated HTTPS origin. HTTP admission may
//! consult it only for the dedicated challenge endpoint, never other routes.

use super::operation::OperationError;
use crate::{EnvSource, HostPlatform};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteChallenge {
    pub origin: String,
    pub token: String,
    pub expires_at: u64,
}

impl std::fmt::Debug for RouteChallenge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RouteChallenge")
            .field("origin", &self.origin)
            .field("token", &"[redacted]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

fn path(env: &dyn EnvSource, platform: HostPlatform) -> Result<PathBuf, OperationError> {
    crate::coord_data_dir(env, platform)
        .map(|directory| {
            directory
                .join("access-operations")
                .join("route-challenge.json")
        })
        .map_err(|_| OperationError::Storage)
}

fn now() -> Result<u64, OperationError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| OperationError::Storage)
}

pub fn read_active_route_challenge(
    env: &dyn EnvSource,
    platform: HostPlatform,
) -> Result<Option<RouteChallenge>, OperationError> {
    let path = path(env, platform)?;
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(OperationError::Storage),
    };
    let parent = path.parent().ok_or(OperationError::Storage)?;
    let parent_metadata = fs::symlink_metadata(parent).map_err(|_| OperationError::Storage)?;
    if !parent_metadata.is_dir() || parent_metadata.file_type().is_symlink() {
        return Err(OperationError::Storage);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if parent_metadata.permissions().mode() & 0o077 != 0 {
            return Err(OperationError::Storage);
        }
    }
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 4096 {
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
    let challenge: RouteChallenge =
        serde_json::from_slice(&bytes).map_err(|_| OperationError::InvalidRecord)?;
    let current = now()?;
    if challenge.expires_at <= current {
        return Ok(None);
    }
    if challenge.expires_at > current.saturating_add(300)
        || challenge.token.len() != 64
        || !challenge
            .token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(OperationError::InvalidRecord);
    }
    let origin = crate::normalize_https_origin(Some(&challenge.origin), "access.challenge")
        .map_err(|_| OperationError::InvalidRecord)?;
    if origin.as_deref() != Some(challenge.origin.as_str()) {
        return Err(OperationError::InvalidRecord);
    }
    Ok(Some(challenge))
}

pub fn arm_route_challenge(
    env: &dyn EnvSource,
    platform: HostPlatform,
    origin: &str,
) -> Result<RouteChallenge, OperationError> {
    let origin = crate::normalize_https_origin(Some(origin), "access.challenge")
        .map_err(|_| OperationError::InvalidRecord)?
        .ok_or(OperationError::InvalidRecord)?;
    if read_active_route_challenge(env, platform)?.is_some() {
        return Err(OperationError::Busy);
    }
    let path = path(env, platform)?;
    let parent = path.parent().ok_or(OperationError::Storage)?;
    let parent_metadata = fs::symlink_metadata(parent).map_err(|_| OperationError::Storage)?;
    if !parent_metadata.is_dir() || parent_metadata.file_type().is_symlink() {
        return Err(OperationError::Storage);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if parent_metadata.permissions().mode() & 0o077 != 0 {
            return Err(OperationError::Storage);
        }
    }
    match fs::remove_file(&path) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(_) => return Err(OperationError::Storage),
    }
    let mut random = [0u8; 32];
    getrandom::fill(&mut random).map_err(|_| OperationError::Storage)?;
    let challenge = RouteChallenge {
        origin,
        token: random.iter().map(|byte| format!("{byte:02x}")).collect(),
        expires_at: now()?.saturating_add(300),
    };
    let bytes = serde_json::to_vec(&challenge).map_err(|_| OperationError::InvalidRecord)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|_| OperationError::Storage)?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| OperationError::Storage)?;
    Ok(challenge)
}

pub fn clear_route_challenge(
    env: &dyn EnvSource,
    platform: HostPlatform,
    token: &str,
) -> Result<(), OperationError> {
    if let Some(active) = read_active_route_challenge(env, platform)? {
        if active.token != token {
            return Err(OperationError::InvalidRecord);
        }
        fs::remove_file(path(env, platform)?).map_err(|_| OperationError::Storage)?;
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::MapEnv;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn expired_challenge_is_not_admitted_and_wrong_token_cannot_clear_it() {
        let directory = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
        let operations = directory.path().join("access-operations");
        fs::create_dir(&operations).unwrap_or_else(|error| panic!("{error}"));
        fs::set_permissions(&operations, fs::Permissions::from_mode(0o700))
            .unwrap_or_else(|error| panic!("{error}"));
        let env = MapEnv::new().with(
            crate::COORD_DATA_DIR_ENV,
            &directory.path().to_string_lossy(),
        );
        let challenge = arm_route_challenge(&env, HostPlatform::Linux, "https://example.com")
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(clear_route_challenge(&env, HostPlatform::Linux, &"a".repeat(64)).is_err());
        assert!(
            read_active_route_challenge(&env, HostPlatform::Linux)
                .unwrap_or_else(|error| panic!("{error}"))
                .is_some()
        );
        let expired = RouteChallenge {
            expires_at: 0,
            ..challenge
        };
        fs::write(
            operations.join("route-challenge.json"),
            serde_json::to_vec(&expired).unwrap_or_else(|error| panic!("{error}")),
        )
        .unwrap_or_else(|error| panic!("{error}"));
        assert!(
            read_active_route_challenge(&env, HostPlatform::Linux)
                .unwrap_or_else(|error| panic!("{error}"))
                .is_none()
        );
    }
}
