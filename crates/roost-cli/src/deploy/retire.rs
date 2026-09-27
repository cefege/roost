//! Retiring the release a settled deploy replaced, and the two guards that
//! make removing a directory the right thing to do. Called by the target-side
//! apply driver once the new definition is proven up, and by nothing else.
//!
//! The first guard is the worktree question. A release directory looks like a
//! git worktree because the developer's own checkout is one, and every release a
//! real host has was staged by a copy — so asking git to remove a worktree fails
//! on exactly the machine that matters. It fails at SETTLEMENT, after the
//! service definition already points at the new release, which reads as a failed
//! deploy of a worker that is actually running the new code. So the question is
//! asked of git rather than assumed, and a directory that is not a registered
//! worktree is removed outright.
//!
//! The second guard is confinement, and it is what makes the plain removal safe:
//! a prior release has to be a direct child of the release root, must not be a
//! symlink, and must not BE the release root. A path that arrived from a
//! definition somebody hand-edited must not be able to turn a settlement into a
//! recursive delete of something else.

use std::path::{Path, PathBuf};
use std::process::Stdio;

/// How a prior release directory is removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retirement {
    /// It is a registered git worktree, so git's own removal is used and its
    /// administrative files go with it.
    GitWorktree,
    /// It is an ordinary staged directory, so it is removed directly.
    PlainDirectory,
}

impl Retirement {
    /// The word a log line and an operator readout use.
    pub const fn display_name(self) -> &'static str {
        match self {
            Retirement::GitWorktree => "removed the prior release worktree",
            Retirement::PlainDirectory => "removed the prior staged release",
        }
    }
}

/// Decide how `prior` is retired, from what git reports about it.
///
/// `registered_worktrees` is the parsed `git worktree list --porcelain` output,
/// passed in so the decision is testable without a repository. The confinement
/// and symlink checks are here rather than in the caller because they are the
/// part that must never be skipped.
pub fn plan_retirement(
    release_root: &Path,
    prior: &Path,
    registered_worktrees: &[PathBuf],
) -> Result<Retirement, String> {
    if prior == release_root {
        return Err(format!(
            "refusing to retire the release root {} itself",
            release_root.display()
        ));
    }
    if prior.parent() != Some(release_root) {
        return Err(format!(
            "refusing to retire {}: it is not a release directory directly inside {}",
            prior.display(),
            release_root.display()
        ));
    }
    if prior.is_symlink() {
        return Err(format!(
            "refusing to retire {}: it is a symbolic link, so what it names is not a release",
            prior.display()
        ));
    }
    if !prior.exists() {
        return Err(format!(
            "refusing to retire {}: it is not there",
            prior.display()
        ));
    }
    let canonical = prior
        .canonicalize()
        .map_err(|error| format!("cannot resolve {}: {error}", prior.display()))?;
    if canonical.parent()
        != Some(
            &release_root
                .canonicalize()
                .map_err(|error| format!("cannot resolve {}: {error}", release_root.display()))?,
        )
    {
        return Err(format!(
            "refusing to retire {}: it resolves to {}, which is outside {}",
            prior.display(),
            canonical.display(),
            release_root.display()
        ));
    }
    if registered_worktrees
        .iter()
        .any(|worktree| worktree == prior)
    {
        return Ok(Retirement::GitWorktree);
    }
    Ok(Retirement::PlainDirectory)
}

/// The worktrees git has registered, read from inside `directory`.
///
/// A target with no git, or a directory that is not in a repository at all,
/// reports none — which is the honest answer, and the one that leads to a plain
/// removal. It never reports a worktree it was not told about.
pub fn registered_worktrees(directory: &Path) -> Vec<PathBuf> {
    let Ok(output) = std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(directory)
        .stdin(Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix("worktree ").map(PathBuf::from))
        .collect()
}

/// Retire `prior`, and report how it went.
pub fn retire_prior_release(release_root: &Path, prior: &Path) -> Result<Retirement, String> {
    let planned = plan_retirement(release_root, prior, &registered_worktrees(prior))?;
    match planned {
        Retirement::GitWorktree => {
            let removal = std::process::Command::new("git")
                .args(["worktree", "remove", "--force"])
                .arg(prior)
                .stdin(Stdio::null())
                .output()
                .map_err(|error| format!("cannot run git to remove the prior release: {error}"))?;
            if !removal.status.success() {
                return Err(format!(
                    "git could not remove the prior release worktree {}: {}",
                    prior.display(),
                    String::from_utf8_lossy(&removal.stderr).trim()
                ));
            }
        }
        Retirement::PlainDirectory => {
            std::fs::remove_dir_all(prior).map_err(|error| {
                format!(
                    "could not remove the prior release {}: {error}",
                    prior.display()
                )
            })?;
        }
    }
    Ok(planned)
}
