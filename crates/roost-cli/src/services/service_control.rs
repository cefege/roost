//! What the deploy transaction needs from a service manager, and the one that
//! drives the machine's real one. A trait rather than a direct call so a deploy
//! can be run end to end — swap, fail, roll back, prove the bytes came back —
//! on a machine with no user manager at all.
//!
//! Activation is asked about, not assumed. Writing a unit is not starting it: a
//! unit systemd refuses to load, and one whose `StandardOutput=` it silently
//! discarded, both leave a file that reads fine and a service that is not
//! serving, so a deploy that trusted the write would report success against a
//! coordinator that was never running.

use std::time::{Duration, Instant};

use roost_host::HostPlatform;

use crate::services::service_argv::{ServiceAction, ServiceCommand, action_commands};
use crate::services::service_spec::ServiceTarget;
use crate::status::service_probe::service_is_loaded;

/// How long a freshly activated service is given to come up. A coordinator
/// opens its database and binds a socket; a worker spawns a keeper and dials.
/// Ten seconds is the bound the boot path itself is held to.
pub const ACTIVATION_DEADLINE: Duration = Duration::from_secs(10);

/// How often a waiting deploy asks whether the service is up.
const ACTIVATION_POLL: Duration = Duration::from_millis(250);

/// The pause between two tries of a command that is retried
/// ([`ServiceCommand::attempts`]).
const RETRY_PAUSE: Duration = Duration::from_secs(1);

/// Why a service manager could not do what it was asked.
#[derive(Debug, thiserror::Error)]
#[error("the {label} service manager could not {action}: {cause}")]
pub struct ServiceControlError {
    /// The service that was addressed.
    pub label: String,
    /// What was asked of it.
    pub action: ServiceAction,
    /// The manager's own answer.
    pub cause: String,
}

/// The three things a deploy needs to ask a service manager. Takes a target
/// rather than a spec because a recovery run has a journal on disk and no
/// resolved spec.
pub trait ServiceManager {
    /// The platform this manager drives.
    fn platform(&self) -> HostPlatform;

    /// Carry out `action`, and report the first command that genuinely failed.
    /// An action this platform does not have is a success, not a failure.
    fn apply(
        &mut self,
        target: &ServiceTarget,
        action: ServiceAction,
    ) -> Result<(), ServiceControlError>;

    /// Whether the service is up, waited for rather than sampled once. A
    /// manager that cannot answer has not proven the service is up, so running
    /// out of time is `false` and the deploy rolls back.
    fn await_active(&mut self, target: &ServiceTarget) -> bool;
}

/// The manager that drives the machine's real one.
#[derive(Debug, Clone)]
pub struct PlatformServiceManager {
    /// The platform whose service manager this drives. Passed in rather than
    /// discovered, so a deploy and the definition it just wrote can never
    /// disagree about which manager they are talking to.
    pub platform: HostPlatform,
    /// How long [`ServiceManager::await_active`] waits.
    pub deadline: Duration,
}

impl PlatformServiceManager {
    pub fn new(platform: HostPlatform) -> Self {
        Self {
            platform,
            deadline: ACTIVATION_DEADLINE,
        }
    }
}

impl ServiceManager for PlatformServiceManager {
    fn platform(&self) -> HostPlatform {
        self.platform
    }

    fn apply(
        &mut self,
        target: &ServiceTarget,
        action: ServiceAction,
    ) -> Result<(), ServiceControlError> {
        run_commands(&action_commands(action, target, self.platform), RETRY_PAUSE).map_err(
            |cause| ServiceControlError {
                label: target.label.clone(),
                action,
                cause,
            },
        )
    }

    fn await_active(&mut self, target: &ServiceTarget) -> bool {
        let started = Instant::now();
        loop {
            if service_is_loaded(&target.label, self.platform) {
                return true;
            }
            if started.elapsed() >= self.deadline {
                return false;
            }
            std::thread::sleep(ACTIVATION_POLL);
        }
    }
}

/// Run `commands` in order: each up to its `attempts`, `pause` apart, and a
/// failure after the last try ends the action unless the command tolerates it.
fn run_commands(commands: &[ServiceCommand], pause: Duration) -> Result<(), String> {
    for command in commands {
        let mut outcome = spawn(&command.argv);
        for _ in 1..command.attempts {
            if outcome.is_ok() {
                break;
            }
            std::thread::sleep(pause);
            outcome = spawn(&command.argv);
        }
        match outcome {
            Ok(()) => {}
            Err(_) if command.tolerate_failure => {}
            Err(cause) => return Err(cause),
        }
    }
    Ok(())
}

fn spawn(argv: &[String]) -> Result<(), String> {
    let (program, arguments) = argv
        .split_first()
        .ok_or_else(|| "an empty command".to_string())?;
    let status = std::process::Command::new(program)
        .args(arguments)
        .stdin(std::process::Stdio::null())
        .status()
        .map_err(|error| format!("{program} could not be run: {error}"))?;
    if status.success() {
        return Ok(());
    }
    match status.code() {
        Some(code) => Err(format!("{program} exited {code}")),
        None => Err(format!("{program} was killed by a signal")),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use super::{ServiceCommand, run_commands};

    fn counter_file(case: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("roost-service-retry-{case}-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    /// How many times the command ran; the counter is removed.
    fn tries(counter: &Path) -> String {
        let text = std::fs::read_to_string(counter).unwrap_or_default();
        let _ = std::fs::remove_file(counter);
        text.trim().to_owned()
    }

    /// A command that fails until it has been run `succeed_on` times, as
    /// launchd's `bootstrap` does while the previous job is still unloading.
    fn flaky(counter: &Path, succeed_on: u32, attempts: u32) -> ServiceCommand {
        let script = format!(
            "n=$(($(cat '{path}' 2>/dev/null || echo 0) + 1)); echo $n > '{path}'; [ $n -ge {succeed_on} ]",
            path = counter.display()
        );
        ServiceCommand {
            argv: vec!["sh".into(), "-c".into(), script],
            tolerate_failure: false,
            attempts,
        }
    }

    #[test]
    fn a_retried_command_succeeds_once_the_manager_accepts_it() {
        let counter = counter_file("accepts");
        let outcome = run_commands(&[flaky(&counter, 3, 5)], Duration::ZERO);
        assert_eq!(outcome, Ok(()));
        assert_eq!(tries(&counter), "3");
    }

    #[test]
    fn a_command_still_refused_after_its_last_try_fails_the_action() {
        let counter = counter_file("refused");
        let outcome = run_commands(&[flaky(&counter, 3, 2)], Duration::ZERO);
        assert_eq!(outcome, Err("sh exited 1".to_string()));
        assert_eq!(tries(&counter), "2");
    }
}
