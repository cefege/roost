//! The record of a self-replace in flight, and the previous copy of the binary
//! it can put back. Written by `update::rollout` before the swap and retired by
//! it only after the new binary is proved on disk; depends on `atomic_file`'s
//! durable writer for the write itself and on nothing else in the update group.
//!
//! The journal is written BEFORE the previous copy is retained, which reverses
//! the deploy transaction's order, and the reversal is the point. Both orders
//! keep the journal ahead of the swap, but they differ in what a crash between
//! the two leaves on the machine. This order leaves a journal that says the
//! swap had not started and names where the copy was going, which recovery can
//! act on. The other leaves a rollback file beside a live binary that nothing on
//! disk mentions, and the next operator has to recognise it by name.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::services::atomic_file::write_durable;

/// The journal format version. A journal whose schema is not this one is
/// refused rather than guessed at: an older layout describes a rollback point
/// this build cannot honour, and a newer one names fields it would drop.
pub const SELF_UPDATE_JOURNAL_SCHEMA: u32 = 1;

/// The journal's file name inside the service directory.
pub const JOURNAL_FILE_NAME: &str = "self-update.json";

/// The suffix the replaced executable is saved under, beside itself, so a
/// rollback restores a path this update already proved it can write.
pub const PREVIOUS_EXECUTABLE_SUFFIX: &str = ".roost-update-previous";

/// The permission bits a journal carries. It names the keeper a swap was
/// admitted over, which is one machine's business and nobody else's.
const JOURNAL_MODE: u32 = 0o600;

/// The longest target version a journal will carry. A release tag is a short
/// label; anything longer is a journal written by something else.
const MAX_TARGET_VERSION_BYTES: usize = 128;

/// Where in its lifecycle a self-replace is.
///
/// Two phases, and each exists because recovery does something different for
/// it. There is deliberately no keeper phase: the admission is decided and
/// recorded before this journal is written, and no keeper is ever mutated by a
/// swap, because the keeper is a separate binary this command does not touch.
/// There is no committing phase either, because there is nothing to converge —
/// the rename either landed or it did not, and both are the same question asked
/// of the digest on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelfUpdatePhase {
    /// The journal is on disk and the swap has not been proved.
    Prepared,
    /// The new binary is on disk and proved; retiring the copy is all that is
    /// left, and that is not a step worth a phase of its own.
    Installed,
}

/// What the swap was allowed to do to the keeper on this machine.
///
/// Recorded because a retained journal is where an operator goes to ask what
/// was at risk, and "the swap touched one file" is only reassuring when the
/// journal says which keeper that file was admitted over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum KeeperRecord {
    /// No running keeper on this machine could be identified, so the swap had
    /// nothing to protect. The reason travels with it because "no roster" and
    /// "no local coordinator" and "no worker definition" are different
    /// situations that produce the same safe outcome.
    NoRunningKeeper {
        /// Why no keeper could be identified, in one sentence.
        reason: String,
    },
    /// A running keeper was identified and the candidate was admitted over it.
    Admitted {
        /// The worker the keeper belongs to, which is the machine's own name.
        worker_fingerprint: String,
        /// The coordinator's own classification of the swap.
        classification: String,
        /// The action that classification permits, which for a swap that does
        /// not touch the keeper is always the preserving one.
        required_action: String,
    },
}

/// The journal written before an executable is replaced, and read back by the
/// next `roost update` on the machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelfUpdateJournal {
    /// Always [`SELF_UPDATE_JOURNAL_SCHEMA`]; a mismatch is refused on read.
    pub schema: u32,
    /// Where in its lifecycle this swap is.
    pub phase: SelfUpdatePhase,
    /// When the swap started, in milliseconds since the epoch.
    pub created_at_ms: i64,
    /// The release this swap installs, as the release tagged it.
    pub target_version: String,
    /// The executable being replaced, absolute.
    pub executable_path: String,
    /// Where the replaced bytes are retained, beside `executable_path`.
    pub previous_path: String,
    /// The digest of the binary as it was found.
    pub source_binary_sha256: String,
    /// The digest the release published for the candidate.
    pub target_binary_sha256: String,
    /// The replaced binary's permission bits, so a restore puts back the mode
    /// the operator had rather than a default.
    pub source_binary_mode: u32,
    /// What the swap was admitted to do to the keeper.
    pub keeper: KeeperRecord,
}

