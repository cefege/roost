//! The keeper capability: the 64-hex secret a worker presents in its first
//! `Hello` and the keeper checks before it serves anything. The worker creates
//! it (`load_or_create`); the keeper only reads it (`load`). Ports
//! `loadOrCreateCapability` and `verifyLocalEndpointCapability` of v2
//! `packages/host/src/local-endpoint.ts`.

use std::fmt::{self, Write as _};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use crate::owner_only::{create_new_private_file, create_private_dir_all, restrict_to_owner};

use sha2::{Digest, Sha256};

/// The secret's entropy, rendered as twice as many hex characters.
const CAPABILITY_BYTES: usize = 32;

/// The shared secret a keeper demands at `Hello`.
///
/// `Debug` never prints the value: the capability rides inside every endpoint
/// and client a log line may render, and a secret in a log is a secret any
/// reader of that log holds.
#[derive(Clone)]
pub struct KeeperCapability(String);

/// Why a capability could not be read or created.
#[derive(Debug, thiserror::Error)]
pub enum CapabilityError {
    #[error("the keeper capability file {0} does not exist")]
    Missing(PathBuf),
    #[error("the keeper capability file {0} is not 64 lowercase hex characters")]
    Malformed(PathBuf),
    #[error("the keeper capability file {path} could not be used: {reason}")]
    Io { path: PathBuf, reason: String },
    #[error("the entropy source is unreadable: {0}")]
    Entropy(String),
}

impl fmt::Debug for KeeperCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KeeperCapability(<redacted>)")
    }
}

impl KeeperCapability {
    /// Read an existing capability. The keeper uses this: a keeper that minted
    /// its own secret would demand one no worker holds.
    pub fn load(path: &Path) -> Result<Self, CapabilityError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Err(CapabilityError::Missing(path.to_path_buf()));
            }
            Err(error) => return Err(io_error(path, &error)),
        };
        let value = text.trim();
        if !is_lowercase_hex_secret(value) {
            return Err(CapabilityError::Malformed(path.to_path_buf()));
        }
        Ok(Self(value.to_owned()))
    }

    /// Read the capability, or mint it when the file does not exist yet. The
    /// worker uses this, once per keeper dial.
    ///
    /// The file is created exclusively, so two processes racing to mint agree
    /// on the one that won rather than each keeping its own.
    pub fn load_or_create(path: &Path) -> Result<Self, CapabilityError> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            create_private_dir_all(parent).map_err(|error| io_error(parent, &error))?;
        }
        match Self::load(path) {
            Err(CapabilityError::Missing(_)) => {}
            loaded => return loaded,
        }
        let minted = Self::mint()?;
        let mut file = match create_new_private_file(path) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => return Self::load(path),
            Err(error) => return Err(io_error(path, &error)),
        };
        file.write_all(format!("{}\n", minted.0).as_bytes())
            .map_err(|error| io_error(path, &error))?;
        restrict_to_owner(path).map_err(|error| io_error(path, &error))?;
        tracing::info!(path = %path.display(), "minted the keeper capability");
        Ok(minted)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether `presented` is this capability. Both sides are hashed first so
    /// the comparison runs over two equal-length digests in constant time: a
    /// byte-wise early exit would tell a local attacker how much of a guess was
    /// right.
    pub fn verify(&self, presented: &str) -> bool {
        let expected = Sha256::digest(self.0.as_bytes());
        let received = Sha256::digest(presented.as_bytes());
        expected
            .iter()
            .zip(received.iter())
            .fold(0_u8, |difference, (left, right)| {
                difference | (left ^ right)
            })
            == 0
    }

    fn mint() -> Result<Self, CapabilityError> {
        let mut bytes = [0_u8; CAPABILITY_BYTES];
        getrandom::fill(&mut bytes).map_err(|error| CapabilityError::Entropy(error.to_string()))?;
        let mut hex = String::with_capacity(CAPABILITY_BYTES * 2);
        for byte in bytes {
            let _ = write!(hex, "{byte:02x}");
        }
        Ok(Self(hex))
    }
}

/// v2's `/^[a-f0-9]{64}$/`.
fn is_lowercase_hex_secret(value: &str) -> bool {
    value.len() == CAPABILITY_BYTES * 2
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn io_error(path: &Path, error: &std::io::Error) -> CapabilityError {
    CapabilityError::Io {
        path: path.to_path_buf(),
        reason: error.to_string(),
    }
}
