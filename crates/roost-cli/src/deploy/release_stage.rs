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
use roost_platform::posix_shell_quote;

/// Put a staged release on the target at `remote_release_dir`.
///
/// tar over ssh rather than rsync, because the target is a POSIX box whose only
/// guaranteed tools are the ones its own service manager needs, and because the
/// payload is produced here rather than selected by path patterns on the far
/// side: a release ships exactly the files this build produced, which is the
/// property a release digest is about.
///
/// The release is extracted into a temporary name beside its destination and
/// then renamed, so a target that loses power mid-extract leaves the temporary
/// directory rather than a release directory holding half a binary.
///
/// The destination is removed first, and that is the load-bearing half. `mv`
/// onto an EXISTING directory is a nesting move, not a replacement: with a
/// leftover from an earlier attempt still in place, `mv "$staging" "$root"`
/// quietly produces `$root/<sha>.staging` and fails, which is what a second
/// deploy to the same target did. The destination is the deploying box's own
/// scratch path (`~/.roost-deploy/<sha>`), never an installed release — the
/// installed one lives under the service directory and is written by the
/// target's own apply — so removing it costs nothing and makes the stage
/// idempotent, which is what lets a deploy be re-run after a failure.
pub async fn stage_over_ssh(
    host: &str,
    release: &StagedRelease,
    remote_release_dir: &str,
) -> Result<(), CommandFailure> {
    let tar = tar_stdin(release.local_dir.parent().unwrap_or(&release.local_dir))?;
    let command = staging_command(remote_release_dir);
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

/// The command that puts a release at `remote_release_dir` on the target.
///
/// Separate from the spawn so the shape can be run through a real shell in a
/// test: the difference between a move that replaces and a move that nests is
/// not visible by reading the string, it is only visible by running it twice.
pub fn staging_command(remote_release_dir: &str) -> String {
    let staging = format!("{remote_release_dir}.staging");
    format!(
        "set -e; umask 077; root={root}; staging={staging}; \
         mkdir -p \"$(dirname \"$root\")\"; rm -rf \"$staging\" \"$root\"; mkdir -p \"$staging\"; \
         tar -C \"$staging\" -xf -; mv \"$staging\" \"$root\"",
        root = posix_shell_quote(remote_release_dir),
        staging = posix_shell_quote(&staging),
    )
}
