//! The POSIX self-replace, and what has to be true before one is allowed.
//! Called by `update::mod` only. Depends on the update group's own journal and
//! candidate modules, on `atomic_file`'s durable write and digest, and on
//! nothing else in this crate.
//!
//! Two properties carry this file and neither is visible by reading it.
//!
//! The replacement is a RENAME, never a write into the target. The new bytes go
//! to a fresh file in the target's own directory — same directory, so the
//! rename cannot cross a device boundary — are flushed there, and are given
//! their mode before the rename. Setting the mode afterwards would be a window
//! in which the name `roost` resolves to a file nobody can execute, and a copy
//! that truncates the target in place leaves a machine whose `roost` on `PATH`
//! is half a program.
//!
//! THE RUNNING IMAGE IS NOT THE FILE. On Linux and macOS the kernel holds the
//! old inode open for as long as a process is executing from it, so renaming
//! over a running executable is safe on both: this process finishes running the
//! bytes it started with and the next invocation gets the new ones. That is why
//! the operation is a rename, and it is also why a failed rename is the safe
//! outcome — the original file is byte for byte where it was.

use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use tracing::{info, warn};

use crate::services::atomic_file::write_durable;
use crate::services::deploy_journal::sha256_hex;
use crate::update::candidate::VerifiedCandidate;
use crate::update::journal::{KeeperRecord, SelfUpdateJournal};

/// The permission bits an installed executable carries.
///
/// The mode is set on the STAGING file before the rename, not on the target
/// after it. A rename is the instant the name resolves to the new inode, so a
/// mode applied afterwards is a window in which the binary is unrunnable.
pub const EXECUTABLE_MODE: u32 = 0o755;

/// Every way a self-replace can end without having replaced anything.
#[derive(Debug, thiserror::Error)]
pub enum ReplaceError {
    #[error("no roost binary is installed at {0}")]
    NoInstalledBinary(PathBuf),
    #[error("the installed binary at {path} could not be read: {cause}")]
    Unreadable { path: PathBuf, cause: String },
    #[error("the verified candidate at {path} now hashes to {found}, not the {expected} the release published, so it is not installed")]
    CandidateChanged {
        path: PathBuf,
        expected: String,
        found: String,
    },
    #[error("the previous binary at {path} could not be retained: {cause}")]
    PreviousNotRetained { path: PathBuf, cause: String },
    #[error("the previous binary at {path} does not match the digest recorded for it, so it is not a copy this update may install back")]
    PreviousCopyTruncated { path: PathBuf },
    #[error("the swap onto {path} failed: {cause}")]
    SwapFailed { path: PathBuf, cause: String },
    #[error("the binary at {path} reads as {found}, which is neither the copy this update replaced ({replaced}) nor the one it installed ({target}); it is left alone and the journal is kept")]
    InstalledBinaryUnrecognised {
        path: PathBuf,
        /// Named `replaced`, not `source`: thiserror reads a field literally
        /// called `source` as the error's cause and demands it be an
        /// `Error`. This is a digest, not a cause.
        replaced: String,
        target: String,
        found: String,
    },
    #[error("the self-update journal is not usable: {0}")]
    Journal(String),
    #[error("the keeper on this machine {running}, and the candidate {summary}, so this update is refused and nothing is replaced")]
    KeeperNotAdmissible { running: String, summary: String },
}

/// What one self-replace did, as the operator is told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaceOutcome {
    /// The executable that was replaced.
    pub executable: PathBuf,
    /// The release now installed there.
    pub target_version: String,
    /// The keeper this swap was admitted over, as recorded in the journal.
    pub keeper: KeeperRecord,
}

/// What is installed at `executable`, read without changing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledBinary {
    /// The digest of the binary as found.
    pub sha256: String,
    /// Its permission bits, so a restore puts back the mode the operator had.
    pub mode: u32,
    /// The keeper this swap was admitted over. Set by
    /// [`admit_against_running_keeper`] before the swap, never guessed.
    pub keeper: Option<KeeperRecord>,
}

