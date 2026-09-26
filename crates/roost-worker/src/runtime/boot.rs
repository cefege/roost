//! The resolved, validated configuration `serve` takes, and the one resolver
//! that produces it. Called by the `roost worker` binary and by `roost-cli`,
//! which is why it takes an injectable environment rather than reading one.
//!
//! The split is deliberate and it is the reason this file exists separately
//! from `serve`. A service manager hands a worker an environment and a command
//! line, and the only acceptable moment to refuse is before anything has been
//! started — before a socket is bound, a keeper is probed, or a frame is
//! written. So resolution runs first, and `serve` receives a value that has
//! already been checked.
//!
//! The identity is DERIVED, not configured: the fingerprint is the SHA-256 of
//! the public key in the worker's own key file, so a worker and the key the
//! coordinator has an `authorized_keys` row for cannot disagree. Deriving it
//! is the one thing resolution reads from disk, and the first boot on a machine
//! with no key generates one — the install step, and it happens here because
//! there is nowhere earlier to put it and nowhere later that a refusal is still
//! cheap.
//!
//! Every path here comes from a `roost-host` path function. This file decides
//! which file lives where; it does not decide where a file lives.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use roost_host::{
    BuildIdentity, DEFAULT_COORDINATOR_BIND, DEV_BUILD_STAMP, EnvSource, HostPlatform,
    build_identity, worker_data_dir, worker_log_dir,
};
use roost_platform::KEEPER_FORCE_LIVE_RETIRE_ENV;
use roost_protocol::wire::WorkerFp;

use crate::host::jwt::load_worker_key;
use crate::link_dial::CoordinatorEndpoint;

/// The coordinator URL, when the environment does not name one.
pub const ENV_COORDINATOR_URL: &str = "ROOST_COORDINATOR_URL";

/// The ed25519 private key this worker's identity and its coordinator
/// credential both come from. Nothing else names a worker's identity, which is
/// why it is the only file resolution must be able to create.
pub const ENV_WORKER_KEY_PATH: &str = "ROOST_WORKER_KEY_PATH";

/// The Unix socket the keeper listens on.
pub const ENV_KEEPER_SOCKET: &str = "ROOST_KEEPER_SOCKET";

/// Where the keeper records its pid, mode 0600.
pub const ENV_KEEPER_PID_FILE: &str = "ROOST_KEEPER_PID_FILE";

/// The `roost-keeper` executable, when it is not beside this binary.
pub const ENV_KEEPER_EXECUTABLE: &str = "ROOST_KEEPER_EXECUTABLE";

/// The keeper's socket filename inside the worker data directory.
pub const KEEPER_SOCKET_NAME: &str = "mux-keeper.sock";

/// The keeper's pid filename inside the worker data directory.
pub const KEEPER_PID_NAME: &str = "mux-keeper.pid";

/// The worker key filename inside the worker data directory.
pub const WORKER_KEY_NAME: &str = "coordinator_ed25519.key";

/// The executable name the keeper ships under.
pub const KEEPER_EXECUTABLE_NAME: &str = "roost-keeper";

/// Why a configuration was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BootConfigError {
    #[error("the worker data directory could not be resolved: {0}")]
    DataDir(String),
    #[error("{value} is not a usable coordinator URL: {reason}")]
    BadCoordinatorUrl { value: String, reason: String },
    #[error("the worker fingerprint is not 64 lowercase hex characters")]
    BadFingerprint,
    #[error("the worker key at {path} is unusable: {reason}")]
    WorkerKey { path: PathBuf, reason: String },
    #[error("the keeper force-live-retire authorization must be exactly 0 or 1, not {value:?}")]
    BadForceLiveRetire { value: String },
}

/// A command line's values, before they are laid over a configuration.
///
/// Named rather than left as four loose options so `roost-worker` and
/// `roost worker` cannot disagree about which option maps to which variable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkerOverrides {
    /// The coordinator base URL, overriding [`ENV_COORDINATOR_URL`].
    pub coordinator: Option<String>,
    /// The keeper socket, overriding [`ENV_KEEPER_SOCKET`].
    pub keeper_socket: Option<String>,
    /// The keeper executable, overriding [`ENV_KEEPER_EXECUTABLE`].
    pub keeper_executable: Option<String>,
}

