//! The service-manager command lines, as ARGV and never as a shell string. A
//! label can come from the environment, and a label in a shell string is an
//! injection; a label as an argv element is just an argument.
//!
//! This is the writer's half of the vocabulary. `status/service_probe.rs` asks
//! whether a service is loaded and derives the unit name from the label through
//! the same [`crate::status::service_probe::systemd_unit_name`] this module
//! uses, so the installer and the readout cannot drift on what a unit is
//! called.

use roost_host::HostPlatform;

use crate::services::scheduled_task::{powershell_argv, register_and_start_script, stop_script};
use crate::services::service_spec::ServiceTarget;
use crate::status::service_probe::current_uid;

/// What a caller asks a service manager to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceAction {
    /// Re-read the definitions on disk. systemd needs this before a restart
    /// picks up an edited unit; launchd reads a plist only at bootstrap, so
    /// for it there is nothing to do here.
    Reload,
    /// Start a service that is not running.
    Start,
    /// Stop a service and start it again on the definition just written.
    Restart,
    /// Stop a service and leave it stopped.
    Stop,
}

impl ServiceAction {
    /// The name a log line and an error message use.
    pub const fn display_name(self) -> &'static str {
        match self {
            ServiceAction::Reload => "reload",
            ServiceAction::Start => "start",
            ServiceAction::Restart => "restart",
            ServiceAction::Stop => "stop",
        }
    }
}

impl std::fmt::Display for ServiceAction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.display_name())
    }
}

/// One command, and whether failing it is a failure of the action.
///
/// launchd answers "no such job" for a `bootout` of something that was never
/// loaded, and for a `kickstart` of a job that has not finished starting. Both
/// are the normal state of a first install and neither means the deploy failed,
/// so tolerance is a property of the command rather than a `|| true` welded
/// into a shell string — which is the only reason the shell strings had one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceCommand {
    /// The program and its arguments.
    pub argv: Vec<String>,
    /// Whether a non-zero exit is an answer rather than a failure.
    pub tolerate_failure: bool,
    /// How many times the command is run before its failure counts, one second
    /// apart. Only launchd's `bootstrap` needs more than one: see
    /// [`launchd_commands`].
    pub attempts: u32,
}

/// launchd unloads a booted-out job asynchronously and answers `bootstrap`
/// with "5: Input/output error" until it is gone; a worker with live channels
/// takes a few seconds. Fifteen one-second tries cover it with room to spare,
/// and a definition launchd truly refuses still fails, fifteen seconds later.
pub const BOOTSTRAP_ATTEMPTS: u32 = 15;

impl ServiceCommand {
    fn required(argv: &[&str]) -> Self {
        Self {
            argv: argv
                .iter()
                .map(|argument| (*argument).to_string())
                .collect(),
            tolerate_failure: false,
            attempts: 1,
        }
    }

    fn tolerated(argv: &[&str]) -> Self {
        Self {
            tolerate_failure: true,
            ..Self::required(argv)
        }
    }

    fn retried(argv: &[&str], attempts: u32) -> Self {
        Self {
            attempts,
            ..Self::required(argv)
        }
    }
}

/// Every command one action is made of, in the order they must run. An empty
/// result is an action this platform does not have, not a failure.
pub fn action_commands(
    action: ServiceAction,
    target: &ServiceTarget,
    platform: HostPlatform,
) -> Vec<ServiceCommand> {
    match platform {
        HostPlatform::Linux => systemd_commands(action, target),
        HostPlatform::MacOs => launchd_commands(action, target),
        HostPlatform::Windows => scheduled_task_commands(action, target),
    }
}

fn systemd_commands(action: ServiceAction, target: &ServiceTarget) -> Vec<ServiceCommand> {
    if action == ServiceAction::Reload {
        return vec![ServiceCommand::required(&[
            "systemctl",
            "--user",
            "daemon-reload",
        ])];
    }
    let verb = match action {
        ServiceAction::Reload => return Vec::new(),
        ServiceAction::Start => "start",
        ServiceAction::Restart => "restart",
        ServiceAction::Stop => "stop",
    };
    let unit = crate::status::service_probe::systemd_unit_name(&target.label);
    let act = ServiceCommand::required(&["systemctl", "--user", verb, unit.as_str()]);
    if action == ServiceAction::Stop {
        return vec![act];
    }
    // A start or restart runs the definition just written; without the reload
    // systemd keeps the unit it read before and restarts the old release.
    vec![
        ServiceCommand::required(&["systemctl", "--user", "daemon-reload"]),
        act,
    ]
}

