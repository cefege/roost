//! The credential the coordinator link dials with. Called by the link loop once
//! per dial, because every dial mints afresh.
//!
//! A credential is never URL material: it travels as the second requested
//! WebSocket subprotocol, and `link_dial::dial_request` is what keeps it that
//! way. This module only produces the string.
//!
//! Minting is re-done per dial rather than cached for the process, because a
//! link that outlives its token authenticates as nothing and fails in a way
//! that reads like a coordinator outage. v2 did the same and refreshed in band
//! on a schedule; the per-dial mint here is strictly simpler and strictly
//! harder to get wrong, at the cost of one signature per reconnect.

use std::path::{Path, PathBuf};

use roost_protocol::wire::WorkerFp;

use crate::host::jwt::{
    COORDINATOR_AUDIENCE, CREDENTIAL_LIFETIME, WorkerKeyError, load_worker_key,
};

/// Why no credential could be produced for a dial.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CredentialError {
    #[error("no coordinator credential could be minted: {reason}")]
    Unavailable { reason: String },
}

impl From<WorkerKeyError> for CredentialError {
    fn from(error: WorkerKeyError) -> Self {
        Self::Unavailable {
            reason: error.to_string(),
        }
    }
}

/// A source of short-lived coordinator credentials.
///
/// `&self` and `Send + Sync` because the link loop mints from inside its own
/// task while a signal handler and the boot sequence are running: the source
/// has to be reachable from all of them without being owned by one.
pub trait CredentialSource: Send + Sync {
    /// A credential for exactly one dial.
    fn mint(&self) -> Result<String, CredentialError>;
}

/// The source the service installs: the worker's own key, read per dial.
///
/// The key FILE is read per dial too, and that is the one piece of state this
/// source does not hold. A cached key would be one signature cheaper per
/// reconnect and would keep signing with a key the operator has rotated out —
/// and the rotation is the moment where a stale credential is least
/// diagnosable, because the coordinator's answer is an unknown `kid` for a
/// worker that was working a minute ago. The file is a kilobyte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerKeyCredential {
    key_path: PathBuf,
}

impl WorkerKeyCredential {
    /// A source that signs from the key at `key_path`.
    ///
    /// The path is `WorkerBoot::worker_key_path`, which is where the boot
    /// sequence already derived this worker's fingerprint from, so the identity
    /// in the dial and the identity in the token cannot come from two places.
    #[must_use]
    pub fn new(key_path: impl Into<PathBuf>) -> Self {
        Self {
            key_path: key_path.into(),
        }
    }

    /// The key file this source signs from.
    #[must_use]
    pub fn key_path(&self) -> &Path {
        &self.key_path
    }

    /// The identity a dial from this source presents, without minting.
    ///
    /// A dial that cannot name itself should not be attempted: the coordinator
    /// routes on the fingerprint in the path, so a link whose token and whose
    /// path disagree is refused before the first frame.
    pub fn fingerprint(&self) -> Result<WorkerFp, CredentialError> {
        load_worker_key(&self.key_path)
            .map(|key| key.fingerprint().clone())
            .map_err(CredentialError::from)
    }
}

impl CredentialSource for WorkerKeyCredential {
    fn mint(&self) -> Result<String, CredentialError> {
        let key = load_worker_key(&self.key_path)?;
        tracing::debug!(
            key_path = %self.key_path.display(),
            fingerprint = %key.fingerprint(),
            "link: signing a coordinator credential for this dial"
        );
        key.mint_credential(COORDINATOR_AUDIENCE, CREDENTIAL_LIFETIME)
            .map_err(CredentialError::from)
    }
}