/// Everything `serve` needs, already resolved and checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerBoot {
    /// The registry fingerprint this worker dials and registers as.
    pub fingerprint: WorkerFp,
    /// The coordinator's base URL. The scheme is rewritten to `ws`/`wss` by
    /// [`CoordinatorEndpoint`], so an operator configures the URL they use
    /// everywhere else.
    pub coordinator_base: String,
    /// The keeper socket to adopt or start.
    pub keeper_socket: PathBuf,
    /// Where a started keeper records its pid.
    pub keeper_pid_file: PathBuf,
    /// The `roost-keeper` to start when there is nothing to adopt.
    pub keeper_executable: PathBuf,
    /// The ed25519 key the coordinator credential is signed from.
    pub worker_key_path: PathBuf,
    /// Where this worker's own log lines and the keeper's log are written.
    ///
    /// Separate from the data directory because a service definition points
    /// them at different roots: data is state, logs are not, and an operator
    /// rotates one without touching the other.
    pub log_dir: PathBuf,
    /// The version this worker reports in its hello.
    pub worker_version: String,
    /// An identifier unique to this activation.
    ///
    /// The coordinator uses it to tell one worker process from the next
    /// incarnation of the same worker, so it must change on every start and
    /// must not be a clock an operator can set. It is a nanosecond wall reading
    /// joined to the pid: neither part is unique alone, and together they are
    /// unique enough that two activations of the same binary on one host
    /// cannot collide.
    pub process_epoch: String,
    /// The operator's one-shot authorization to retire a keeper that cannot be
    /// adopted or proved empty. It ends every PTY that keeper hosts, which is
    /// why it defaults off and why spending it is the activation's job.
    pub force_live_retire: bool,
}

impl WorkerBoot {
    /// Resolve a boot from an environment.
    pub fn resolve(env: &dyn EnvSource, platform: HostPlatform) -> Result<Self, BootConfigError> {
        let support = worker_data_dir(env, platform)
            .map_err(|error| BootConfigError::DataDir(error.to_string()))?;
        let logs = worker_log_dir(env, platform)
            .map_err(|error| BootConfigError::DataDir(error.to_string()))?;
        // The key file decides the identity, and it is resolved before the
        // endpoint is checked because every other field in the refusal is
        // reported next to it: an operator holding a bad coordinator URL needs
        // the fingerprint to know which machine is refusing to start.
        let worker_key_path = resolve_path(env, ENV_WORKER_KEY_PATH, support.join(WORKER_KEY_NAME));
        let fingerprint = resolve_fingerprint(&worker_key_path)?;
        let coordinator_base = env
            .get(ENV_COORDINATOR_URL)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("http://{DEFAULT_COORDINATOR_BIND}"));
        // Checked here rather than at the first dial, so a bad URL is a boot
        // refusal an operator reads instead of a reconnect loop they debug.
        check_endpoint(&coordinator_base, fingerprint.as_str())?;
        Ok(Self {
            fingerprint,
            coordinator_base,
            keeper_socket: resolve_path(env, ENV_KEEPER_SOCKET, support.join(KEEPER_SOCKET_NAME)),
            keeper_pid_file: resolve_path(env, ENV_KEEPER_PID_FILE, support.join(KEEPER_PID_NAME)),
            keeper_executable: resolve_keeper_executable(env)?,
            worker_key_path,
            log_dir: logs,
            worker_version: reported_version(&build_identity(env)),
            process_epoch: new_process_epoch(),
            force_live_retire: resolve_force_live_retire(env)?,
        })
    }

    /// Re-check a value assembled by hand rather than resolved.
    ///
    /// `roost-cli` owns refusing before anything starts, so this is what it
    /// calls once it has built a `WorkerBoot` from its own argv. It is the same
    /// check `resolve` makes, not a second and looser one.
    pub fn check(&self) -> Result<(), BootConfigError> {
        self.fingerprint
            .check()
            .map_err(|_| BootConfigError::BadFingerprint)?;
        check_endpoint(&self.coordinator_base, self.fingerprint.as_str())
    }

    /// Lay a command line's values over a resolved configuration, re-checking
    /// once at the end.
    ///
    /// Checking once, after the overlay, rather than per field, is what makes
    /// "nothing has been started yet" a fact about this call rather than a claim
    /// about the ordering inside it.
    pub fn apply(&mut self, overrides: WorkerOverrides) -> Result<(), BootConfigError> {
        if let Some(base) = overrides.coordinator {
            self.coordinator_base = base;
        }
        if let Some(hex) = overrides.fingerprint {
            self.fingerprint =
                WorkerFp::try_from(hex.as_str()).map_err(|_| BootConfigError::BadFingerprint)?;
        }
        if let Some(socket) = overrides.keeper_socket {
            self.keeper_socket = PathBuf::from(socket);
        }
        if let Some(executable) = overrides.keeper_executable {
            self.keeper_executable = PathBuf::from(executable);
        }
        self.check()
    }
}