/// launchd has no reload: an agent is read once, at bootstrap. A restart is
/// therefore bootout, bootstrap, enable, kickstart. launchd refuses
/// `bootstrap` while the prior job is still unloading and reports that as an
/// input/output error rather than as "not yet", so `bootstrap` is retried
/// ([`BOOTSTRAP_ATTEMPTS`]); run once, it fails every restart of a running
/// worker and the rollback's restart with it.
fn launchd_commands(action: ServiceAction, target: &ServiceTarget) -> Vec<ServiceCommand> {
    let job = format!("gui/{}/{}", current_uid(), target.label);
    let bootout = ServiceCommand::tolerated(&["launchctl", "bootout", job.as_str()]);
    if matches!(action, ServiceAction::Reload) {
        return Vec::new();
    }
    if action == ServiceAction::Stop {
        return vec![bootout];
    }
    let domain = format!("gui/{}", current_uid());
    vec![
        bootout,
        ServiceCommand::retried(
            &[
                "launchctl",
                "bootstrap",
                &domain,
                &target.definition_path.display().to_string(),
            ],
            BOOTSTRAP_ATTEMPTS,
        ),
        ServiceCommand::required(&["launchctl", "enable", job.as_str()]),
        ServiceCommand::tolerated(&["launchctl", "kickstart", "-k", job.as_str()]),
    ]
}

/// Task Scheduler reads the launcher afresh on every start, so there is no
/// reload. A start registers the task (replacing any older one) and runs it; a
/// stop that finds nothing running is the normal state of a first install.
fn scheduled_task_commands(action: ServiceAction, target: &ServiceTarget) -> Vec<ServiceCommand> {
    let as_command = |script: String, tolerate_failure: bool| ServiceCommand {
        argv: powershell_argv(&script),
        tolerate_failure,
        attempts: 1,
    };
    let start = || {
        as_command(
            register_and_start_script(&target.label, &target.definition_path),
            false,
        )
    };
    let stop = || as_command(stop_script(&target.label), true);
    match action {
        ServiceAction::Reload => Vec::new(),
        ServiceAction::Start => vec![start()],
        ServiceAction::Restart => vec![stop(), start()],
        ServiceAction::Stop => vec![stop()],
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use roost_host::HostPlatform;

    use super::{ServiceAction, action_commands};
    use crate::services::service_spec::ServiceTarget;

    fn target() -> ServiceTarget {
        ServiceTarget {
            label: "roost3-coord".to_string(),
            definition_path: PathBuf::from("/home/u/.config/systemd/user/roost3-coord.service"),
        }
    }

    #[test]
    fn a_systemd_restart_reloads_the_unit_it_just_wrote_first() {
        let argv: Vec<Vec<String>> =
            action_commands(ServiceAction::Restart, &target(), HostPlatform::Linux)
                .into_iter()
                .map(|command| command.argv)
                .collect();
        assert_eq!(
            argv,
            vec![
                vec!["systemctl", "--user", "daemon-reload"],
                vec!["systemctl", "--user", "restart", "roost3-coord.service"],
            ]
        );
        let stop = action_commands(ServiceAction::Stop, &target(), HostPlatform::Linux);
        assert_eq!(
            stop.into_iter()
                .map(|command| command.argv)
                .collect::<Vec<_>>(),
            vec![vec!["systemctl", "--user", "stop", "roost3-coord.service"]]
        );
    }

    #[test]
    fn launchd_reload_is_nothing_because_an_agent_is_read_at_bootstrap() {
        assert!(action_commands(ServiceAction::Reload, &target(), HostPlatform::MacOs).is_empty());
    }

    #[test]
    fn a_job_that_was_never_loaded_does_not_fail_a_launchd_restart() {
        let commands = action_commands(ServiceAction::Restart, &target(), HostPlatform::MacOs);
        let tolerated: Vec<bool> = commands
            .iter()
            .map(|command| command.tolerate_failure)
            .collect();
        assert_eq!(
            tolerated,
            vec![true, false, false, true],
            "bootout of an absent job and kickstart of a starting job are answers, not failures"
        );
    }

    #[test]
    fn a_hostile_label_stays_inside_a_launchd_job_spec() {
        let hostile = ServiceTarget {
            label: "x; rm -rf /".to_string(),
            ..target()
        };
        let argv: Vec<String> =
            action_commands(ServiceAction::Restart, &hostile, HostPlatform::MacOs)
                .into_iter()
                .flat_map(|command| command.argv)
                .collect();
        // launchd addresses a job as `gui/<uid>/<label>`, so the label is one
        // element of a compound spec — never a word of its own, which is what
        // would let it be read as a second command.
        for argument in argv.iter().filter(|argument| argument.contains("rm -rf")) {
            assert!(
                argument.starts_with("gui/") && argument.ends_with(&hostile.label),
                "the label escaped its job spec: {argument}"
            );
        }
        assert!(
            argv.iter().all(|argument| {
                argument == "launchctl"
                    || argument == "bootout"
                    || argument == "bootstrap"
                    || argument == "enable"
                    || argument == "kickstart"
                    || argument == "-k"
                    || argument.starts_with("gui/")
                    || *argument == target().definition_path.display().to_string()
            }),
            "an unexpected argument reached the command line: {argv:?}"
        );
    }
}
