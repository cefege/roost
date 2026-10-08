//! A capability-guarded local endpoint a worker serves to its own children: a
//! Unix socket (a named pipe on Windows) beside a 64-hex capability file that
//! is created once and stays stable across restarts. Ports v2
//! `packages/host/src/local-endpoint.ts`.
//! Called by `agents::environment`, which resolves the agent-report endpoint,
//! and by `agents::report_server`, which prepares, binds and secures it.

use std::io::{ErrorKind, Write};

use roost_keeper::owner_only::{
    create_new_private_file, create_private_dir_all, restrict_to_owner,
};
use std::path::{Path, PathBuf};
use std::time::Duration;

use roost_protocol::fingerprint::{fingerprint_hex, is_fingerprint_hex};
use sha2::{Digest, Sha256};

/// Bytes a connection may send before it authenticates.
pub const LOCAL_ENDPOINT_UNAUTHENTICATED_MAX_BYTES: usize = 64 * 1024;
/// How long a connection may stay unauthenticated.
pub const LOCAL_ENDPOINT_UNAUTHENTICATED_TIMEOUT: Duration = Duration::from_millis(2_000);
/// How many unauthenticated connections an endpoint holds at once.
pub const LOCAL_ENDPOINT_MAX_UNAUTHENTICATED_CONNECTIONS: usize = 16;
/// The endpoint kind a Unix host serves, exported to children verbatim.
pub const LOCAL_ENDPOINT_KIND_UDS: &str = "uds";
/// The endpoint kind a Windows host serves, exported to children verbatim.
pub const LOCAL_ENDPOINT_KIND_NAMED_PIPE: &str = "named-pipe";

/// The endpoint kind this build serves.
#[cfg(unix)]
pub const fn local_endpoint_kind() -> &'static str {
    LOCAL_ENDPOINT_KIND_UDS
}

/// The endpoint kind this build serves.
#[cfg(windows)]
pub const fn local_endpoint_kind() -> &'static str {
    LOCAL_ENDPOINT_KIND_NAMED_PIPE
}

const CAPABILITY_BYTES: usize = 32;
const ENDPOINT_NAME_MAX_BYTES: usize = 64;

/// One resolved endpoint: where it listens and the secret its clients prove.
#[derive(Clone, PartialEq, Eq)]
pub struct LocalEndpoint {
    pub address: PathBuf,
    /// 64 lowercase hex characters, read from `capability_path`.
    pub capability: String,
    pub capability_path: PathBuf,
}

/// The capability is a credential, so a logged endpoint never shows it.
impl std::fmt::Debug for LocalEndpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalEndpoint")
            .field("address", &self.address)
            .field("capability_path", &self.capability_path)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LocalEndpointError {
    #[error("invalid local endpoint name: {0}")]
    InvalidName(String),
    #[error("invalid local endpoint capability: {}", .0.display())]
    InvalidCapability(PathBuf),
    #[error("local endpoint {action} failed at {}: {source}", .path.display())]
    Io {
        action: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
}

fn io_fault(
    action: &'static str,
    path: &Path,
) -> impl FnOnce(std::io::Error) -> LocalEndpointError {
    let path = path.to_path_buf();
    move |source| LocalEndpointError::Io {
        action,
        path,
        source,
    }
}

/// Resolve the same stable endpoint every time for one name under one
/// directory, creating the capability file the first time.
pub fn resolve_local_endpoint(
    name: &str,
    data_dir: &Path,
) -> Result<LocalEndpoint, LocalEndpointError> {
    validate_endpoint_name(name)?;
    let capability_path = data_dir.join(format!("{name}.cap"));
    let capability = load_or_create_capability(&capability_path)?;
    Ok(LocalEndpoint {
        address: endpoint_address(name, data_dir),
        capability,
        capability_path,
    })
}

/// The socket file beside the capability.
#[cfg(unix)]
fn endpoint_address(name: &str, data_dir: &Path) -> PathBuf {
    data_dir.join(format!("{name}.sock"))
}

/// A named pipe unique to this data directory, so two workers on one machine
/// never contend for one pipe name.
#[cfg(windows)]
fn endpoint_address(name: &str, data_dir: &Path) -> PathBuf {
    let digest: [u8; 32] = Sha256::digest(data_dir.to_string_lossy().as_bytes()).into();
    let digest = fingerprint_hex(&digest);
    PathBuf::from(format!(r"\\.\pipe\roost-{name}-{}", &digest[..16]))
}

/// Make the socket's directory and clear a stale socket file, so a bind after
/// an unclean exit does not fail on the inode the dead process left behind.
#[cfg(unix)]
pub fn prepare_local_endpoint(endpoint: &LocalEndpoint) -> Result<(), LocalEndpointError> {
    if let Some(parent) = endpoint.address.parent() {
        private_directory(parent)?;
    }
    remove_if_present(&endpoint.address)
}

/// Make the capability's directory. A named pipe leaves no file behind when
/// its server exits, so there is nothing stale to clear.
#[cfg(windows)]
pub fn prepare_local_endpoint(endpoint: &LocalEndpoint) -> Result<(), LocalEndpointError> {
    if let Some(parent) = endpoint.capability_path.parent() {
        private_directory(parent)?;
    }
    Ok(())
}

/// Restrict a bound socket to this user. Mode, not a check at accept: another
/// local user who cannot connect never gets to present a guessed capability.
#[cfg(unix)]
pub fn secure_local_endpoint(endpoint: &LocalEndpoint) -> Result<(), LocalEndpointError> {
    restrict_to_owner(&endpoint.address).map_err(io_fault("secure", &endpoint.address))
}

/// A named pipe is refused to remote clients at creation; the capability is
/// the local proof.
#[cfg(windows)]
pub fn secure_local_endpoint(_endpoint: &LocalEndpoint) -> Result<(), LocalEndpointError> {
    Ok(())
}

/// Remove the socket file an endpoint served.
#[cfg(unix)]
pub fn cleanup_local_endpoint(endpoint: &LocalEndpoint) -> Result<(), LocalEndpointError> {
    remove_if_present(&endpoint.address)
}

/// A named pipe disappears with its last handle; nothing to remove.
#[cfg(windows)]
pub fn cleanup_local_endpoint(_endpoint: &LocalEndpoint) -> Result<(), LocalEndpointError> {
    Ok(())
}

/// Whether `received` is `expected`, compared over SHA-256 digests in constant
/// time so the comparison leaks neither the length nor a matching prefix.
pub fn verify_local_endpoint_capability(expected: &str, received: &str) -> bool {
    let expected: [u8; 32] = Sha256::digest(expected.as_bytes()).into();
    let received: [u8; 32] = Sha256::digest(received.as_bytes()).into();
    expected
        .iter()
        .zip(received.iter())
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

fn validate_endpoint_name(name: &str) -> Result<(), LocalEndpointError> {
    let bytes = name.as_bytes();
    let shaped = !bytes.is_empty()
        && bytes.len() <= ENDPOINT_NAME_MAX_BYTES
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if shaped {
        Ok(())
    } else {
        Err(LocalEndpointError::InvalidName(name.to_owned()))
    }
}

fn private_directory(path: &Path) -> Result<(), LocalEndpointError> {
    create_private_dir_all(path).map_err(io_fault("mkdir", path))
}

#[cfg(unix)]
fn remove_if_present(path: &Path) -> Result<(), LocalEndpointError> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != ErrorKind::NotFound => Err(io_fault("remove", path)(error)),
        _ => Ok(()),
    }
}

