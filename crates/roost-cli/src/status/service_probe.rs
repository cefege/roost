//! Is this host's coordinator/worker service actually loaded by its platform's
//! service manager? Called by status/collect.rs, and by nothing else — every
//! other service question the CLI asks goes through the deploy group's own
//! service-control vocabulary, because a second spelling of `systemctl --user
//! is-active roost3-coord.service` is a second thing to keep in sync.
//!
//! The probe is a subprocess with a deadline, never a library call, because
//! `systemctl --user` talks to a user manager over a socket and `launchctl
//! print` to launchd: both can block indefinitely on a wedged manager, and
//! `roost status` is a one-shot readout an operator waits on. A probe that
//! hangs is worse than a probe that answers "not loaded".

use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use roost_host::HostPlatform;

/// A wedged service manager must not hold the readout open. Five seconds is
/// the deadline the TypeScript probe used; below it a loaded-but-busy manager
/// reads as down, above it an operator is left watching a cursor.
pub const PROBE_DEADLINE: Duration = Duration::from_secs(5);

/// The unit name systemd must be asked about. systemd identifies a unit by the
/// file name it was installed under, and `roost-host` installs
/// `{label}.service`, so deriving the name from the label keeps the probe and
/// the installer from drifting apart when a label is overridden.
pub fn systemd_unit_name(label: &str) -> String {
    format!("{label}.service")
}

pub fn service_is_loaded(label: &str, platform: HostPlatform) -> bool {
    probe_exit_code(&probe_command(label, platform)) == Some(0)
}

fn probe_command(label: &str, platform: HostPlatform) -> Vec<String> {
    match platform {
        HostPlatform::Linux => vec![
            "systemctl".to_string(),
            "--user".to_string(),
            "is-active".to_string(),
            systemd_unit_name(label),
        ],
        HostPlatform::MacOs => vec![
            "launchctl".to_string(),
            "print".to_string(),
            format!("gui/{}/{}", current_uid(), label),
        ],
        // v3 ships Linux and macOS only and refuses a Windows host before any
        // command runs, so there is no Windows service manager to ask.
        HostPlatform::Windows => Vec::new(),
    }
}

/// The command that makes the service manager REPORT its state, rather than
/// answer whether the unit exists.
///
/// This exists because the two are not the same question and only one of them
/// is honest. `systemctl show` exits 0 for a unit it has never heard of, and
/// `launchctl print` fails identically for a job that was never loaded and for
/// a launchd this command cannot reach — so on their own both read as "not
/// running" when the truth is "cannot tell". A deploy's staging decision turns
/// on that difference, so the reachability question is asked separately and the
/// two answers are never collapsed.
pub fn service_state_command(label: &str, platform: HostPlatform) -> Vec<String> {
    match platform {
        HostPlatform::Linux => vec![
            "systemctl".to_string(),
            "--user".to_string(),
            "show".to_string(),
            systemd_unit_name(label),
        ],
        HostPlatform::MacOs => vec![
            "launchctl".to_string(),
            "print".to_string(),
            format!("gui/{}/{}", current_uid(), label),
        ],
        HostPlatform::Windows => Vec::new(),
    }
}

/// The launchd domain query that answers whether launchd was reachable at all.
/// It succeeds whether or not the job is loaded, which is the whole point: it
/// separates "unloaded" from "no launchd here".
pub fn launchd_domain_query() -> Vec<String> {
    vec![
        "launchctl".to_string(),
        "print-disabled".to_string(),
        format!("gui/{}", current_uid()),
    ]
}

