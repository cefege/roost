//! Putting a release and its service definition on disk. Both are idempotent:
//! a second run with the same inputs rewrites nothing at all, so re-running an
//! install that half-failed cannot leave a second copy of a binary or a
//! truncated unit where a working one was.
//!
//! Idempotence is a byte comparison against what is already there, not a
//! timestamp check and not a "skip if the file exists" shortcut. The shortcut
//! is what leaves a truncated unit installed after an interrupted first run,
//! because the file exists and the file is wrong.

use std::path::{Path, PathBuf};

use roost_host::{
    EnvSource, HostPlatform, ProtocolError, ProtocolResult, build_identity, roost_versions_dir,
};

use crate::services::atomic_file::{InstalledFile, read_installed_file, write_durable};
use crate::services::definition_text::{
    DEFINITION_MODE, definition_is_complete, render_definition,
};
use crate::services::service_spec::ServiceSpec;

/// The permission bits an installed program carries. A service manager starts
/// it directly, so it has to be executable, and it holds a database, so it is
/// not readable by anyone else.
pub const PROGRAM_MODE: u32 = 0o755;

/// The directory name a release's executables live in, under the release root.
const RELEASE_BIN_DIR: &str = "bin";

/// The `roost` executable's file name.
const ROOST_PROGRAM: &str = "roost";

/// The keeper executable that ships beside `roost`. It is a separate binary so
/// a coordinator deploy never disturbs a live PTY, and `roost keeper` refuses
/// rather than running something else when it is absent.
const KEEPER_PROGRAM: &str = "roost-keeper";

/// What one install step did, so a caller can report it and a test can tell a
/// no-op from a rewrite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallOutcome {
    /// The file this step owns.
    pub path: PathBuf,
    /// Whether anything on disk changed. A second identical install reports
    /// `false` and leaves the file's bytes alone.
    pub changed: bool,
}

/// Every distinct way an install can fail. Each variant names the file, because
/// an operator reading the failure needs to know which path is theirs.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("the release program {program} cannot be read: {cause}")]
    SourceMissing {
        /// The program that was to be installed.
        program: PathBuf,
        /// Why it could not be read.
        cause: String,
    },
    #[error("the rendered {label} definition is incomplete for {platform}; refusing to install it")]
    IncompleteDefinition {
        /// The service the definition belongs to.
        label: String,
        /// The platform whose format it was rendered for.
        platform: &'static str,
    },
    #[error("{path}: {cause}")]
    Io {
        /// The file the failure happened on.
        path: PathBuf,
        /// The operating system's answer.
        cause: String,
    },
    #[error(transparent)]
    Host(#[from] ProtocolError),
}

/// The directory a release of this build installs its executables into:
/// `<versions dir>/<version>/bin`. A release names its own version, so two
/// releases sit side by side and the installed definition can point at either.
pub fn release_bin_dir(env: &dyn EnvSource, platform: HostPlatform) -> ProtocolResult<PathBuf> {
    let version = build_identity(env).artifact_version;
    Ok(roost_versions_dir(env, platform)?
        .join(version)
        .join(RELEASE_BIN_DIR))
}

/// Where `roost` for this build belongs.
pub fn default_program_path(
    env: &dyn EnvSource,
    platform: HostPlatform,
) -> ProtocolResult<PathBuf> {
    Ok(release_bin_dir(env, platform)?.join(ROOST_PROGRAM))
}

/// Install one program at `destination` with `mode`, and report whether
/// anything changed. Identical content and identical permissions are a no-op.
pub fn install_binary(
    source: &Path,
    destination: &Path,
    mode: u32,
) -> Result<InstallOutcome, InstallError> {
    let bytes = std::fs::read(source).map_err(|error| InstallError::SourceMissing {
        program: source.to_path_buf(),
        cause: error.to_string(),
    })?;
    install_bytes(destination, &bytes, mode)
}

/// Install the `roost` program and, when the build ships one, the keeper
/// beside it. The keeper is optional at the source because a development build
/// has no separate keeper artifact; the report says which steps ran, so a
/// release cannot claim to have shipped a keeper it did not install.
pub fn install_release_programs(
    roost_source: &Path,
    keeper_source: Option<&Path>,
    bin_dir: &Path,
) -> Result<Vec<InstallOutcome>, InstallError> {
    let mut outcomes = vec![install_binary(
        roost_source,
        &bin_dir.join(ROOST_PROGRAM),
        PROGRAM_MODE,
    )?];
    if let Some(keeper) = keeper_source
        && keeper.is_file()
    {
        outcomes.push(install_binary(
            keeper,
            &bin_dir.join(KEEPER_PROGRAM),
            PROGRAM_MODE,
        )?);
    }
    Ok(outcomes)
}

/// Create the directories a service needs before it can start: its data
/// directory, its log directory, and the directory its definition lives in.
/// A macOS account that has never launched a daemon has no `LaunchAgents`
/// directory, and a definition staged into a directory that does not exist
/// fails on the very first install of a machine.
pub fn ensure_service_directories(spec: &ServiceSpec) -> Result<Vec<PathBuf>, InstallError> {
    let mut created = Vec::new();
    for directory in [
        Some(spec.data_dir.clone()),
        Some(spec.log_dir.clone()),
        spec.definition_path.parent().map(Path::to_path_buf),
    ]
    .into_iter()
    .flatten()
    {
        std::fs::create_dir_all(&directory).map_err(|error| InstallError::Io {
            path: directory.clone(),
            cause: error.to_string(),
        })?;
        created.push(directory);
    }
    Ok(created)
}

/// Render `spec` and install it, atomically, and report whether anything
/// changed. The rendered text is proved complete for the platform first: a
/// definition the platform would decline to load is refused here, while the
/// installed one is still the one that works.
pub fn install_definition(
    spec: &ServiceSpec,
    platform: HostPlatform,
) -> Result<InstallOutcome, InstallError> {
    let text = render_definition(spec, platform)?;
    if !definition_is_complete(&text, platform) {
        return Err(InstallError::IncompleteDefinition {
            label: spec.label.clone(),
            platform: platform.as_str(),
        });
    }
    install_bytes(&spec.definition_path, text.as_bytes(), DEFINITION_MODE)
}

/// Write `bytes` at `path` unless the file already holds exactly that, with
/// exactly those permissions.
pub fn install_bytes(path: &Path, bytes: &[u8], mode: u32) -> Result<InstallOutcome, InstallError> {
    if already_installed(path, bytes, mode)? {
        return Ok(InstallOutcome {
            path: path.to_path_buf(),
            changed: false,
        });
    }
    write_durable(path, bytes, mode).map_err(|error| InstallError::Io {
        path: path.to_path_buf(),
        cause: error.to_string(),
    })?;
    Ok(InstallOutcome {
        path: path.to_path_buf(),
        changed: true,
    })
}

fn already_installed(path: &Path, bytes: &[u8], mode: u32) -> Result<bool, InstallError> {
    let installed = read_installed_file(path).map_err(|error| InstallError::Io {
        path: path.to_path_buf(),
        cause: error.to_string(),
    })?;
    match installed {
        InstalledFile::Absent => Ok(false),
        InstalledFile::Present {
            bytes: present,
            mode: present_mode,
        } => Ok(present == bytes && present_mode == mode),
    }
}