/// Read what is installed at `executable`.
///
/// A path that is absent is a refusal rather than a default, because "there is
/// nothing here to update" and "here is a `roost` and I could not read it" lead
/// to opposite actions and only one of them is safe to assume.
pub fn read_installed(executable: &Path) -> Result<InstalledBinary, ReplaceError> {
    let bytes = match fs::read(executable) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(ReplaceError::NoInstalledBinary(executable.to_path_buf()));
        }
        Err(error) => {
            return Err(ReplaceError::Unreadable {
                path: executable.to_path_buf(),
                cause: error.to_string(),
            });
        }
    };
    let mode = fs::metadata(executable)
        .map_err(|error| ReplaceError::Unreadable {
            path: executable.to_path_buf(),
            cause: error.to_string(),
        })?
        .permissions()
        .mode()
        & 0o7777;
    Ok(InstalledBinary {
        sha256: sha256_hex(&bytes),
        mode,
        keeper: None,
    })
}

/// Replace `executable` with `candidate`.
///
/// The order is the design and none of it is negotiable: read what is there,
/// prove the candidate is still what the release published, write the journal,
/// retain the old bytes, swap, prove what landed, and only then retire the
/// journal and the retained copy. Every failure before the swap leaves the
/// original file byte for byte where it was. Every failure after it leaves a
/// journal naming the two digests the file on disk is allowed to have, which is
/// what lets the next run tell "the swap landed" from "something else wrote
/// here".
///
/// The candidate is taken as a [`VerifiedCandidate`] rather than a bare path
/// because the digest has to travel WITH it. Re-hashing the file here and
/// journalling whatever came back would accept bytes that changed after the
/// download was verified, and the journal would then record a digest the
/// release never published as the one it installed.
pub fn replace_executable(
    executable: &Path,
    candidate: &VerifiedCandidate,
    target_version: &str,
    installed: &InstalledBinary,
    service_dir: &Path,
    now_ms: i64,
) -> Result<ReplaceOutcome, ReplaceError> {
    let keeper = installed
        .keeper
        .clone()
        .ok_or_else(|| ReplaceError::KeeperNotAdmissible {
            running: "was never admitted against".to_string(),
            summary: "reached a swap with no keeper decision recorded".to_string(),
        })?;
    let candidate_path = candidate.path.as_path();
    let on_disk = digest_of_file(candidate_path)?;
    if on_disk != candidate.sha256 {
        return Err(ReplaceError::CandidateChanged {
            path: candidate_path.to_path_buf(),
            expected: candidate.sha256.clone(),
            found: on_disk,
        });
    }
    let candidate_sha256 = candidate.sha256.clone();
    let journal = SelfUpdateJournal::prepared(
        now_ms,
        target_version,
        executable,
        &installed.sha256,
        &candidate_sha256,
        installed.mode,
        keeper.clone(),
    )
    .map_err(ReplaceError::Journal)?;
    write_journal(&journal, service_dir)?;
    info!(
        executable = %executable.display(),
        source_sha256 = %installed.sha256,
        target_sha256 = %candidate_sha256,
        "self-update journal written; retaining the installed binary",
    );
    retain_previous(&journal)?;
    swap_candidate_into_place(candidate_path, executable)?;
    let landed = digest_of_file(executable)?;
    if landed != candidate_sha256 {
        // The rename returned and the bytes are not the candidate's. Keeping
        // the journal is the only recoverable choice: it names the two digests
        // this file is allowed to have, and the previous copy is still beside
        // it. Removing the journal here is what would make this unrecoverable.
        return Err(ReplaceError::InstalledBinaryUnrecognised {
            path: executable.to_path_buf(),
            replaced: installed.sha256.clone(),
            target: candidate_sha256,
            found: landed,
        });
    }
    let settled = journal.installed();
    write_journal(&settled, service_dir)?;
    retire(&settled, service_dir)?;
    info!(
        executable = %executable.display(),
        target_version,
        "self-update settled",
    );
    Ok(ReplaceOutcome {
        executable: executable.to_path_buf(),
        target_version: target_version.to_string(),
        keeper,
    })
}

fn write_journal(
    journal: &SelfUpdateJournal,
    service_dir: &Path,
) -> Result<(), ReplaceError> {
    journal
        .write(service_dir)
        .map(|_| ())
        .map_err(|error| ReplaceError::SwapFailed {
            path: SelfUpdateJournal::path_in(service_dir),
            cause: error.to_string(),
        })
}

