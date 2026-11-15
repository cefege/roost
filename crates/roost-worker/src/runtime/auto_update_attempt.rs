//! The record of the last self-update this worker started, and the rule that
//! the record is written before the updater is spawned.
//!
//! Called by [`super::auto_update`]'s poller; the spawn itself is injected, so
//! the ordering is testable without a service manager.

use std::path::Path;

use serde::{Deserialize, Serialize};

const ATTEMPT_FILE: &str = "self-update-attempt.json";

/// When this worker last started an updater, persisted so a restart of the
/// worker does not start a second one inside the retry window. Files written
/// with the older `tag` and `unit` fields still read: unknown fields are ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    pub at_ms: u64,
}

/// How one start of the updater ended.
#[derive(Debug)]
pub(super) enum StartOutcome {
    Started,
    /// The attempt could not be written, so no updater was started.
    NotRecorded(std::io::Error),
    /// The attempt is recorded and the updater did not start.
    SpawnFailed(String),
}

/// Record the attempt, then start the updater. The record comes first so a
/// worker the updater restarts, or one that crashes right after the spawn, finds
/// the attempt on disk; if the record cannot be written nothing is started,
/// because an unrecorded start could repeat on every restart.
pub(super) fn start_after_recording(
    data_dir: &Path,
    attempt: &Attempt,
    spawn: impl FnOnce() -> Result<(), String>,
) -> StartOutcome {
    if let Err(error) = write_attempt(data_dir, attempt) {
        return StartOutcome::NotRecorded(error);
    }
    match spawn() {
        Ok(()) => StartOutcome::Started,
        Err(error) => StartOutcome::SpawnFailed(error),
    }
}

pub(super) fn read_attempt(data_dir: &Path) -> Option<Attempt> {
    let text = std::fs::read_to_string(data_dir.join(ATTEMPT_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_attempt(data_dir: &Path, attempt: &Attempt) -> std::io::Result<()> {
    let text = serde_json::to_string(attempt).map_err(std::io::Error::other)?;
    let temporary = data_dir.join(format!("{ATTEMPT_FILE}.tmp"));
    std::fs::write(&temporary, text)?;
    std::fs::rename(temporary, data_dir.join(ATTEMPT_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attempt(at_ms: u64) -> Attempt {
        Attempt { at_ms }
    }

    #[test]
    fn an_attempt_file_written_with_tag_and_unit_still_reads() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join(ATTEMPT_FILE),
            r#"{"tag":"v3.0.0-rc.37","at_ms":1234,"unit":"roost-self-update-x"}"#,
        )
        .unwrap();
        assert_eq!(read_attempt(directory.path()), Some(attempt(1234)));
    }

    #[test]
    fn the_attempt_is_on_disk_before_the_updater_starts() {
        let directory = tempfile::tempdir().unwrap();
        let mut recorded_when_spawned = None;
        let outcome = start_after_recording(directory.path(), &attempt(5_000), || {
            recorded_when_spawned = Some(read_attempt(directory.path()));
            Ok(())
        });
        assert!(matches!(outcome, StartOutcome::Started));
        assert_eq!(recorded_when_spawned, Some(Some(attempt(5_000))));
    }

    #[test]
    fn an_attempt_that_cannot_be_recorded_does_not_start_the_updater() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("absent");
        let mut spawned = false;
        let outcome = start_after_recording(&missing, &attempt(5_000), || {
            spawned = true;
            Ok(())
        });
        assert!(matches!(outcome, StartOutcome::NotRecorded(_)));
        assert!(!spawned, "an unrecorded attempt must never spawn");
    }

    #[test]
    fn a_failed_spawn_leaves_the_attempt_recorded() {
        let directory = tempfile::tempdir().unwrap();
        let outcome = start_after_recording(directory.path(), &attempt(5_000), || {
            Err("no systemd".to_owned())
        });
        assert!(matches!(outcome, StartOutcome::SpawnFailed(_)));
        assert_eq!(read_attempt(directory.path()), Some(attempt(5_000)));
    }
}