fn check_endpoint(base: &str, fingerprint: &str) -> Result<(), BootConfigError> {
    CoordinatorEndpoint::new(base, fingerprint)
        .map(|_| ())
        .map_err(|error| BootConfigError::BadCoordinatorUrl {
            value: base.to_string(),
            reason: error.to_string(),
        })
}

/// An environment value when it is set and not empty, else the default.
///
/// "Set to an empty string" and "absent" are the same thing here: an empty
/// `ROOST_KEEPER_SOCKET` is a path nothing can connect to, and falling back to
/// the installer's layout is the only reading of it that works.
fn resolve_path(env: &dyn EnvSource, name: &str, default: PathBuf) -> PathBuf {
    match env.get(name) {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => default,
    }
}

/// The fingerprint of the key at `key_path`, which is the whole of a worker's
/// identity: SHA-256 of the public key, rendered by the one renderer in the
/// workspace. A machine with no key is given one here, because a worker that
/// has never been installed has no other way to become itself, and this runs
/// before anything is bound.
fn resolve_fingerprint(key_path: &Path) -> Result<WorkerFp, BootConfigError> {
    load_worker_key(key_path)
        .map(|key| key.fingerprint().clone())
        .map_err(|error| BootConfigError::WorkerKey {
            path: key_path.to_path_buf(),
            reason: error.to_string(),
        })
}

/// A fresh per-activation identity.
///
/// A UUID is what v2 minted; nothing downstream parses this string as one, and
/// a hand-rolled value keeps a uuid dependency out of a crate with no other
/// use for it. The pid is here because two activations inside one nanosecond
/// are possible on a coarse clock, and the wall reading is here because pids
/// repeat.
pub fn new_process_epoch() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let nanos = u64::try_from(nanos).unwrap_or(u64::MAX);
    format!("{nanos}-{}", std::process::id())
}

/// The version a hello reports.
///
/// A build that carries a version reports it. A source checkout reports its
/// commit rather than the literal `dev`, because a fleet of workers all
/// claiming `dev` is exactly the state the coordinator's compatibility check
/// cannot use.
fn reported_version(identity: &BuildIdentity) -> String {
    if identity.artifact_version == DEV_BUILD_STAMP {
        identity.build_sha.clone()
    } else {
        identity.artifact_version.clone()
    }
}

fn resolve_force_live_retire(env: &dyn EnvSource) -> Result<bool, BootConfigError> {
    match env.get(KEEPER_FORCE_LIVE_RETIRE_ENV) {
        None => Ok(false),
        Some(value) if value == "0" => Ok(false),
        Some(value) if value == "1" => Ok(true),
        Some(value) => Err(BootConfigError::BadForceLiveRetire { value }),
    }
}

/// The keeper to start, which is the one beside this binary unless the
/// environment names another.
///
/// Beside this binary is the installed layout: a release ships `roost` and
/// `roost-keeper` into one directory. A worker reaching across the tree for a
/// keeper source file is how a keeper ends up running code that is not the one
/// the admission gate hashed.
fn resolve_keeper_executable(env: &dyn EnvSource) -> Result<PathBuf, BootConfigError> {
    if let Some(value) = env
        .get(ENV_KEEPER_EXECUTABLE)
        .filter(|value| !value.is_empty())
    {
        return Ok(PathBuf::from(value));
    }
    let Ok(executable) = std::env::current_exe() else {
        return Ok(PathBuf::from(KEEPER_EXECUTABLE_NAME));
    };
    Ok(executable.parent().map_or_else(
        || PathBuf::from(KEEPER_EXECUTABLE_NAME),
        |directory| directory.join(KEEPER_EXECUTABLE_NAME),
    ))
}