/// Copy the installed binary beside itself and prove the copy before the swap
/// may rely on it.
///
/// A rollback that installs a truncated save over a working binary is worse than
/// no rollback, so the digest is checked here — before anything is replaced —
/// and again before any restore reads the copy back.
fn retain_previous(journal: &SelfUpdateJournal) -> Result<(), ReplaceError> {
    let previous_path = journal.previous_path();
    let executable = journal.executable();
    let bytes = fs::read(&executable).map_err(|error| ReplaceError::PreviousNotRetained {
        path: previous_path.clone(),
        cause: error.to_string(),
    })?;
    // An explicit `if let Err` rather than `map_err`: the closure would have
    // to move `previous_path` to name it in the error, and the value is read
    // twice more below. Returning on the error path moves it only when the
    // function is already leaving.
    if let Err(error) = write_durable(&previous_path, &bytes, journal.source_binary_mode) {
        return Err(ReplaceError::PreviousNotRetained {
            path: previous_path,
            cause: error.to_string(),
        });
    }
    let retained = fs::read(&previous_path).map_err(|error| ReplaceError::PreviousNotRetained {
        path: previous_path.clone(),
        cause: error.to_string(),
    })?;
    if sha256_hex(&retained) != journal.source_binary_sha256 {
        return Err(ReplaceError::PreviousCopyTruncated { path: previous_path });
    }
    Ok(())
}

/// The rename itself: flush the candidate, give it its mode, move it onto the
/// target's name in one step, then flush the directory so the rename itself
/// survives a power cut.
fn swap_candidate_into_place(
    candidate_path: &Path,
    executable: &Path,
) -> Result<(), ReplaceError> {
    let candidate = File::open(candidate_path).map_err(|error| ReplaceError::SwapFailed {
        path: candidate_path.to_path_buf(),
        cause: error.to_string(),
    })?;
    candidate.sync_all().map_err(|error| ReplaceError::SwapFailed {
        path: candidate_path.to_path_buf(),
        cause: error.to_string(),
    })?;
    drop(candidate);
    fs::set_permissions(candidate_path, fs::Permissions::from_mode(EXECUTABLE_MODE)).map_err(
        |error| ReplaceError::SwapFailed {
            path: candidate_path.to_path_buf(),
            cause: error.to_string(),
        },
    )?;
    fs::rename(candidate_path, executable).map_err(|error| ReplaceError::SwapFailed {
        path: executable.to_path_buf(),
        cause: error.to_string(),
    })?;
    if let Some(parent) = executable.parent() {
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| ReplaceError::SwapFailed {
                path: executable.to_path_buf(),
                cause: error.to_string(),
            })?;
    }
    Ok(())
}

/// Put the previous binary back, having proved the retained copy is the one the
/// journal recorded. Never called on a copy that does not hash to it.
pub fn restore_previous(journal: &SelfUpdateJournal) -> Result<(), ReplaceError> {
    let previous_path = journal.previous_path();
    let bytes = fs::read(&previous_path).map_err(|error| ReplaceError::PreviousNotRetained {
        path: previous_path.clone(),
        cause: error.to_string(),
    })?;
    if sha256_hex(&bytes) != journal.source_binary_sha256 {
        return Err(ReplaceError::PreviousCopyTruncated { path: previous_path });
    }
    write_durable(&journal.executable(), &bytes, journal.source_binary_mode).map_err(|error| {
        ReplaceError::PreviousNotRetained {
            path: previous_path,
            cause: error.to_string(),
        }
    })?;
    info!(
        executable = %journal.executable_path,
        "self-update rolled back to the previous binary",
    );
    Ok(())
}

/// The new binary is proved and the retained copy is no longer needed. The copy
/// goes first: a crash between the two leaves a journal whose recovery finds
/// the installed digest already correct and retires both, which is the same
/// terminal state reached one step later.
fn retire(journal: &SelfUpdateJournal, service_dir: &Path) -> Result<(), ReplaceError> {
    if let Err(error) = fs::remove_file(journal.previous_path())
        && error.kind() != std::io::ErrorKind::NotFound
    {
        warn!(
            path = %journal.previous_path,
            reason = %error,
            "the previous binary could not be removed; the journal is kept for recovery",
        );
        return Err(ReplaceError::PreviousNotRetained {
            path: journal.previous_path(),
            cause: error.to_string(),
        });
    }
    SelfUpdateJournal::clear(service_dir).map_err(|error| ReplaceError::SwapFailed {
        path: SelfUpdateJournal::path_in(service_dir),
        cause: error.to_string(),
    })
}

/// The digest of a file on disk, streamed so a release binary is never held in
/// memory to be checked.
pub fn digest_of_file(path: &Path) -> Result<String, ReplaceError> {
    crate::update::candidate::digest_of(path).map_err(|error| ReplaceError::SwapFailed {
        path: path.to_path_buf(),
        cause: error.to_string(),
    })
}
