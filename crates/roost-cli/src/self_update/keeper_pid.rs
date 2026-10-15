//! Whether the keeper survived an update: the same process, still running.
//!
//! Read the way `cargo xtask fleet install` reads it: the pid the keeper wrote,
//! and whether that process answers `kill -0`. The keeper is not replaced by a
//! self-update, so a changed or dead keeper afterwards means the update took it
//! down with the worker. That is reported; it is not rolled back here.

use std::path::{Path, PathBuf};
use std::process::Command;

use roost_host::EnvSource;
use roost_worker::runtime::boot::{ENV_KEEPER_PID_FILE, KEEPER_PID_NAME};

/// What the keeper's pid file names, and whether that process runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeeperPid {
    /// A running process with this pid.
    Alive(String),
    /// No running process; the recorded contents, or `none` when there are none.
    Dead(String),
}

impl KeeperPid {
    fn label(&self) -> String {
        match self {
            Self::Alive(pid) => pid.clone(),
            Self::Dead(recorded) => format!("dead:{recorded}"),
        }
    }
}

/// Where the keeper records its pid: the worker's override, else the data directory.
pub fn pid_file(env: &dyn EnvSource, data_dir: &Path) -> PathBuf {
    match env.get(ENV_KEEPER_PID_FILE) {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => data_dir.join(KEEPER_PID_NAME),
    }
}

/// Read the keeper's pid file and probe the process it names.
pub fn read(pid_file: &Path) -> KeeperPid {
    let recorded = std::fs::read_to_string(pid_file)
        .map(|contents| contents.trim().to_owned())
        .unwrap_or_default();
    if is_pid(&recorded) && process_answers(&recorded) {
        KeeperPid::Alive(recorded)
    } else if recorded.is_empty() {
        KeeperPid::Dead("none".to_owned())
    } else {
        KeeperPid::Dead(recorded)
    }
}

fn is_pid(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// `kill -0` through the shell, as fleet install probes it: `roost-cli` forbids
/// `unsafe`, so there is no direct `libc::kill` here.
fn process_answers(pid: &str) -> bool {
    Command::new("sh")
        .args(["-c", "kill -0 \"$1\" 2>/dev/null", "sh", pid])
        .status()
        .is_ok_and(|status| status.success())
}

/// `Ok` when the keeper is running and is the same process it was before the update.
pub fn keeper_survived(before: &KeeperPid, after: &KeeperPid) -> Result<(), String> {
    match after {
        KeeperPid::Dead(_) => Err(format!(
            "the keeper did not survive the update: {} before, {} after",
            before.label(),
            after.label()
        )),
        KeeperPid::Alive(_) if before == after => Ok(()),
        KeeperPid::Alive(_) => Err(format!(
            "the keeper was replaced during the update: {} before, {} after",
            before.label(),
            after.label()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alive(pid: &str) -> KeeperPid {
        KeeperPid::Alive(pid.to_owned())
    }

    fn dead(recorded: &str) -> KeeperPid {
        KeeperPid::Dead(recorded.to_owned())
    }

    #[test]
    fn a_running_keeper_with_the_same_pid_survived() {
        assert_eq!(keeper_survived(&alive("41"), &alive("41")), Ok(()));
    }

    #[test]
    fn a_keeper_that_is_no_longer_running_did_not_survive() {
        let error = keeper_survived(&alive("41"), &dead("41")).expect_err("dead keeper refused");
        assert!(error.contains("did not survive"), "{error}");
        assert!(error.contains("dead:41"), "{error}");
    }

    #[test]
    fn a_keeper_with_a_new_pid_was_replaced() {
        let error =
            keeper_survived(&alive("41"), &alive("77")).expect_err("replaced keeper refused");
        assert!(error.contains("replaced"), "{error}");
    }

    #[test]
    fn a_dead_keeper_before_and_after_is_not_a_survivor() {
        assert!(keeper_survived(&dead("none"), &dead("none")).is_err());
    }

    #[test]
    fn a_pid_file_must_hold_only_digits_to_be_probed() {
        assert!(is_pid("4242"));
        assert!(!is_pid(""));
        assert!(!is_pid("4242\n"));
        assert!(!is_pid("12 34"));
        assert!(!is_pid("-1"));
    }

    #[test]
    fn a_missing_pid_file_reads_as_dead_none() {
        let path =
            std::env::temp_dir().join(format!("roost-keeper-pid-missing-{}", std::process::id()));
        assert_eq!(read(&path), dead("none"));
    }

    #[test]
    fn the_running_test_process_reads_as_alive() {
        let path =
            std::env::temp_dir().join(format!("roost-keeper-pid-live-{}", std::process::id()));
        std::fs::write(&path, format!("{}\n", std::process::id())).expect("pid file written");
        let read_back = read(&path);
        std::fs::remove_file(&path).expect("pid file removed");
        assert_eq!(read_back, alive(&std::process::id().to_string()));
    }
}
