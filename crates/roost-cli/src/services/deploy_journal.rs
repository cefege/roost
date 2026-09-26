//! The record of a deploy whose definition swap is in flight, and the saved
//! copy of what was replaced. The journal's existence IS the in-flight marker:
//! it is written before the swap and removed only after the new service is
//! proven up, so a machine that lost power mid-deploy comes back with both the
//! fact that a swap was in progress and the bytes to undo it.
//!
//! The saved copy is a separate file beside the definition rather than a field
//! in the journal, because the journal has to stay small enough to be read with
//! one `read` after a crash and the definition can be a few kilobytes. The
//! journal records the copy's digest, so a rollback refuses to install a
//! truncated save over a working definition.

use std::path::{Path, PathBuf};

use roost_host::HostPlatform;
use serde::{Deserialize, Serialize};

use crate::services::atomic_file::write_durable;

/// The journal format version. A journal whose schema is not this one is
/// refused rather than guessed at: an older layout describes a rollback point
/// this build cannot honour, and a newer one describes fields it would drop.
pub const DEPLOY_JOURNAL_SCHEMA: u32 = 1;

/// The journal's file name inside the service directory.
pub const JOURNAL_FILE_NAME: &str = "deploy-journal.json";

/// The suffix the replaced definition is saved under, beside itself. Keeping it
/// beside the definition rather than in the service directory means a rollback
/// restores a path the deploy already proved it can write.
pub const PREVIOUS_DEFINITION_SUFFIX: &str = ".roost-previous";

/// The permission bits a journal and a saved definition carry. A saved
/// definition can name a one-shot grant, so it is never readable by anyone else.
const JOURNAL_MODE: u32 = 0o600;

/// Where in its lifecycle a journalled deploy is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployPhase {
    /// The new definition is about to replace the saved one.
    Swapping,
    /// The new definition did not come up, and the saved one is being put back.
    RollingBack,
}

/// What was installed before this deploy, and the proof it is the right copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SavedDefinition {
    /// Nothing was installed. A rollback of a first install removes what the
    /// deploy wrote rather than restoring an absence.
    Absent,
    /// The bytes that were replaced, saved beside the definition.
    Saved {
        /// Lowercase hex SHA-256 of the saved bytes.
        sha256: String,
        /// How many bytes were saved.
        len: u64,
    },
}

impl SavedDefinition {
    /// The record for content that is now on disk at the definition path.
    pub fn saved(bytes: &[u8]) -> Self {
        SavedDefinition::Saved {
            sha256: sha256_hex(bytes),
            len: bytes.len() as u64,
        }
    }

    /// The path a saved copy is kept at, beside the definition it replaces.
    pub fn saved_path(definition_path: &Path) -> PathBuf {
        sibling(definition_path, PREVIOUS_DEFINITION_SUFFIX)
    }

    /// The digest this record promises, or `None` for a first install.
    pub fn sha256(&self) -> Option<&str> {
        match self {
            SavedDefinition::Absent => None,
            SavedDefinition::Saved { sha256, .. } => Some(sha256),
        }
    }
}

/// The journal written before a definition is replaced, and read back by the
/// next deploy on the machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployJournal {
    /// Always [`DEPLOY_JOURNAL_SCHEMA`]; a mismatch is refused on read.
    pub schema: u32,
    /// Where in its lifecycle this deploy is.
    pub phase: DeployPhase,
    /// The service identity being replaced.
    pub label: String,
    /// The platform whose service manager was asked to load it.
    pub platform: String,
    /// The definition file being replaced.
    pub definition_path: String,
    /// What was installed, and the proof the saved copy is the right one.
    pub previous: SavedDefinition,
}

impl DeployJournal {
    /// The journal to write before `target`'s definition is replaced.
    pub fn swapping(
        target: &crate::services::service_spec::ServiceTarget,
        platform: HostPlatform,
        previous: SavedDefinition,
    ) -> Self {
        Self {
            schema: DEPLOY_JOURNAL_SCHEMA,
            phase: DeployPhase::Swapping,
            label: target.label.clone(),
            platform: platform.as_str().to_string(),
            definition_path: target.definition_path.display().to_string(),
            previous,
        }
    }

    /// The same journal, one step further on.
    pub fn rolling_back(&self) -> Self {
        Self {
            phase: DeployPhase::RollingBack,
            ..self.clone()
        }
    }

    /// The path this journal is written to inside `service_dir`.
    pub fn path_in(service_dir: &Path) -> PathBuf {
        service_dir.join(JOURNAL_FILE_NAME)
    }