fn read_capability(path: &Path) -> Result<String, LocalEndpointError> {
    let bytes = std::fs::read(path).map_err(io_fault("read", path))?;
    let text = String::from_utf8_lossy(&bytes);
    let capability = text.trim();
    if is_fingerprint_hex(capability) {
        Ok(capability.to_owned())
    } else {
        Err(LocalEndpointError::InvalidCapability(path.to_path_buf()))
    }
}

/// The capability at `path`, minting it with an exclusive create when absent.
/// A concurrent resolver that loses the create race reads the winner's value,
/// which is why the create is exclusive rather than a plain write.
fn load_or_create_capability(path: &Path) -> Result<String, LocalEndpointError> {
    if let Some(parent) = path.parent() {
        private_directory(parent)?;
    }
    match read_capability(path) {
        Err(LocalEndpointError::Io { source, .. }) if source.kind() == ErrorKind::NotFound => {}
        resolved => return resolved,
    }
    let capability = random_capability()?;
    let mut file = match create_new_private_file(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::AlreadyExists => return read_capability(path),
        Err(error) => return Err(io_fault("create", path)(error)),
    };
    file.write_all(format!("{capability}\n").as_bytes())
        .map_err(io_fault("write", path))?;
    drop(file);
    restrict_to_owner(path).map_err(io_fault("secure", path))?;
    tracing::info!(
        path = %path.display(),
        "a local endpoint capability was minted; it stays stable across restarts"
    );
    Ok(capability)
}

fn random_capability() -> Result<String, LocalEndpointError> {
    let mut bytes = [0u8; CAPABILITY_BYTES];
    getrandom::fill(&mut bytes).map_err(|error| LocalEndpointError::Io {
        action: "read",
        path: PathBuf::from("the OS random source"),
        source: std::io::Error::other(error),
    })?;
    Ok(fingerprint_hex(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_capability_matches_only_itself() {
        let capability = "a".repeat(64);
        assert!(verify_local_endpoint_capability(&capability, &capability));
        assert!(!verify_local_endpoint_capability(
            &capability,
            &"b".repeat(64)
        ));
        assert!(!verify_local_endpoint_capability(
            &capability,
            &"a".repeat(63)
        ));
        assert!(!verify_local_endpoint_capability(&capability, ""));
    }

    #[test]
    fn an_endpoint_name_is_one_safe_path_segment() {
        for accepted in ["agent-report", "a", "A.b_c-9"] {
            assert!(validate_endpoint_name(accepted).is_ok(), "{accepted}");
        }
        let too_long = "a".repeat(65);
        for refused in ["", "-lead", ".hidden", "a/b", "a b", too_long.as_str()] {
            assert!(validate_endpoint_name(refused).is_err(), "{refused}");
        }
    }
}
