//! Platform-aware worker, coordinator, version, and service paths.
//!
//! Every default here is the v3 generation's own, because v3 installs beside an
//! older one on the same machine: a shared data directory, database, unit, or
//! port is two releases writing the same file. The names are collected in one
//! place below so a reviewer can check the whole set rather than grep for
//! suffixes.
//!
//! Nothing here reads the environment or the clock. Both arrive as arguments
//! through `EnvSource`, which is what makes every default testable off-host.

use std::path::PathBuf;

use roost_platform::HostPlatform;
use roost_protocol::{ProtocolError, ProtocolResult};

use crate::env::{EnvSource, HOME_ENV, XDG_DATA_HOME_ENV, XDG_STATE_HOME_ENV};

/// The launchd label the coordinator installs under on macOS.
pub const COORD_LABEL_DARWIN: &str = "com.roost.coordinator-v3";

/// The systemd user unit the coordinator installs under on Linux.
pub const COORD_LABEL_LINUX: &str = "roost3-coord";

/// The launchd label the worker installs under on macOS.
pub const WORKER_LABEL_DARWIN: &str = "com.roost.worker-v3";

/// The systemd user unit the worker installs under on Linux.
pub const WORKER_LABEL_LINUX: &str = "roost3-worker";

/// The worker data directory name under the macOS and Linux data roots.
pub const WORKER_DATA_DIR_NAME: &str = "RoostWorkerV3";

/// The coordinator data directory name under the macOS and Linux data roots.
pub const COORD_DATA_DIR_NAME: &str = "RoostCoordinatorV3";

/// The worker log directory name under the macOS and Linux state roots.
pub const WORKER_LOG_DIR_NAME: &str = "RoostWorkerV3";

/// The coordinator log directory name under the macOS and Linux state roots.
pub const COORD_LOG_DIR_NAME: &str = "RoostCoordV3";

/// Overrides [`worker_data_dir`].
pub const WORKER_DATA_DIR_ENV: &str = "ROOST_WORKER_DATA_DIR";

/// Overrides [`worker_log_dir`].
pub const WORKER_LOG_DIR_ENV: &str = "ROOST_WORKER_LOG_DIR";

/// Overrides [`coord_data_dir`].
pub const COORD_DATA_DIR_ENV: &str = "ROOST_COORD_DATA_DIR";

/// Overrides [`coord_log_dir`].
pub const COORD_LOG_DIR_ENV: &str = "ROOST_COORD_LOG_DIR";

/// Overrides [`roost_service_dir`].
pub const SERVICE_DIR_ENV: &str = "ROOST_SERVICE_DIR";

/// Overrides [`roost_versions_dir`].
pub const VERSIONS_DIR_ENV: &str = "ROOST_VERSIONS_DIR";

/// Overrides [`coord_service_label`] and names the coordinator's unit file.
pub const COORD_LABEL_ENV: &str = "ROOST_COORD_LABEL";

/// Overrides [`worker_service_label`] and names the worker's unit file.
///
/// Deliberately not `ROOST_WORKER_LABEL`: that is the variable the
/// machine-enrollment command emits to name the machine to the rest of the
/// fleet, a different thing for a different consumer. Renaming this one would
/// silently drop an operator's existing override and start a second unit.
pub const WORKER_LABEL_ENV: &str = "ROOST_WORKER_AGENT_LABEL";

/// Names the coordinator's macOS launchd definition outright.
pub const COORD_PLIST_ENV: &str = "ROOST_COORD_PLIST";

/// Names the coordinator's Linux systemd unit definition outright.
pub const COORD_UNIT_ENV: &str = "ROOST_COORD_UNIT";

/// Names the worker's macOS launchd definition outright.
pub const WORKER_PLIST_ENV: &str = "ROOST_WORKER_PLIST";

/// Names the worker's Linux systemd unit definition outright.
pub const WORKER_UNIT_ENV: &str = "ROOST_WORKER_UNIT";

/// The subdirectory of the service directory holding installed definitions.
pub const SERVICE_DIR_SUBDIR: &str = "service";

/// The subdirectory of the service directory holding released versions.
pub const VERSIONS_DIR_SUBDIR: &str = "versions";

/// The XDG default for data, when `XDG_DATA_HOME` is unset.
const XDG_DATA_DEFAULT_LEAF: &str = "share";

/// The XDG default for mutable state, when `XDG_STATE_HOME` is unset.
const XDG_STATE_DEFAULT_LEAF: &str = "state";