    /// Write the journal durably, before the step it describes happens.
    pub fn write(&self, service_dir: &Path) -> Result<PathBuf, std::io::Error> {
        let encoded = serde_json::to_vec(self).map_err(std::io::Error::other)?;
        let path = Self::path_in(service_dir);
        write_durable(&path, &encoded, JOURNAL_MODE)?;
        Ok(path)
    }

    /// The journal in `service_dir`, if one is there and it is a shape this
    /// build understands. A journal it cannot parse is an error, not an absent
    /// file: a deploy that ignores an unreadable journal treats a half-finished
    /// swap as if it had never started.
    pub fn load(service_dir: &Path) -> Result<Option<Self>, String> {
        let path = Self::path_in(service_dir);
        let encoded = match std::fs::read(&path) {
            Ok(encoded) => encoded,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        let journal: Self = serde_json::from_slice(&encoded)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if journal.schema != DEPLOY_JOURNAL_SCHEMA {
            return Err(format!(
                "{}: deploy journal schema {} is not {}",
                path.display(),
                journal.schema,
                DEPLOY_JOURNAL_SCHEMA
            ));
        }
        Ok(Some(journal))
    }

    /// The definition file this journal is about, and the service it is for.
    pub fn target(&self) -> Result<RecoveredTarget, String> {
        let platform = HostPlatform::parse(&self.platform)
            .map_err(|error| format!("deploy journal platform: {error}"))?;
        Ok(RecoveredTarget {
            label: self.label.clone(),
            definition_path: PathBuf::from(&self.definition_path),
            platform,
        })
    }

    /// Remove the journal. Called only once the deploy is proven, or once a
    /// rollback is proven — never on the way to discovering a failure.
    pub fn clear(service_dir: &Path) -> Result<(), std::io::Error> {
        match std::fs::remove_file(Self::path_in(service_dir)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

/// A service identity and its platform, rebuilt from a journal. Named for what
/// a recovery run has, not for what a resolved spec would.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredTarget {
    /// The service identity.
    pub label: String,
    /// The definition file the in-flight deploy was replacing.
    pub definition_path: PathBuf,
    /// The platform whose manager was asked to load it.
    pub platform: HostPlatform,
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().map_or_else(
        || "definition".to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    name.push_str(suffix);
    match path.parent() {
        Some(parent) => parent.join(name),
        None => PathBuf::from(name),
    }
}

/// The digest a journal records. SHA-256 because it is what the release
/// pipeline already signs with, so a saved copy can be compared against a
/// manifest an operator already trusts.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(bytes);
    hex::encode(digest)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use roost_host::HostPlatform;

    use super::{DeployJournal, DeployPhase, SavedDefinition, sha256_hex};
    use crate::services::service_spec::ServiceTarget;

    fn journal() -> DeployJournal {
        DeployJournal::swapping(
            &ServiceTarget {
                label: "roost3-worker".to_string(),
                definition_path: PathBuf::from("/u/.config/systemd/user/roost3-worker.service"),
            },
            HostPlatform::Linux,
            SavedDefinition::saved(b"[Unit]\n"),
        )
    }

    #[test]
    fn the_saved_copy_sits_beside_the_definition_it_replaces() {
        assert_eq!(
            SavedDefinition::saved_path(Path::new("/u/.config/systemd/user/roost3-worker.service")),
            PathBuf::from("/u/.config/systemd/user/roost3-worker.service.roost-previous")
        );
    }

    #[test]
    fn a_digest_is_the_one_the_release_pipeline_signs_with() {
        assert_eq!(
            sha256_hex(b"roost"),
            "31a89e3644524440db30f17a7a9676768329cf3f40d1142d327429ea3eb3e6b3"
        );
    }

    #[test]
    fn a_journal_round_trips_through_json_with_its_phase() {
        let encoded = serde_json::to_string(&journal()).expect("journal encodes");
        let decoded: DeployJournal = serde_json::from_str(&encoded).expect("journal decodes");
        assert_eq!(decoded, journal());
        assert_eq!(decoded.rolling_back().phase, DeployPhase::RollingBack);
        assert_eq!(decoded.phase, DeployPhase::Swapping);
    }

    #[test]
    fn a_first_install_records_an_absence_rather_than_an_empty_digest() {
        let first = DeployJournal::swapping(
            &ServiceTarget {
                label: "roost3-coord".to_string(),
                definition_path: PathBuf::from("/u/agent.plist"),
            },
            HostPlatform::MacOs,
            SavedDefinition::Absent,
        );
        assert_eq!(first.previous.sha256(), None);
        assert_eq!(
            first.target().expect("macos parses").platform,
            HostPlatform::MacOs
        );
    }
}
