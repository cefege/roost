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

use crate::services::service_argv::{ServiceAction, action_commands};
use crate::services::service_spec::ServiceTarget;
use crate::status::service_probe::service_is_loaded;

/// How long a freshly activated service is given to come up. A coordinator
/// opens its database and binds a socket; a worker spawns a keeper and dials.
/// Ten seconds is the bound the boot path itself is held to.
pub const ACTIVATION_DEADLINE: Duration = Duration::from_secs(10);

/// How often a waiting deploy asks whether the service is up.
const ACTIVATION_POLL: Duration = Duration::from_millis(250);

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
        for command in action_commands(action, target, self.platform) {
            match spawn(&command.argv) {
                Ok(()) => {}
                Err(_) if command.tolerate_failure => {}
                Err(cause) => {
                    return Err(ServiceControlError {
                        label: target.label.clone(),
                        action,
                        cause,
                    });
                }
            }
        }
        Ok(())
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