/// Where the worker keeps its durable state.
pub fn worker_data_dir(env: &dyn EnvSource, platform: HostPlatform) -> ProtocolResult<PathBuf> {
    if let Some(override_dir) = non_empty(env, WORKER_DATA_DIR_ENV) {
        return Ok(PathBuf::from(override_dir));
    }
    match platform {
        HostPlatform::MacOs => Ok(home_dir(env)?
            .join("Library")
            .join("Application Support")
            .join(WORKER_DATA_DIR_NAME)),
        HostPlatform::Linux => {
            Ok(xdg_root(env, XDG_DATA_HOME_ENV, XDG_DATA_DEFAULT_LEAF)?.join(WORKER_DATA_DIR_NAME))
        }
        unsupported => Err(unsupported_platform(unsupported)),
    }
}

/// Where the worker writes its logs.
pub fn worker_log_dir(env: &dyn EnvSource, platform: HostPlatform) -> ProtocolResult<PathBuf> {
    if let Some(override_dir) = non_empty(env, WORKER_LOG_DIR_ENV) {
        return Ok(PathBuf::from(override_dir));
    }
    match platform {
        HostPlatform::MacOs => Ok(home_dir(env)?
            .join("Library")
            .join("Logs")
            .join(WORKER_LOG_DIR_NAME)),
        HostPlatform::Linux => Ok(
            xdg_root(env, XDG_STATE_HOME_ENV, XDG_STATE_DEFAULT_LEAF)?.join(WORKER_LOG_DIR_NAME)
        ),
        unsupported => Err(unsupported_platform(unsupported)),
    }
}

/// Where the coordinator keeps its database, authorized keys, and journals.
pub fn coord_data_dir(env: &dyn EnvSource, platform: HostPlatform) -> ProtocolResult<PathBuf> {
    if let Some(override_dir) = non_empty(env, COORD_DATA_DIR_ENV) {
        return Ok(PathBuf::from(override_dir));
    }
    match platform {
        HostPlatform::MacOs => Ok(home_dir(env)?
            .join("Library")
            .join("Application Support")
            .join(COORD_DATA_DIR_NAME)),
        HostPlatform::Linux => {
            Ok(xdg_root(env, XDG_DATA_HOME_ENV, XDG_DATA_DEFAULT_LEAF)?.join(COORD_DATA_DIR_NAME))
        }
        unsupported => Err(unsupported_platform(unsupported)),
    }
}

/// Where the coordinator writes its logs.
pub fn coord_log_dir(env: &dyn EnvSource, platform: HostPlatform) -> ProtocolResult<PathBuf> {
    if let Some(override_dir) = non_empty(env, COORD_LOG_DIR_ENV) {
        return Ok(PathBuf::from(override_dir));
    }
    match platform {
        HostPlatform::MacOs => Ok(home_dir(env)?
            .join("Library")
            .join("Logs")
            .join(COORD_LOG_DIR_NAME)),
        HostPlatform::Linux => {
            Ok(xdg_root(env, XDG_STATE_HOME_ENV, XDG_STATE_DEFAULT_LEAF)?.join(COORD_LOG_DIR_NAME))
        }
        unsupported => Err(unsupported_platform(unsupported)),
    }
}

/// Where installed service definitions and released versions live.
///
/// Hangs off the **worker's** data directory on both POSIX platforms. That
/// asymmetry is load-bearing: the installer runs on the machine that enrolls a
/// worker, so the deploy journal, the stage directory, and the version tree
/// have to sit where a worker can reach them without a second configured root.
pub fn roost_service_dir(env: &dyn EnvSource, platform: HostPlatform) -> ProtocolResult<PathBuf> {
    if let Some(override_path) = non_empty(env, SERVICE_DIR_ENV) {
        return Ok(PathBuf::from(override_path));
    }
    match platform {
        posix @ (HostPlatform::MacOs | HostPlatform::Linux) => {
            Ok(worker_data_dir(env, posix)?.join(SERVICE_DIR_SUBDIR))
        }
        unsupported => Err(unsupported_platform(unsupported)),
    }
}

/// Where released versions are unpacked, beside [`roost_service_dir`].
pub fn roost_versions_dir(env: &dyn EnvSource, platform: HostPlatform) -> ProtocolResult<PathBuf> {
    if let Some(override_path) = non_empty(env, VERSIONS_DIR_ENV) {
        return Ok(PathBuf::from(override_path));
    }
    match platform {
        posix @ (HostPlatform::MacOs | HostPlatform::Linux) => {
            Ok(worker_data_dir(env, posix)?.join(VERSIONS_DIR_SUBDIR))
        }
        unsupported => Err(unsupported_platform(unsupported)),
    }
}

