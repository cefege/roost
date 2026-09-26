//! One value describing an installed Roost service: which of the two roles it
//! is, the identity and file the platform reads it under, the program to run,
//! and the environment it runs with. Both definition formats and the deploy
//! transaction are built from this and nothing else, so a unit and a plist
//! cannot disagree about what they install.
//!
//! Names come from `roost-host`, and the per-role settings come from
//! service_settings.rs, which resolves the coordinator's through
//! `load_coord_config` rather than re-deriving them: the definition then states
//! what the coordinator will actually resolve at boot, and an invalid setting
//! is refused by the install instead of by the first boot.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use roost_host::build_identity::{DEV_BUILD_STAMP, GIT_SHA_ENV, ROOST_GIT_SHA_ENV, build_identity};
use roost_host::{
    EnvSource, HostPlatform, ProtocolError, ProtocolResult, coord_data_dir, coord_log_dir,
    coord_service_label, coord_service_path, worker_data_dir, worker_log_dir, worker_service_label,
    worker_service_path,
};

use crate::services::memory_limits::{ResourceLimits, host_total_memory_bytes};
use crate::services::service_environment::{
    DIAGNOSTIC_ENV, ENV_HOME, ENV_PATH, default_service_path,
};
use crate::services::service_settings::role_settings;

/// Which of the two long-lived services a definition describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceRole {
    /// `roost coord` — the coordinator, which serves the browser front end.
    Coordinator,
    /// `roost worker` — the worker, which owns this machine's PTYs.
    Worker,
}

impl ServiceRole {
    /// Both roles, in the order a fresh install brings them up.
    pub const ALL: [ServiceRole; 2] = [ServiceRole::Coordinator, ServiceRole::Worker];

    /// The `roost` subcommand that runs this role.
    pub const fn subcommand(self) -> &'static str {
        match self {
            ServiceRole::Coordinator => "coord",
            ServiceRole::Worker => "worker",
        }
    }

    /// The human name a service manager shows for this role.
    pub const fn display_name(self) -> &'static str {
        match self {
            ServiceRole::Coordinator => "coordinator",
            ServiceRole::Worker => "worker",
        }
    }

    /// The identity the platform reads this role's service under.
    pub fn service_label(
        self,
        env: &dyn EnvSource,
        platform: HostPlatform,
    ) -> ProtocolResult<String> {
        match self {
            ServiceRole::Coordinator => coord_service_label(env, platform),
            ServiceRole::Worker => worker_service_label(env, platform),
        }
    }

    /// The file this role's definition is written to.
    pub fn definition_path(
        self,
        env: &dyn EnvSource,
        platform: HostPlatform,
    ) -> ProtocolResult<PathBuf> {
        match self {
            ServiceRole::Coordinator => coord_service_path(env, platform),
            ServiceRole::Worker => worker_service_path(env, platform),
        }
    }

    /// The directory this role keeps its durable state in.
    pub fn data_dir(self, env: &dyn EnvSource, platform: HostPlatform) -> ProtocolResult<PathBuf> {
        match self {
            ServiceRole::Coordinator => coord_data_dir(env, platform),
            ServiceRole::Worker => worker_data_dir(env, platform),
        }
    }

    /// The directory this role's service writes its log files to.
    pub fn log_dir(self, env: &dyn EnvSource, platform: HostPlatform) -> ProtocolResult<PathBuf> {
        match self {
            ServiceRole::Coordinator => coord_log_dir(env, platform),
            ServiceRole::Worker => worker_log_dir(env, platform),
        }
    }
}

/// Everything a definition is rendered from. Two definitions differ only in
/// their `program` and their `environment`, which is what makes the two
/// formats a rendering of one thing rather than two hand-maintained templates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceSpec {
    /// Which service this is.
    pub role: ServiceRole,
    /// The identity the platform reads it under.
    pub label: String,
    /// The file the definition is written to.
    pub definition_path: PathBuf,
    /// The compiled `roost` binary, which the service runs by absolute path.
    pub program: PathBuf,
    /// The directory the service runs in. A release points at its own root, so
    /// nothing an install starts depends on a source checkout still being there.
    pub working_directory: PathBuf,
    /// Where the platform's log files for this service go.
    pub log_dir: PathBuf,
    /// Where this role's durable state lives. The install creates it, and a
    /// reset reads it, so it belongs on the spec rather than being resolved
    /// twice from an environment that may have changed in between.
    pub data_dir: PathBuf,
    /// The cgroup ceilings, rendered into the unit and carried nowhere by
    /// launchd, which has no per-agent memory limit.
    pub limits: ResourceLimits,
    /// The environment, in the order a definition prints it.
    pub environment: BTreeMap<String, String>,
}

