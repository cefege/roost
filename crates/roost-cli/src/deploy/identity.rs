//! The three proofs that what this deploy would ship is the thing it claims to
//! be, and the refusals (all exit 7) when it is not. Called by the deploy
//! command before anything on the target is touched; depends on git and on the
//! installed coordinator definition, and on nothing else in the deploy group.
//!
//! The reason a deploy refuses to proceed on an unproved build is not tidiness.
//! The SHA is what the worker stamps into its heartbeat and what the
//! coordinator's fleet roster compares, so a deploy that ships tree A while
//! stamping tree B produces a machine that reports a build it is not running —
//! and a machine that reports a build it is not running can never earn keeper
//! update admission, which is how a fleet slowly becomes unupdatable.
//!
//! Each proof answers a different question about the same value, and the caller
//! picks by where the deploy was started from:
//!
//! - [`local_git_sha_or_die`]: what is in this checkout, dirty state included.
//! - [`published_git_sha_or_die`]: is it the exact tip of its upstream. The
//!   default for a deploy an operator started by hand.
//! - [`coordinator_release_git_sha_or_die`]: is this checkout the installed
//!   coordinator's own release, running the build the coordinator says it runs.
//!   The default for a deploy a coordinator started, whose checkout is a
//!   detached release worktree with no upstream to be the tip of.

use std::path::Path;
use std::process::Stdio;

use roost_host::HostPlatform;
use tokio::process::Command;

use crate::command_error::CommandFailure;
use crate::deploy::codes;
use crate::deploy::installed::{installed_build_sha, installed_release_dir};
use crate::status::service_definition::parse_installed_environment;

/// The environment that turns a dirty tree into an allowed deploy. The stamp
/// gains a `-dirty` suffix so a fleet roster can still see that the build is
/// not what the commit says.
pub const ALLOW_DIRTY_ENV: &str = "ROOST_ALLOW_DIRTY";

/// The suffix a dirty stamp carries, and the marker a published proof refuses.
pub const DIRTY_SUFFIX: &str = "-dirty";

/// A 40-character lowercase hex commit, which is what git and the fleet roster
/// both speak.
fn is_commit_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// This checkout's HEAD, with the dirty guard every deploy path shares.
///
/// A dirty tree is refused unless `ROOST_ALLOW_DIRTY=1`, because otherwise the
/// shipped tree and the stamp disagree. With the variable set the stamp is
/// suffixed rather than left bare, so nothing downstream can mistake it for a
/// published build.
pub async fn local_git_sha_or_die(cwd: &Path) -> Result<String, CommandFailure> {
    let sha = git(cwd, &["rev-parse", "HEAD"], "resolve the source commit").await?;
    if !is_commit_sha(&sha) {
        return Err(unproved(format!(
            "git rev-parse returned {sha:?}, which is not a commit identity"
        )));
    }
    let status = git(
        cwd,
        &["status", "--porcelain"],
        "verify the source working tree",
    )
    .await?;
    if status.trim().is_empty() {
        return Ok(sha);
    }
    if std::env::var(ALLOW_DIRTY_ENV).as_deref() == Ok("1") {
        return Ok(format!("{sha}{DIRTY_SUFFIX}"));
    }
    Err(unproved(format!(
        "uncommitted changes in working tree {}.\n\
         Commit first, OR re-run with {ALLOW_DIRTY_ENV}=1 to ship the dirty state with a \
         `{DIRTY_SUFFIX}` stamp. Run `git status` to see what is pending.",
        cwd.display()
    )))
}

/// Prove this checkout is a clean commit that is the exact tip of its own
/// refreshed upstream, and that it is the build the caller expected.
///
/// The tip check is the one that catches the ordinary accident: a commit made
/// and not pushed deploys fine, reports fine, and is lost the next time anyone
/// fetches. `--expected-sha` is what a caller that already knows the build adds
/// as a second, independent check.
pub async fn published_git_sha_or_die(
    cwd: &Path,
    expected_sha: Option<&str>,
) -> Result<String, CommandFailure> {
    let sha = local_git_sha_or_die(cwd).await?;
    if sha.ends_with(DIRTY_SUFFIX) {
        return Err(unproved(
            "a published deploy requires a clean committed source snapshot",
        ));
    }
    if let Some(expected) = expected_sha
        && !sha.eq_ignore_ascii_case(expected)
    {
        return Err(unproved(format!(
            "source HEAD {} does not match required build {}",
            short(&sha),
            short(expected)
        )));
    }
    let target = publish_target(cwd).await?;
    git(
        cwd,
        &[
            "fetch",
            "--quiet",
            "--no-tags",
            "--",
            &target.remote,
            &target.merge_ref,
        ],
        "refresh source upstream",
    )
    .await?;
    let remote_sha = git(
        cwd,
        &["rev-parse", "FETCH_HEAD"],
        "resolve refreshed source upstream",
    )
    .await?;
    if !is_commit_sha(&remote_sha) {
        return Err(unproved(format!(
            "the refreshed upstream resolved to {remote_sha:?}, which is not a commit identity"
        )));
    }
    if !remote_sha.eq_ignore_ascii_case(&sha) {
        return Err(unproved(format!(
            "source HEAD {} is not the exact refreshed upstream tip {}",
            short(&sha),
            short(&remote_sha)
        )));
    }
    Ok(sha)
}

