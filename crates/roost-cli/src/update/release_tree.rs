//! Stages a complete release in its own version directory and activates it
//! through the existing journaled executable swap. The candidate is a symlink
//! to that directory, so `current_exe` and its OMP sibling resolve together.

use std::path::{Path, PathBuf};

use roost_host::{EnvSource, HostPlatform, ROOST_PROGRAM_FILE};

use crate::command_error::CommandFailure;
use crate::update::candidate::{self, VerifiedCandidate};

/// Whether this executable belongs to a worker-only managed installation.
pub fn is_managed_worker(env: &dyn EnvSource, platform: HostPlatform, executable: &Path) -> bool {
    use crate::services::service_spec::ServiceRole;
    let directory_for = |role: ServiceRole| {
        let path = role.definition_path(env, platform).ok()?;
        let text = std::fs::read_to_string(path).ok()?;
        crate::deploy::installed::installed_release_dir(&text, platform)
            .and_then(|dir| std::fs::canonicalize(dir.join(ROOST_PROGRAM_FILE)).ok())
    };
    let executable = std::fs::canonicalize(executable).ok();
    let worker = directory_for(ServiceRole::Worker);
    let coordinator = directory_for(ServiceRole::Coordinator);
    worker.is_some() && worker == executable && coordinator != executable
}

/// Fetch a complete release before preparing its atomic command-path swap.
#[cfg(unix)]
pub async fn stage(
    env: &dyn EnvSource,
    platform: HostPlatform,
    arch: &str,
    tag: &str,
    executable: &Path,
) -> Result<(VerifiedCandidate, PathBuf), CommandFailure> {
    let staged = crate::deploy::release_fetch::fetch_release(env, tag, platform, arch).await?;
    let staged_root = staged.local_dir.parent().map(Path::to_path_buf);
    let versions = roost_host::roost_versions_dir(env, platform)
        .map_err(|error| CommandFailure::generic(error.to_string()))?;
    let version_dir = versions.join(tag);
    let destination = version_dir.join("bin");
    let result = (|| {
        let expected = staged.digest;
        std::fs::create_dir_all(&versions)
            .map_err(|error| CommandFailure::generic(error.to_string()))?;
        match std::fs::create_dir(&version_dir) {
            Ok(()) => {
                if let Err(error) =
                    crate::deploy::apply_release::install_release(&staged.local_dir, &destination)
                {
                    let _ = std::fs::remove_dir_all(&version_dir);
                    return Err(CommandFailure::generic(error));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing = crate::deploy::release::release_digest(&destination)?;
                if existing != expected {
                    return Err(CommandFailure::generic(format!(
                        "{} already contains a different release; refusing to replace it",
                        destination.display()
                    )));
                }
            }
            Err(error) => {
                return Err(CommandFailure::generic(format!(
                    "cannot create {}: {error}",
                    version_dir.display()
                )));
            }
        }
        let actual = crate::deploy::release::release_digest(&destination)?;
        if expected != actual {
            return Err(CommandFailure::generic(
                "the installed release differs from the verified staging tree",
            ));
        }
        let target = destination.join(ROOST_PROGRAM_FILE);
        let sha256 = candidate::digest_of(&target)
            .map_err(|error| CommandFailure::generic(error.to_string()))?;
        let path = create_candidate_link(executable, &target)?;
        Ok((
            VerifiedCandidate {
                path,
                sha256,
                url: tag.to_owned(),
            },
            target,
        ))
    })();
    if let Some(root) = staged_root {
        let _ = std::fs::remove_dir_all(root);
    }
    result
}

#[cfg(unix)]
fn create_candidate_link(executable: &Path, target: &Path) -> Result<PathBuf, CommandFailure> {
    let base = candidate::candidate_path_for(executable);
    for attempt in 0..64 {
        let path = base.with_extension(format!("link.{}.{attempt}", std::process::id()));
        match std::os::unix::fs::symlink(target, &path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(CommandFailure::generic(format!(
                    "cannot stage {}: {error}",
                    path.display()
                )));
            }
        }
    }
    Err(CommandFailure::generic("no free update candidate link"))
}