/// The service identity the coordinator installs and reports as loaded.
pub fn coord_service_label(env: &dyn EnvSource, platform: HostPlatform) -> ProtocolResult<String> {
    if let Some(label) = non_empty(env, COORD_LABEL_ENV) {
        return Ok(label);
    }
    match platform {
        HostPlatform::MacOs => Ok(COORD_LABEL_DARWIN.to_string()),
        HostPlatform::Linux => Ok(COORD_LABEL_LINUX.to_string()),
        unsupported => Err(unsupported_platform(unsupported)),
    }
}

/// The service identity the worker installs and reports as loaded.
pub fn worker_service_label(env: &dyn EnvSource, platform: HostPlatform) -> ProtocolResult<String> {
    if let Some(label) = non_empty(env, WORKER_LABEL_ENV) {
        return Ok(label);
    }
    match platform {
        HostPlatform::MacOs => Ok(WORKER_LABEL_DARWIN.to_string()),
        HostPlatform::Linux => Ok(WORKER_LABEL_LINUX.to_string()),
        unsupported => Err(unsupported_platform(unsupported)),
    }
}

/// The file the coordinator's service definition is written to.
pub fn coord_service_path(env: &dyn EnvSource, platform: HostPlatform) -> ProtocolResult<PathBuf> {
    match platform {
        HostPlatform::MacOs => match env.get(COORD_PLIST_ENV) {
            Some(path) => Ok(PathBuf::from(path)),
            None => Ok(launch_agent_path(
                env,
                &format!("{}.plist", coord_service_label(env, platform)?),
            )?),
        },
        HostPlatform::Linux => match env.get(COORD_UNIT_ENV) {
            Some(path) => Ok(PathBuf::from(path)),
            None => Ok(systemd_user_path(
                env,
                &format!("{}.service", coord_service_label(env, platform)?),
            )?),
        },
        unsupported => Err(unsupported_platform(unsupported)),
    }
}

/// The file the worker's service definition is written to.
pub fn worker_service_path(env: &dyn EnvSource, platform: HostPlatform) -> ProtocolResult<PathBuf> {
    match platform {
        HostPlatform::MacOs => match env.get(WORKER_PLIST_ENV) {
            Some(path) => Ok(PathBuf::from(path)),
            None => Ok(launch_agent_path(
                env,
                &format!("{}.plist", worker_service_label(env, platform)?),
            )?),
        },
        HostPlatform::Linux => match env.get(WORKER_UNIT_ENV) {
            Some(path) => Ok(PathBuf::from(path)),
            None => Ok(systemd_user_path(
                env,
                &format!("{}.service", worker_service_label(env, platform)?),
            )?),
        },
        unsupported => Err(unsupported_platform(unsupported)),
    }
}

fn launch_agent_path(env: &dyn EnvSource, file_name: &str) -> ProtocolResult<PathBuf> {
    Ok(home_dir(env)?
        .join("Library")
        .join("LaunchAgents")
        .join(file_name))
}

fn systemd_user_path(env: &dyn EnvSource, file_name: &str) -> ProtocolResult<PathBuf> {
    Ok(home_dir(env)?
        .join(".config")
        .join("systemd")
        .join("user")
        .join(file_name))
}

/// An override that is set but empty falls back to the default, which is what a
/// truthiness test in the original did. A path override that is set and empty
/// is a different case, and is read with [`EnvSource::get`] instead.
fn non_empty(env: &dyn EnvSource, key: &str) -> Option<String> {
    env.get(key).filter(|value| !value.is_empty())
}

fn home_dir(env: &dyn EnvSource) -> ProtocolResult<PathBuf> {
    env.home_dir().ok_or_else(|| {
        ProtocolError::new(
            HOME_ENV,
            format!("{HOME_ENV} is required to resolve a default Roost path"),
        )
    })
}

/// The XDG root, or the platform's own default beneath the home directory.
fn xdg_root(env: &dyn EnvSource, key: &str, default_leaf: &str) -> ProtocolResult<PathBuf> {
    Ok(match non_empty(env, key) {
        Some(root) => PathBuf::from(root),
        None => home_dir(env)?.join(".local").join(default_leaf),
    })
}

fn unsupported_platform(platform: HostPlatform) -> ProtocolError {
    ProtocolError::new(
        "host.platform",
        format!(
            "Roost v3 does not support {}; v3 installs on macOS and Linux only",
            platform.display_name()
        ),
    )
}