/// Prove this checkout IS the installed coordinator's release, running exactly
/// the build the installed definition says it runs, with a clean HEAD there.
///
/// This is the proof a coordinator-started deploy has to use instead of the
/// upstream one: a coordinator installed by `roost push` runs from a detached
/// release directory with no branch, so asking it for a publishable upstream
/// asks a question whose answer is always "no" — and treating that as a dirty
/// tree sends a machine that is perfectly deployable into a permanent
/// "Update available". The authority is the installed service definition, which
/// names both the directory and the build.
pub async fn coordinator_release_git_sha_or_die(
    cwd: &Path,
    expected_sha: &str,
    definition_path: &Path,
    platform: HostPlatform,
) -> Result<String, CommandFailure> {
    let definition = std::fs::read_to_string(definition_path).map_err(|error| {
        unproved(format!(
            "coordinator release proof: service definition {} is unreadable: {error}",
            definition_path.display()
        ))
    })?;
    let installed = parse_installed_environment(&definition, platform);
    let installed_release = installed_release_dir(&definition, platform).ok_or_else(|| {
        unproved(
            "coordinator release proof: the installed coordinator definition names no release \
             directory",
        )
    })?;
    let source = real_path(cwd)?;
    let release = real_path(&installed_release)?;
    if source != release {
        return Err(unproved(format!(
            "coordinator release proof: source {} is not the installed coordinator release {}",
            cwd.display(),
            release.display()
        )));
    }
    match installed_build_sha(&installed) {
        Some(installed_sha) if installed_sha.eq_ignore_ascii_case(expected_sha) => {}
        Some(installed_sha) => {
            return Err(unproved(format!(
                "coordinator release proof: installed coordinator runs {}, not {}",
                short(&installed_sha),
                short(expected_sha)
            )));
        }
        None => {
            return Err(unproved(
                "coordinator release proof: the installed coordinator definition stamps no \
                 build, so there is nothing to match the required build against",
            ));
        }
    }
    let sha = local_git_sha_or_die(cwd).await?;
    if sha.ends_with(DIRTY_SUFFIX) {
        return Err(unproved(
            "coordinator release proof: the coordinator release tree is not clean",
        ));
    }
    if !sha.eq_ignore_ascii_case(expected_sha) {
        return Err(unproved(format!(
            "coordinator release proof: source HEAD {} does not match required build {}",
            short(&sha),
            short(expected_sha)
        )));
    }
    Ok(sha.to_ascii_lowercase())
}

/// A checkout's publishable upstream, resolved from its branch's configuration
/// rather than guessed.
struct PublishTarget {
    remote: String,
    merge_ref: String,
}

async fn publish_target(cwd: &Path) -> Result<PublishTarget, CommandFailure> {
    let branch = git(
        cwd,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        "source HEAD has no publishable branch",
    )
    .await?;
    let remote = git(
        cwd,
        &["config", "--get", &format!("branch.{branch}.remote")],
        "source branch has no configured remote",
    )
    .await?;
    let merge_ref = git(
        cwd,
        &["config", "--get", &format!("branch.{branch}.merge")],
        "source branch has no configured upstream ref",
    )
    .await?;
    if !merge_ref.starts_with("refs/heads/")
        || merge_ref.contains(char::is_whitespace)
        || ["~", "^", ":", "?", "*", "[", "\\"]
            .iter()
            .any(|marker| merge_ref.contains(marker))
    {
        return Err(unproved(
            "source branch upstream is not a publishable remote branch",
        ));
    }
    Ok(PublishTarget { remote, merge_ref })
}

/// One git invocation, with the refusal text naming what the step was for.
async fn git(cwd: &Path, args: &[&str], label: &str) -> Result<String, CommandFailure> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|error| unproved(format!("{label}: cannot run git: {error}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(unproved(format!(
            "{label}: {}",
            if stderr.trim().is_empty() {
                format!("git {} exited {}", args.join(" "), exit_of(&output.status))
            } else {
                stderr.trim().to_string()
            }
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn exit_of(status: &std::process::ExitStatus) -> String {
    status
        .code()
        .map_or_else(|| "on a signal".to_string(), |code| code.to_string())
}

fn real_path(path: &Path) -> Result<std::path::PathBuf, CommandFailure> {
    std::fs::canonicalize(path).map_err(|error| {
        unproved(format!(
            "coordinator release proof: cannot resolve {}: {error}",
            path.display()
        ))
    })
}

/// The eight characters an operator sees in a mismatch message. A full SHA in a
/// refusal makes the message unreadable and the two halves impossible to compare
/// by eye.
fn short(sha: &str) -> String {
    sha.chars().take(8).collect()
}

fn unproved(message: impl Into<String>) -> CommandFailure {
    codes::refuse(codes::IDENTITY_UNPROVED, message)
}
