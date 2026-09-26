//! Why a keeper client could not do what it was asked. Owned by the worker,
//! through [`crate::client`].
//!
//! Every variant names a cause an operator can act on, because a keeper
//! problem is a support ticket and "it did not work" is not a diagnosable
//! report. The spawn timeout in particular exists to turn a hang into a line
//! in a log.

/// Why the client could not do what it was asked.
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("no keeper is listening on {0}")]
    NotListening(PathBuf),
    #[error("the keeper at {path} did not answer a spawn within {timeout:?}")]
    SpawnNotAcknowledged { path: PathBuf, timeout: Duration },
    #[error("the keeper refused the spawn: {0}")]
    SpawnRefused(String),
    #[error("the keeper does not support {0:?}")]
    Unsupported(&'static str),
    #[error("the connection to the keeper failed: {0}")]
    Io(String),
}
