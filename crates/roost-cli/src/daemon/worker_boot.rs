//! Turning `roost worker`'s arguments into a validated `WorkerBoot`, or
//! refusing before anything dials. Called by daemon/mod.rs.
//!
//! `roost-worker` owns the boot's SHAPE and this module owns the refusal
//! point, which is why `WorkerBoot::check` exists at all: the worker crate
//! resolves a boot from an environment for its own tests, and the CLI resolves
//! one from argv plus an environment, and both must be held to the same check
//! rather than each having a looser one of its own.

use roost_host::{EnvSource, ProcessEnv, supported_host_platform};

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
pub fn resolve_from(
    args: &WorkerArgs,
    env: &dyn EnvSource,
    platform: roost_host::HostPlatform,
) -> Result<roost_worker::WorkerBoot, CommandFailure> {
    let overlaid = OverlayEnv::new(env).with(
        roost_worker::runtime::boot::ENV_COORDINATOR_URL,
        args.coordinator_url.as_deref(),
    );
    let boot = roost_worker::WorkerBoot::resolve(&overlaid, platform)?;
    #[cfg(feature = "smoke")]
    let boot = with_fault_sockets(boot, args, env)?;
    Ok(boot)
}

/// The smoke harness's fault sockets, when both flags were given. The harness
/// addresses each worker by `ROOST_WORKER_LABEL`, so a worker without one
/// could never be reached and is refused before it boots.
#[cfg(feature = "smoke")]
fn with_fault_sockets(
    mut boot: roost_worker::WorkerBoot,
    args: &WorkerArgs,
    env: &dyn EnvSource,
) -> Result<roost_worker::WorkerBoot, CommandFailure> {
    let (Some(peer_fault_socket), Some(input_hold_socket)) = (
        args.terminal_peer_fault_socket.clone(),
        args.direct_input_hold_socket.clone(),
    ) else {
        return Ok(boot);
    };
    let label_env = roost_worker::runtime::bootstrap_redeem::ENV_WORKER_LABEL;
    let worker_label = env
        .get(label_env)
        .filter(|label| !label.is_empty())
        .ok_or_else(|| {
            CommandFailure::usage(format!(
                "terminal peer fault worker has no label: {label_env} is unset"
            ))
        })?;
    boot.fault_sockets = Some(roost_worker::smoke_faults::FaultSockets {
        peer_fault_socket,
        input_hold_socket,
        worker_label,
    });
    Ok(boot)
}
