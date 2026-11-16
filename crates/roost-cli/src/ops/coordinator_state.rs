//! Whether the local coordinator service is running; `db-to-postgres` and
//! `db-to-sqlite` refuse to copy a live database. Called by those two commands;
//! depends on the service-manager probes in `services` and `status`.

use roost_host::HostPlatform;

use crate::services::scheduled_task::{powershell_argv, running_probe_script};
use crate::status::service_probe::current_uid;

/// What the service manager said about this install's coordinator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoordinatorState {
    /// The unit is loaded and running. The database is being written to.
    Running,
    /// The unit is not running: stopped, not installed, or never loaded.
    Stopped,
    /// The service manager could not be asked, which is not the same as stopped.
    Unavailable,
}

/// The argv that asks the platform's service manager whether the coordinator
/// is running.
///
/// argv rather than a shell string for the reason every other service call in
/// this crate is argv: a unit name can come from the environment, and a shell
/// string is a quoting bug waiting for a name with a space in it.
#[must_use]
pub fn coordinator_probe_argv(platform: HostPlatform, label: &str) -> Vec<String> {
    match platform {
        HostPlatform::Linux => vec![
            "systemctl".to_string(),
            "--user".to_string(),
            "is-active".to_string(),
            label.to_string(),
        ],
        HostPlatform::MacOs => vec![
            "launchctl".to_string(),
            "print".to_string(),
            format!("gui/{}/{}", current_uid(), label),
        ],
        HostPlatform::Windows => powershell_argv(&running_probe_script(label)),
    }
}

/// Read the service manager's answer, given a probe that runs one argv.
pub fn coordinator_state_with(
    platform: HostPlatform,
    label: &str,
    mut probe: impl FnMut(&[String]) -> Option<String>,
) -> CoordinatorState {
    let argv = coordinator_probe_argv(platform, label);
    if argv.is_empty() {
        return CoordinatorState::Unavailable;
    }
    let Some(answer) = probe(&argv) else {
        return CoordinatorState::Unavailable;
    };
    // systemd's `is-active` exits non-zero and prints `inactive` for a unit
    // that is not running, and exits non-zero with no useful word for a unit
    // it has never heard of. Only an explicit `active` is a running
    // coordinator; everything else is a database nobody is writing to.
    if answer.trim() == "active" {
        CoordinatorState::Running
    } else {
        CoordinatorState::Stopped
    }
}

/// Ask the real service manager.
pub fn coordinator_state(platform: HostPlatform, label: &str) -> CoordinatorState {
    coordinator_state_with(platform, label, |argv| {
        let output = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .output()
            .ok()?;
        // A manager that reported failure has not said `active`, and the one
        // case that matters — a unit it does not know — is a failure; either
        // way the stdout is the answer.
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    })
}