/// Whether a service manager's report says the service is running with a live
/// main process.
///
/// A pid is required as well as a state, and not as a formality: a service
/// manager that reports `active` for a job whose process exited leaves a
/// definition that reads correct and a machine with no worker behind it, and
/// `roost push` in v2 rolled a whole fleet back over exactly that.
pub fn service_is_running(report: &str, platform: HostPlatform) -> bool {
    let line = |name: &str| -> Option<String> {
        report.lines().find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == name).then(|| value.trim().to_string())
        })
    };
    match platform {
        HostPlatform::Linux => {
            line("ActiveState").as_deref() == Some("active")
                && line("SubState").as_deref() == Some("running")
                && line("MainPID").is_some_and(|pid| {
                    !pid.is_empty() && pid != "0" && pid.bytes().all(|byte| byte.is_ascii_digit())
                })
        }
        HostPlatform::MacOs => {
            let field = |name: &str| -> Option<String> {
                report.lines().find_map(|line| {
                    let (key, value) = line.split_once('=')?;
                    (key.trim() == name).then(|| value.trim().to_string())
                })
            };
            field("state").as_deref() == Some("running")
                && field("active count").as_deref() == Some("1")
                && field("pid").is_some_and(|pid| {
                    !pid.is_empty() && pid != "0" && pid.bytes().all(|byte| byte.is_ascii_digit())
                })
        }
        HostPlatform::Windows => false,
    }
}

/// The uid a launchd per-user domain is addressed by, shared with the install
/// paths in `crate::services::service_argv` so a job is bootstrapped into the
/// same domain it is probed in. `libc::getuid` is an `unsafe` call and this
/// crate forbids `unsafe`, so the uid is read from a file the account owns
/// instead. Every account has a home directory, and its owner is the account
/// running this command.
pub fn current_uid() -> u32 {
    use std::os::unix::fs::MetadataExt;
    // `libc::getuid` is an `unsafe` call and this crate forbids `unsafe`, so
    // the uid is read from a file the account owns instead. Every account has a
    // home directory, and its owner is the account running this command.
    std::env::var("HOME")
        .ok()
        .and_then(|home| std::fs::metadata(home).ok())
        .map(|metadata| metadata.uid())
        .unwrap_or(0)
}

/// The child's exit code, or `None` when it could not be run or did not finish
/// inside the deadline. A timed-out probe reports failure rather than success:
/// a manager that cannot answer has not proven the service is loaded.
fn probe_exit_code(argv: &[String]) -> Option<i32> {
    let (program, arguments) = argv.split_first()?;
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let pid = child.id();
    let (sender, receiver) = mpsc::channel();
    // The wait moves onto a thread so the deadline is a channel timeout rather
    // than a second process; the thread ends when the child does, and the
    // timeout path kills the child by pid so a wedged manager does not outlive
    // the command that asked it a question.
    std::thread::spawn(move || {
        let _ = sender.send(child.wait());
    });
    match receiver.recv_timeout(PROBE_DEADLINE) {
        Ok(Ok(status)) => status.code(),
        Ok(Err(_)) => None,
        Err(_) => {
            kill(pid);
            None
        }
    }
}

fn kill(pid: u32) {
    // `Child::kill` needs the handle the waiting thread owns, so the pid is the
    // handle. A pid that has already exited makes this a no-op the kernel
    // ignores, which is the common case here.
    let _ = Command::new("kill")
        .arg("-KILL")
        .arg(pid.to_string())
        .status();
}

#[cfg(test)]
mod tests {
    use super::{probe_exit_code, systemd_unit_name};

    #[test]
    fn a_unit_is_asked_for_by_the_file_name_it_was_installed_under() {
        assert_eq!(systemd_unit_name("roost3-coord"), "roost3-coord.service");
    }

    #[test]
    fn a_program_that_does_not_exist_is_not_a_loaded_service() {
        assert_eq!(probe_exit_code(&["roost-no-such-probe".to_string()]), None);
    }

    #[test]
    fn a_program_that_exits_zero_reports_its_code() {
        assert_eq!(
            probe_exit_code(&[
                "/bin/sh".to_string(),
                "-c".to_string(),
                "exit 0".to_string()
            ]),
            Some(0)
        );
    }
}
