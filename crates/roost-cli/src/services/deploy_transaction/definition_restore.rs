//! Saved-definition integrity checks and restoration for deploy rollback.
//! Called by the journaled transaction before reactivating the prior service.
//! Uses durable writes and verifies the recorded digest before replacement.

use std::path::Path;

use tracing::warn;

use super::DeployError;
use crate::services::atomic_file::write_durable;
use crate::services::definition_text::DEFINITION_MODE;
use crate::services::deploy_journal::{SavedDefinition, sha256_hex};
use crate::services::service_spec::ServiceTarget;

/// Write the saved bytes back, or remove the definition when this deploy was
/// the first install. The saved copy's digest is checked against the journal's
/// before it is installed, so a truncated save cannot quietly replace a
/// definition that was working.
pub(super) fn restore(
    target: &ServiceTarget,
    previous: &SavedDefinition,
) -> Result<(), DeployError> {
    match previous {
        SavedDefinition::Absent => remove_quietly(&target.definition_path),
        SavedDefinition::Saved { sha256, .. } => {
            let saved_path = SavedDefinition::saved_path(&target.definition_path);
            let bytes = std::fs::read(&saved_path).map_err(|error| DeployError::Io {
                path: saved_path.clone(),
                cause: error.to_string(),
            })?;
            if sha256_hex(&bytes) != *sha256 {
                return Err(DeployError::Io {
                    path: saved_path,
                    cause: "the saved definition does not match the digest the journal recorded"
                        .to_string(),
                });
            }
            write_durable(&target.definition_path, &bytes, DEFINITION_MODE).map_err(|error| {
                DeployError::Io {
                    path: target.definition_path.clone(),
                    cause: error.to_string(),
                }
            })?;
        }
    }
    Ok(())
}

pub(super) fn remove_quietly(path: &Path) {
    if let Err(error) = std::fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        warn!(path = %path.display(), "could not remove a deploy artefact: {error}");
    }
}