impl SelfUpdateJournal {
    /// The journal to write before `executable` is replaced by `target`.
    #[allow(clippy::too_many_arguments)]
    pub fn prepared(
        created_at_ms: i64,
        target_version: &str,
        executable: &Path,
        source_binary_sha256: &str,
        target_binary_sha256: &str,
        source_binary_mode: u32,
        keeper: KeeperRecord,
    ) -> Result<Self, String> {
        let journal = SelfUpdateJournal {
            schema: SELF_UPDATE_JOURNAL_SCHEMA,
            phase: SelfUpdatePhase::Prepared,
            created_at_ms,
            target_version: target_version.to_string(),
            executable_path: canonical_path(executable)?,
            previous_path: canonical_path(&SelfUpdateJournal::previous_path_for(executable))?,
            source_binary_sha256: source_binary_sha256.to_string(),
            target_binary_sha256: target_binary_sha256.to_string(),
            source_binary_mode,
            keeper,
        };
        journal.validate()?;
        Ok(journal)
    }

    /// The same journal, one step further on.
    pub fn installed(&self) -> Self {
        Self {
            phase: SelfUpdatePhase::Installed,
            ..self.clone()
        }
    }

    /// The path the replaced executable is retained at, beside itself.
    pub fn previous_path_for(executable: &Path) -> PathBuf {
        let mut name = executable.file_name().map_or_else(
            || "roost".to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        name.push_str(PREVIOUS_EXECUTABLE_SUFFIX);
        match executable.parent() {
            Some(parent) => parent.join(name),
            None => PathBuf::from(name),
        }
    }

    /// The path this journal is written to inside `service_dir`.
    pub fn path_in(service_dir: &Path) -> PathBuf {
        service_dir.join(JOURNAL_FILE_NAME)
    }

    /// The executable this journal is about.
    pub fn executable(&self) -> PathBuf {
        PathBuf::from(&self.executable_path)
    }

    /// Where the replaced bytes are retained.
    pub fn previous_path(&self) -> PathBuf {
        PathBuf::from(&self.previous_path)
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
    /// file: a recovery that ignores an unreadable journal treats a
    /// half-finished swap as if it had never started.
    pub fn load(service_dir: &Path) -> Result<Option<Self>, String> {
        let path = Self::path_in(service_dir);
        let encoded = match std::fs::read(&path) {
            Ok(encoded) => encoded,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        let journal: Self = serde_json::from_slice(&encoded)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if journal.schema != SELF_UPDATE_JOURNAL_SCHEMA {
            return Err(format!(
                "{}: self-update journal schema {} is not {}",
                path.display(),
                journal.schema,
                SELF_UPDATE_JOURNAL_SCHEMA
            ));
        }
        journal.validate()?;
        Ok(Some(journal))
    }

    /// Remove the journal. Called only once the new binary is proved, or once a
    /// restore is proved — never on the way to discovering a failure.
    pub fn clear(service_dir: &Path) -> Result<(), std::io::Error> {
        match std::fs::remove_file(Self::path_in(service_dir)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// The checks a journal has to pass before anything acts on it.
    ///
    /// Every one of these is a field this build would otherwise act on without
    /// having read it: a relative path would resolve against whatever directory
    /// the next run happened to start in, a rollback path that is not the
    /// executable's own sibling would restore bytes from a place the swap never
    /// proved it could write, and a digest that is not 64 hex characters is not
    /// a digest.
    fn validate(&self) -> Result<(), String> {
        if self.created_at_ms <= 0 {
            return Err("self-update journal has no usable timestamp".to_string());
        }
        if self.target_version.is_empty()
            || self.target_version.len() > MAX_TARGET_VERSION_BYTES
            || contains_control(&self.target_version)
        {
            return Err("self-update journal names no usable target version".to_string());
        }
        for (label, value) in [
            ("executable", &self.executable_path),
            ("rollback", &self.previous_path),
        ] {
            if !Path::new(value).is_absolute() || contains_control(value) {
                return Err(format!("self-update journal {label} path is not usable"));
            }
        }
        let expected = Self::previous_path_for(Path::new(&self.executable_path))
            .to_string_lossy()
            .into_owned();
        if self.previous_path != expected {
            return Err(
                "self-update journal rollback path is not beside the executable it replaces"
                    .to_string(),
            );
        }
        for (label, value) in [
            ("source", &self.source_binary_sha256),
            ("target", &self.target_binary_sha256),
        ] {
            if !is_digest(value) {
                return Err(format!("self-update journal {label} digest is not a sha256"));
            }
        }
        if self.source_binary_mode > 0o7777 {
            return Err("self-update journal source mode is not a permission set".to_string());
        }
        Ok(())
    }
}

/// A path this journal will act on, spelled the way it will be spelled again
/// when it is read back, so a comparison between the two is a comparison of
/// values and not of two renderings.
fn canonical_path(path: &Path) -> Result<String, String> {
    let text = path.to_string_lossy().into_owned();
    if !path.is_absolute() || contains_control(&text) {
        return Err(format!("{} is not a path a journal can record", path.display()));
    }
    Ok(text)
}

fn contains_control(value: &str) -> bool {
    value.chars().any(|character| character.is_control())
}

fn is_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
