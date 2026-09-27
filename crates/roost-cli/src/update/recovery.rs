//! Resolving a self-update a previous run left in flight, before anything else
//! happens. Called by `update::mod` at the start of every `roost update`; depends
//! on the update group's journal and rollout modules and on nothing else.
//!
//! A self-replace has one irreversible step and it is a rename, so the states a
//! crash can leave are few and each is decidable from what is on disk. The
//! journal names the two digests the executable is allowed to have — the one it
//! was and the one it was being replaced with — and every decision here is made
//! by reading the digest at the path and comparing it to those two. A file that
//! hashes to neither is not resolved, rolled back or overwritten: it is
//! something this update did not write, and the journal is kept so an operator
//! can look at both the file and the copy beside it.
//!
//! The two outcomes are deliberately different in what they report. `Installed`
//! means the new binary is already the one the journal names, so the retained
//! copy is retired and the update is done. `Restored` means the previous binary
//! was put back, and the caller is expected to REFUSE to carry on: a swap that
//! had to be rolled back has already failed once for a reason this module did
//! not discover, and retrying it immediately is how a bad release gets installed
//! anyway.

use std::fs;
use std::path::Path;

use tracing::{info, warn};

use crate::update::journal::SelfUpdateJournal;
use crate::update::rollout::{ReplaceError, digest_of_file, restore_previous};

/// What resolving an in-flight self-update did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryOutcome {
    /// No journal: nothing was in flight.
    Nothing,
    /// The swap had not happened; the retained copy was retired and the update
    /// may proceed from a clean machine.
    PreparedCleaned,
    /// The new binary is installed and the retained copy was retired.
    InstalledCommitted,
    /// The previous binary was put back, and the caller must not retry.
    PreviousRestored,
}

impl RecoveryOutcome {
    /// Whether this outcome ends the update.
    pub const fn is_terminal(self) -> bool {
        !matches!(self, RecoveryOutcome::Nothing | RecoveryOutcome::PreparedCleaned)
    }

    /// Whether the operator has to be told the update was rolled back.
    pub const fn rolled_back(self) -> bool {
        matches!(self, RecoveryOutcome::PreviousRestored)
    }
}

/// Resolve a self-update an earlier run left in flight.
///
/// A journal this build cannot read is an error, never an absent one. A
/// recovery that ignores an unreadable journal treats a half-finished swap as
/// if it had never started, and the machine it leaves behind has a binary
/// nobody can account for.
pub fn resolve_interrupted_update(service_dir: &Path) -> Result<RecoveryOutcome, ReplaceError> {
    let Some(journal) =
        SelfUpdateJournal::load(service_dir).map_err(ReplaceError::Journal)?
    else {
        return Ok(RecoveryOutcome::Nothing);
    };
    let executable = journal.executable();
    let previous_path = journal.previous_path();
    warn!(
        executable = %executable.display(),
        previous = %previous_path.display(),
        phase = ?journal.phase,
        "resolving a self-update that did not finish",
    );
    let found = digest_of_file(&executable).unwrap_or_else(|_| MISSING.to_string());
    if found == journal.target_binary_sha256 {
        // The rename landed; the crash was after it. The new binary is the one
        // the journal names, so the retained copy is dead weight and retiring
        // it is the whole of what is left.
        retire_retained_copy(&previous_path)?;
        SelfUpdateJournal::clear(service_dir)
            .map_err(|error| ReplaceError::SwapFailed {
                path: SelfUpdateJournal::path_in(service_dir),
                cause: error.to_string(),
            })?;
        info!(
            executable = %executable.display(),
            "the previous self-update had installed the new binary; it is committed",
        );
        return Ok(RecoveryOutcome::InstalledCommitted);
    }
    if found == journal.source_binary_sha256 {
        // The rename had not happened. Nothing was replaced, so there is
        // nothing to undo — but the retained copy was written, and leaving it
        // beside a live `roost` is a file no operator can account for.
        retire_retained_copy(&previous_path)?;
        SelfUpdateJournal::clear(service_dir)
            .map_err(|error| ReplaceError::SwapFailed {
                path: SelfUpdateJournal::path_in(service_dir),
                cause: error.to_string(),
            })?;
        info!(
            executable = %executable.display(),
            "the previous self-update had replaced nothing; it is cleared",
        );
        return Ok(RecoveryOutcome::PreparedCleaned);
    }
    // Neither digest. The file at the path is not one this update wrote, and
    // the retained copy beside it is the only other copy anyone has. Restoring
    // over an unrecognised file would destroy whatever put it there, and
    // clearing the journal would destroy the record of what the two digests
    // were. Both stay, and the operator is told where they are.
    Err(ReplaceError::InstalledBinaryUnrecognised {
        path: executable,
        replaced: journal.source_binary_sha256,
        target: journal.target_binary_sha256,
        found,
    })
}

/// The value a digest read takes when the file is not there at all. It is not a
/// digest either recorded one, so a missing file resolves to the unrecognised
/// branch rather than to a silent success.
const MISSING: &str = "missing";

/// Put the previous binary back and clear the journal, for the caller that
/// decides a retry is not what it wants.
pub fn roll_back_interrupted_update(
    service_dir: &Path,
) -> Result<RecoveryOutcome, ReplaceError> {
    let Some(journal) =
        SelfUpdateJournal::load(service_dir).map_err(ReplaceError::Journal)?
    else {
        return Ok(RecoveryOutcome::Nothing);
    };
    restore_previous(&journal)?;
    retire_retained_copy(&journal.previous_path())?;
    SelfUpdateJournal::clear(service_dir).map_err(|error| ReplaceError::SwapFailed {
        path: SelfUpdateJournal::path_in(service_dir),
        cause: error.to_string(),
    })?;
    info!(
        executable = %journal.executable_path,
        "the previous binary was restored and the self-update journal cleared",
    );
    Ok(RecoveryOutcome::PreviousRestored)
}

/// Remove a retained copy, treating an absent one as done.
fn retire_retained_copy(previous_path: &Path) -> Result<(), ReplaceError> {
    match fs::remove_file(previous_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ReplaceError::PreviousNotRetained {
            path: previous_path.to_path_buf(),
            cause: error.to_string(),
        }),
    }
}
