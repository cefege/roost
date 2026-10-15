//! Whether the keeper survived an update: the same process, still running.
//!
//! Read the way `cargo xtask fleet install` reads it: the pid the keeper wrote,
//! and whether that process answers `kill -0`. The keeper is not replaced by a
//! self-update, so a changed or dead keeper afterwards means the update took it
//! down with the worker. That is reported; it is not rolled back here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use roost_host::{EnvSource, HostPlatform};
use roost_worker::runtime::boot::keeper_pid_path;

use crate::overlay_env::OverlayEnv;

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

/// Where the installed keeper records its pid. The installed definition declares
/// the worker's overrides, so its values are layered over the live environment
/// (which supplies `HOME` and anything the definition omits), and the boot's own
/// rule resolves the path from that.
pub fn pid_file(
    live: &dyn EnvSource,
    installed: &BTreeMap<String, String>,
    platform: HostPlatform,
) -> Result<PathBuf, String> {
    let mut overlay = OverlayEnv::new(live);
    for (name, value) in installed {
        overlay = overlay.with(name, Some(value.as_str()));
    }
    keeper_pid_path(&overlay, platform).map_err(|error| error.to_string())
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
    !text.is_empty()
        && text.bytes().all(|byte| byte.is_ascii_digit())
        && text.parse::<u32>().is_ok_and(|pid| pid > 0)
}

/// `kill -0` through the shell, as fleet install probes it: `roost-cli` forbids
/// `unsafe`, so there is no direct `libc::kill` here.
fn process_answers(pid: &str) -> bool {
    Command::new("sh")
        .args(["-c", "kill -0 \"$1\" 2>/dev/null", "sh", pid])
        .status()
        .is_ok_and(|status| status.success())
}

/// `Ok` when the keeper was not running before the update (a fresh keeper started
/// by the boot is a pass), or when the same running keeper is still running after.
pub fn keeper_survived(before: &KeeperPid, after: &KeeperPid) -> Result<(), String> {
    match (before, after) {
        (KeeperPid::Dead(_), _) => Ok(()),
        (KeeperPid::Alive(_), KeeperPid::Dead(_)) => Err(format!(
            "the keeper did not survive the update: {} before, {} after",
            before.label(),
            after.label()
        )),
        (KeeperPid::Alive(_), KeeperPid::Alive(_)) if before == after => Ok(()),
        (KeeperPid::Alive(_), KeeperPid::Alive(_)) => Err(format!(
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
    fn a_fresh_keeper_after_no_keeper_is_a_pass() {
        assert_eq!(keeper_survived(&dead("none"), &alive("77")), Ok(()));
        assert_eq!(keeper_survived(&dead("none"), &dead("none")), Ok(()));
    }

    #[test]
    fn a_pid_of_zero_names_no_process() {
        assert!(!is_pid("0"));
        assert!(!is_pid("00"));
        assert!(is_pid("1"));
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
    fn the_installed_definition_names_the_pid_file_over_the_live_environment() {
        let live = roost_host::MapEnv::new()
            .with(roost_host::HOME_ENV, "/home/mike")
            .with("ROOST_KEEPER_PID_FILE", "/live/keeper.pid");
        let installed = BTreeMap::from([(
            "ROOST_KEEPER_PID_FILE".to_owned(),
            "/def/keeper.pid".to_owned(),
        )]);
        assert_eq!(
            pid_file(&live, &installed, HostPlatform::Linux),
            Ok(PathBuf::from("/def/keeper.pid"))
        );
    }

    #[test]
    fn an_installed_data_dir_places_the_pid_file_inside_it() {
        let installed =
            BTreeMap::from([("ROOST_WORKER_DATA_DIR".to_owned(), "/def/data".to_owned())]);
        assert_eq!(
            pid_file(&roost_host::MapEnv::new(), &installed, HostPlatform::Linux),
            Ok(PathBuf::from("/def/data").join(roost_worker::runtime::boot::KEEPER_PID_NAME))
        );
    }

    #[test]
    fn with_no_definition_the_live_environment_decides() {
        let live = roost_host::MapEnv::new().with("ROOST_WORKER_DATA_DIR", "/live/data");
        assert_eq!(
            pid_file(&live, &BTreeMap::new(), HostPlatform::Linux),
            Ok(PathBuf::from("/live/data").join(roost_worker::runtime::boot::KEEPER_PID_NAME))
        );
    }

    #[test]
    fn a_missing_pid_file_reads_as_dead_none() {
        let path =
            std::env::temp_dir().join(format!("roost-keeper-pid-missing-{}", std::process::id()));
        assert_eq!(read(&path), dead("none"));
    }

    #[cfg(unix)]
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
