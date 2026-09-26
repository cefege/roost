//! Putting a built release on the target: the one place a release crosses the
//! ssh boundary. Called by the deploy command after `release::build_release`
//! has assembled the tree; depends on the ssh transport and on nothing else in
//! the deploy group, so what a release is and what shipping it means stay two
//! separate questions.
//!
//! tar over ssh rather than rsync, because the target is a POSIX box whose only
//! guaranteed tools are the ones its own service manager needs, and because the
//! payload is produced here rather than selected by path patterns on the far
//! side: a release ships exactly the files this build produced, which is the
//! property a release digest is about.

use std::path::Path;

use crate::command_error::CommandFailure;
use crate::deploy::codes;
use crate::deploy::release::StagedRelease;
use crate::deploy::ssh;

/// Put a staged release on the target at `remote_release_dir`.
///
/// tar over ssh rather than rsync, because the target is a POSIX box whose only
/// guaranteed tools are the ones its own service manager needs, and because the
/// payload is produced here rather than selected by path patterns on the far
/// side: a release ships exactly the files this build produced, which is the
/// property a release digest is about.
///
/// The directory is created under `umask 077` and staged into a temporary name
/// beside it, so a release is either wholly there or wholly absent. A target
/// that loses power mid-extract leaves the temporary directory, not a release
/// directory holding half a binary.
pub async fn stage_over_ssh(
    host: &str,
    release: &StagedRelease,
    remote_release_dir: &str,
) -> Result<(), CommandFailure> {
    let tar = tar_stdin(release.local_dir.parent().unwrap_or(&release.local_dir))?;
    let staging = format!("{remote_release_dir}.staging");
    let command = format!(
        "set -e; umask 077; root={root}; staging={staging}; \
         mkdir -p \"$(dirname \"$root\")\"; rm -rf \"$staging\"; mkdir -p \"$staging\"; \
         tar -C \"$staging\" -xf -; mv \"$staging\" \"$root\"",
        root = roost_platform::posix_shell_quote(remote_release_dir),
        staging = roost_platform::posix_shell_quote(&staging),
    );
    println!(">> stage {} on {host}", release.local_dir.display());
    let outcome = ssh::exec_with_stdin(host, &command, tar).await?;
    if !outcome.ok() {
        return Err(codes::refuse(
            codes::REMOTE_LOST,
            format!(
                "cannot stage the release at {remote_release_dir} on {host}\n{}",
                outcome.detail()
            ),
        ));
    }
    Ok(())
}

fn tar_stdin(directory: &Path) -> Result<Vec<u8>, CommandFailure> {
    let output = std::process::Command::new("tar")
        .arg("-C")
        .arg(directory)
        .args(["-cf", "-", "."])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| {
            codes::refuse(
                codes::BUILD_FAILED,
                format!("cannot run tar to stage the release: {error}"),
            )
        })?;
    if !output.status.success() {
        return Err(codes::refuse(
            codes::BUILD_FAILED,
            format!(
                "tar could not read the built release: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ));
    }
    Ok(output.stdout)
}
