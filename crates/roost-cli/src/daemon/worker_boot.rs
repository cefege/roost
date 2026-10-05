//! Turning `roost worker`'s arguments into a validated `WorkerBoot`, or
//! refusing before anything dials. Called by daemon/mod.rs.
//!
//! `roost-worker` owns the boot's SHAPE and this module owns the refusal
//! point, which is why `WorkerBoot::check` exists at all: the worker crate
//! resolves a boot from an environment for its own tests, and the CLI resolves
//! one from argv plus an environment, and both must be held to the same check
//! rather than each having a looser one of its own.

use roost_host::{EnvSource, ProcessEnv, supported_host_platform};
use roost_worker::agents::environment::SESSION_OVERLAY_ENV_KEYS;
use roost_worker::shell_spec::SESSION_ID_ENV;

use crate::command_error::CommandFailure;
use crate::daemon::WorkerArgs;
use crate::overlay_env::OverlayEnv;

pub fn resolve(args: &WorkerArgs) -> Result<roost_worker::WorkerBoot, CommandFailure> {
    let base = ProcessEnv::new();
    let platform = supported_host_platform()?;
    let boot = resolve_from(args, &base, platform)?;
    boot.check()?;
    Ok(boot)
}

/// The same resolution against a caller-supplied environment, so a test can
/// assert that a flag reaches the loader without a service manager anywhere.
///
/// The five keys a Roost PTY carries are dropped first: a worker started from a
/// shell inside Roost would otherwise read the enclosing session's
/// `ROOST_AGENT_ENDPOINT` as its own socket override and bind the installed
/// worker's agent-report socket.
pub fn resolve_from(
    args: &WorkerArgs,
    env: &dyn EnvSource,
    platform: roost_host::HostPlatform,
) -> Result<roost_worker::WorkerBoot, CommandFailure> {
    if env.get(SESSION_ID_ENV).is_some() {
        tracing::info!(
            target: "boot",
            "ignoring the agent-report variables of the enclosing Roost session"
        );
    }
    let overlaid = SESSION_OVERLAY_ENV_KEYS
        .iter()
        .fold(OverlayEnv::new(env), |overlay, key| overlay.without(key))
        .with(
            roost_worker::runtime::boot::ENV_COORDINATOR_URL,
            args.coordinator_url.as_deref(),
        );
    Ok(roost_worker::WorkerBoot::resolve(&overlaid, platform)?)
}
