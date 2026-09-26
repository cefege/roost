//! Putting a staged release into the place this install actually keeps
//! releases, and the environment its definition is resolved from. Called by
//! apply.rs; depends on `roost-host`'s own path resolution and on the services
//! group's install, and on nothing else in the deploy group.
//!
//! The release root comes from the installed definition rather than from this
//! process's environment, and that is the whole reason this file exists. An
//! operator who moved the versions directory did it by editing the unit, so the
//! unit is the only place the install's path policy survives; a root derived from
//! the environment an ssh login happens to carry would install the new release
//! somewhere the retired one never lived, and settlement would then delete a
//! directory that is not on the path to anything.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use roost_host::{EnvSource, HostPlatform, MapEnv, ProcessEnv, roost_versions_dir};

use crate::command_error::CommandFailure;
use crate::deploy::ssh::reject_control_characters;
use crate::services::install::PROGRAM_MODE;
use crate::services::service_environment::{ENV_HOME, ENV_PATH, default_service_path};

/// The directory name a release's executables live in, inside a release root.
pub const RELEASE_BIN_DIR: &str = "bin";

/// The executable name a release installs.
pub const ROOST_PROGRAM: &str = "roost";

/// The keeper executable beside it. A release without both is not a release: the
/// keeper is a separate binary precisely so a coordinator deploy never disturbs
/// a live PTY, and a `roost` beside no `roost-keeper` describes a keeper this
/// build does not have.
pub const KEEPER_PROGRAM: &str = "roost-keeper";

/// The directory releases are installed under, from the installed definition when
/// there is one.
pub fn release_root_for(
    env: &dyn EnvSource,
    platform: HostPlatform,
    prior_release_dir: Option<&Path>,
) -> Result<PathBuf, String> {
    if let Some(prior) = prior_release_dir
        && let Some(root) = prior.parent()
    {
        return Ok(root.to_path_buf());
    }
    roost_versions_dir(env, platform)
        .map_err(|error| format!("this machine's release root cannot be resolved: {error}"))
}

/// The environment the new definition is resolved from: the target's own home, a
/// service search path, and everything the deploying box decided.
pub fn install_environment(env: &dyn EnvSource, decided: &BTreeMap<String, String>) -> MapEnv {
    let mut install_env = MapEnv::new();
    if let Some(home) = env.home_dir() {
        install_env.set(ENV_HOME, &home.display().to_string());
        install_env.set(ENV_PATH, &default_service_path(&home));
    }
    for (key, value) in decided {
        install_env.set(key, value);
    }
    install_env
}

/// Move a staged release's executables into their installed home.
///
/// The destination is replaced rather than merged, so a release directory can
/// never be a mixture of two builds: a half-updated release is a release whose
/// digest proves nothing.
pub fn install_release(staged_bin: &Path, bin_dir: &Path) -> Result<(), String> {
    if !staged_bin.is_dir() {
        return Err(format!(
            "the staged release has no {RELEASE_BIN_DIR} directory at {}",
            staged_bin.display()
        ));
    }
    for program in [ROOST_PROGRAM, KEEPER_PROGRAM] {
        if !staged_bin.join(program).is_file() {
            return Err(format!(
                "the staged release ships no {program}; a release without it is not a release"
            ));
        }
    }
    if let Some(parent) = bin_dir.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    let retired = bin_dir.with_extension("roost-retired");
    let _ = std::fs::remove_dir_all(&retired);
    if bin_dir.exists() {
        std::fs::rename(bin_dir, &retired).map_err(|error| {
            format!(
                "cannot move the previous {} aside: {error}",
                bin_dir.display()
            )
        })?;
    }
    if let Err(error) = std::fs::rename(staged_bin, bin_dir) {
        // Put the previous release back before reporting, so a failed install
        // leaves the machine on the release it was already running.
        let _ = std::fs::rename(&retired, bin_dir);
        return Err(format!(
            "cannot install the staged release into {}: {error}",
            bin_dir.display()
        ));
    }
    let _ = std::fs::remove_dir_all(&retired);
    for program in [ROOST_PROGRAM, KEEPER_PROGRAM] {
        set_executable(&bin_dir.join(program))?;
    }
    // The staged tree has been moved into place, so the scratch area it came
    // from is now empty; removing it is what keeps a machine from accumulating
    // one directory per deploy.
    if let Some(scratch) = bin_dir.parent().and_then(Path::parent) {
        let _ = std::fs::remove_dir(scratch);
    }
    Ok(())
}

fn set_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(PROGRAM_MODE))
        .map_err(|error| format!("cannot make {} executable: {error}", path.display()))
}

/// The installed definition's bytes, when there are any.
pub fn read_installed(definition_path: &Path) -> Option<String> {
    std::fs::read_to_string(definition_path).ok()
}

/// The manifest on this process's standard input, which is how the deploying box
/// delivers it. Read with a bound so a target that is asked to apply and given
/// no input fails with a message instead of hanging.
pub fn manifest_from_stdin() -> Result<Vec<u8>, CommandFailure> {
    const MAX_MANIFEST_BYTES: u64 = 1 << 20;
    let mut bytes = Vec::new();
    std::io::stdin()
        .lock()
        .take(MAX_MANIFEST_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            CommandFailure::generic(format!("cannot read the apply manifest: {error}"))
        })?;
    if bytes.is_empty() {
        return Err(CommandFailure::generic("the apply manifest arrived empty"));
    }
    Ok(bytes)
}

/// The staging directory a deploy puts a release at, under the target's own
/// home. Both sides compute it from the home they each already know, which is
/// what lets the deploying box ship a release before it knows where the target
/// installs releases.
pub fn staging_dir(home: &str, git_sha: &str) -> PathBuf {
    Path::new(home).join(".roost-deploy").join(git_sha)
}

/// Refuse a staged path that could not have come from a home directory this
/// deploy resolved, before it is used to move anything.
pub fn reject_staged_path(staged: &Path) -> Result<(), CommandFailure> {
    reject_control_characters("staged release path", &staged.display().to_string())
}

/// The environment a target runs its apply with, when it is this process.
pub fn process_environment() -> &'static dyn EnvSource {
    &ProcessEnv
}
