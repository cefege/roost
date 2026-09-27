//! The checkout a fleet ROLLBACK builds from, and the publish step a push takes
//! before it rolls anything. Called by the push command; depends on git and on
//! nothing else in the push group.
//!
//! A rollback ships the commit the fleet is leaving, and that commit is by
//! definition not the tip of anything — the push just published a newer one. So
//! it cannot be deployed out of the operator's checkout, and the only honest
//! source for it is a detached worktree at that exact commit. The worktree is
//! created on demand rather than up front: a push that never rolls back must not
//! leave a checkout behind, and creating one is not free on a large repository.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::command_error::CommandFailure;
use crate::deploy::codes;

/// A detached checkout of one commit, created only if a rollback needs it.
#[derive(Debug, Clone)]
pub struct RollbackCheckout {
    repository: PathBuf,
    commit: String,
    path: PathBuf,
}

impl RollbackCheckout {
    /// Where the checkout for `commit` will live, named after the commit so two
    /// rollbacks of different commits never share one.
    pub fn plan(service_dir: &Path, repository: &Path, commit: &str) -> Self {
        Self {
            repository: repository.to_path_buf(),
            commit: commit.to_string(),
            path: service_dir.join("rollback-checkouts").join(short(commit)),
        }
    }

    /// The checkout, creating it the first time and reusing it after that.
    pub async fn ensure(&self) -> Result<PathBuf, CommandFailure> {
        if self.path.join("Cargo.toml").is_file() {
            return Ok(self.path.clone());
        }
        let _ = std::fs::remove_dir_all(&self.path);
        let rendered = self.path.display().to_string();
        git(
            &self.repository,
            &[
                "worktree",
                "add",
                "--quiet",
                "--force",
                "--detach",
                &rendered,
                &self.commit,
            ],
            "stage the release a fleet rollback restores",
        )
        .await?;
        Ok(self.path.clone())
    }

    /// Remove the worktree and its administrative record, if one was created.
    pub async fn discard(&self) {
        if !self.path.exists() {
            return;
        }
        let rendered = self.path.display().to_string();
        if let Err(failure) = git(
            &self.repository,
            &["worktree", "remove", "--force", &rendered],
            "remove the release a fleet rollback restored",
        )
        .await
        {
            tracing::warn!(
                reason = %failure.message,
                path = %self.path.display(),
                "a rollback checkout is still on disk; `git worktree prune` will clear it"
            );
        }
    }
}

/// Publish this checkout's current branch to its own configured upstream.
///
/// `git push` with no arguments rather than a composed `git push <remote>
/// HEAD:<ref>`: the upstream is the checkout's own configuration, and a push
/// that named a ref the operator did not configure is a push to somewhere they
/// did not choose. The caller proves the result with the deploy group's own
/// published-tip proof, so a push that went nowhere is caught rather than
/// assumed.
pub async fn publish(repository: &Path) -> Result<(), CommandFailure> {
    let status = tokio::process::Command::new("git")
        .arg("push")
        .current_dir(repository)
        .stdin(Stdio::null())
        .status()
        .await
        .map_err(|error| {
            codes::refuse(
                codes::IDENTITY_UNPROVED,
                format!("publish this commit: cannot run git: {error}"),
            )
        })?;
    if status.success() {
        return Ok(());
    }
    Err(codes::refuse(
        codes::IDENTITY_UNPROVED,
        "git push did not succeed; nothing was rolled out, because a commit that is not published \
         is a commit the next fetch cannot find",
    ))
}

/// One git invocation. A failure here is exit 7, because every use of it is a
/// question about what may be shipped.
async fn git(
    repository: &Path,
    args: &[&str],
    label: &str,
) -> Result<String, CommandFailure> {
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(repository)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|error| {
            codes::refuse(
                codes::IDENTITY_UNPROVED,
                format!("{label}: cannot run git: {error}"),
            )
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(codes::refuse(
            codes::IDENTITY_UNPROVED,
            format!(
                "{label}: {}",
                if stderr.trim().is_empty() {
                    format!("git {} did not succeed", args.join(" "))
                } else {
                    stderr.trim().to_string()
                }
            ),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn short(commit: &str) -> String {
    commit.chars().take(12).collect()
}
