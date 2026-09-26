//! Turning `roost coord`'s arguments into a validated `CoordBoot`, or refusing
//! before a socket is bound. Called by daemon/mod.rs. Depends on `roost-host`
//! for the whole of what "valid" means — this module contributes the argv
//! overlay and nothing else, because a second opinion about a bind rule is how
//! a coordinator ends up listening somewhere the installer never intended.
//!
//! The refusal order matters and is `roost-host`'s: the Cloudflare Access pair
//! is checked before anything is normalised, because half a pair is a silently
//! unauthenticated coordinator rather than a boot failure.

use roost_host::coord_config_loader::{ENV_COORDINATOR_BIND, ENV_COORDINATOR_DB};
use roost_host::{EnvSource, ProcessEnv, supported_host_platform};

use crate::command_error::CommandFailure;
use crate::daemon::CoordArgs;
use crate::overlay_env::OverlayEnv;

pub fn resolve(args: &CoordArgs) -> Result<roost_coord::CoordBoot, CommandFailure> {
    let base = ProcessEnv::new();
    let platform = supported_host_platform()?;
    let boot = resolve_from(args, &base, platform)?;
    Ok(boot)
}

/// The same resolution against a caller-supplied environment, so the flag path
/// can be asserted without a service manager, a database, or a socket.
pub fn resolve_from(
    args: &CoordArgs,
    env: &dyn EnvSource,
    platform: roost_host::HostPlatform,
) -> Result<roost_coord::CoordBoot, CommandFailure> {
    let overlaid = OverlayEnv::new(env)
        .with(ENV_COORDINATOR_BIND, args.bind.as_deref())
        .with(ENV_COORDINATOR_DB, args.db.as_deref());
    let config = roost_host::load_coord_config(&overlaid, platform)?;
    Ok(roost_coord::CoordBoot { config, platform })
}