impl ServiceSpec {
    /// Resolve a spec against the operator's environment, for a service whose
    /// program is `program`.
    pub fn resolve(
        role: ServiceRole,
        env: &dyn EnvSource,
        platform: HostPlatform,
        program: &Path,
    ) -> ProtocolResult<Self> {
        Self::resolve_with_host_memory(role, env, platform, program, host_total_memory_bytes())
    }

    /// The same resolution against a caller-supplied host total, so the clamp
    /// branches are reachable without a one-gibibyte machine.
    pub fn resolve_with_host_memory(
        role: ServiceRole,
        env: &dyn EnvSource,
        platform: HostPlatform,
        program: &Path,
        host_total_bytes: u64,
    ) -> ProtocolResult<Self> {
        let home = env.home_dir().ok_or_else(|| {
            ProtocolError::new(
                "HOME",
                "an installed service definition needs a home directory",
            )
        })?;
        let limits = match role {
            ServiceRole::Coordinator => ResourceLimits::coordinator(host_total_bytes),
            ServiceRole::Worker => ResourceLimits::worker(host_total_bytes),
        };
        let mut environment = BTreeMap::new();
        environment.insert(ENV_HOME.to_string(), home.display().to_string());
        environment.insert(ENV_PATH.to_string(), default_service_path(&home));
        // The firehose is a documented trap left on, and the observability
        // crate owns the one value that turns it on. An install therefore
        // writes the operator's own choice or writes nothing at all — it never
        // stamps "on" on a machine that did not ask for it.
        let (diagnostic_name, diagnostic_on) = DIAGNOSTIC_ENV;
        if let Some(choice) = env.get(diagnostic_name) {
            let value = if choice == diagnostic_on {
                diagnostic_on
            } else {
                "0"
            };
            environment.insert(diagnostic_name.to_string(), value.to_string());
        }
        let identity = build_identity(env);
        if identity.build_sha != DEV_BUILD_STAMP {
            // Both spellings, because the coordinator's status readout and the
            // worker's heartbeat have each always read one of them and a fleet
            // roster that showed one SHA and not the other would look stale.
            environment.insert(GIT_SHA_ENV.to_string(), identity.build_sha.clone());
            environment.insert(ROOST_GIT_SHA_ENV.to_string(), identity.build_sha);
        }
        environment.extend(role_settings(role, env, platform)?);
        Ok(Self {
            role,
            label: role.service_label(env, platform)?,
            definition_path: role.definition_path(env, platform)?,
            program: program.to_path_buf(),
            working_directory: program
                .parent()
                .map_or_else(|| program.to_path_buf(), Path::to_path_buf),
            log_dir: role.log_dir(env, platform)?,
            data_dir: role.data_dir(env, platform)?,
            limits,
            environment,
        })
    }

    /// Add or replace one entry. This is how a deploy carries a setting a
    /// caller decided on, including the one-shot authorizations a plain
    /// resolve deliberately refuses to reinstall.
    pub fn with_setting(mut self, name: &str, value: impl Into<String>) -> Self {
        self.environment.insert(name.to_string(), value.into());
        self
    }

    /// The two facts a service manager needs, which is why activation takes
    /// this rather than the whole spec: a recovery run has a journal on disk
    /// and no resolved spec.
    pub fn target(&self) -> ServiceTarget {
        ServiceTarget {
            label: self.label.clone(),
            definition_path: self.definition_path.clone(),
        }
    }
}

/// A service identity and the file it is installed under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceTarget {
    /// The label the platform knows the service by.
    pub label: String,
    /// The definition file the service was installed from.
    pub definition_path: PathBuf,
}
