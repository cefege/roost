//! The build-identity proof a coordinator-started deploy uses instead of the
//! upstream one.
//!
//! `roost push` installs a coordinator from a `git worktree add --detach`
//! release directory. There is no branch there, so asking that checkout for a
//! publishable upstream asks a question whose answer is always "no" — and
//! answering "no" with the dirty-tree refusal pins every machine behind the
//! coordinator at "Update available" forever, with a deploy that looks like it
//! has nothing to say. The authority for that proof is the INSTALLED service
//! definition, which names both the directory and the build, and the three
//! refusals below are the three ways that authority can be absent.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;

use roost_cli::deploy::codes;
use roost_cli::deploy::identity::coordinator_release_git_sha_or_die;
use roost_host::HostPlatform;

/// A checkout that is a real repository at a real commit, with no branch and no
/// remote: exactly what `git worktree add --detach` leaves behind.
fn detached_release(label: &str) -> (PathBuf, String) {
    let root = std::env::temp_dir().join(format!(
        "roost-coord-release-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(&root)
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    };
    git(&["init", "--quiet"]);
    git(&["config", "user.email", "deploy@example.test"]);
    git(&["config", "user.name", "Deploy Test"]);
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::write(root.join("marker"), "release\n").unwrap();
    git(&["add", "marker"]);
    git(&["commit", "--quiet", "-m", "release"]);
    let sha = git(&["rev-parse", "HEAD"]);
    // Detach, and prove there is no branch to be the tip of.
    git(&["checkout", "--quiet", "--detach", "HEAD"]);
    // `git symbolic-ref -q HEAD` exits 1 on a detached HEAD, which is the fact
    // being asserted, so it is asked without the success assertion the rest of
    // this helper makes.
    let symbolic = Command::new("git")
        .args(["symbolic-ref", "-q", "HEAD"])
        .current_dir(&root)
        .output()
        .expect("git runs");
    assert!(
        !symbolic.status.success() && symbolic.stdout.is_empty(),
        "a detached release has no symbolic HEAD to be the tip of"
    );
    (root, sha)
}

/// The coordinator's installed definition, naming the release directory and the
/// build it runs.
fn installed_coordinator_unit(release: &Path, build: &str) -> String {
    format!(
        "[Unit]\nDescription=Roost coordinator\n\n[Service]\n\
         WorkingDirectory={}\n\
         ExecStart=\"{}/bin/roost\" coord\n\
         Environment=\"ROOST_GIT_SHA={build}\"\n",
        release.display(),
        release.display()
    )
}

fn write_unit(root: &Path, text: &str) -> PathBuf {
    let path = root.join("roost3-coord.service");
    std::fs::write(&path, text).unwrap();
    path
}

async fn prove(
    release: &Path,
    expected: &str,
    definition: &Path,
) -> Result<String, roost_cli::command_error::CommandFailure> {
    coordinator_release_git_sha_or_die(release, expected, definition, HostPlatform::Linux).await
}

/// The defect this entry records: a coordinator installed from a detached
/// release worktree is asked for the upstream tip it cannot have, and is refused
/// with a message that reads like a dirty tree. The installed definition is the
/// authority instead, and a checkout that IS that release, running exactly the
/// build the definition stamps, is admitted with no upstream anywhere in sight.
#[tokio::test]
async fn a_detached_coordinator_release_at_its_installed_sha_is_admitted() {
    let (release, sha) = detached_release("admit");
    let unit = write_unit(&release, &installed_coordinator_unit(&release, &sha));
    let proved = prove(&release, &sha, &unit)
        .await
        .expect("a detached release at its installed SHA is admissible");
    assert_eq!(proved, sha.to_ascii_lowercase());
    // No remote is configured, which is the whole point: the upstream proof
    // would have failed here, and this one never asks.
    let remotes = Command::new("git")
        .args(["remote"])
        .current_dir(&release)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&remotes.stdout).trim().is_empty(),
        "the release has no remote, so no upstream exists to be the tip of"
    );
    let _ = std::fs::remove_dir_all(&release);
}

/// The wrong checkout: a source tree that is not the installed release is
/// refused rather than shipped, because its build is not what the coordinator
/// the fleet talks to is running.
#[tokio::test]
async fn a_checkout_that_is_not_the_installed_release_is_refused() {
    let (release, sha) = detached_release("wrong-checkout");
    let other =
        std::env::temp_dir().join(format!("roost-coord-release-other-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&other);
    std::fs::create_dir_all(&other).unwrap();
    let unit = write_unit(&release, &installed_coordinator_unit(&release, &sha));
    let failure = prove(&other, &sha, &unit)
        .await
        .expect_err("a checkout that is not the installed release must refuse");
    assert_eq!(failure.code, codes::IDENTITY_UNPROVED);
    assert!(
        failure
            .message
            .contains("is not the installed coordinator release"),
        "got: {}",
        failure.message
    );
    let _ = std::fs::remove_dir_all(&release);
    let _ = std::fs::remove_dir_all(&other);
}

/// The wrong build: the definition stamps something other than the required
/// build, so the coordinator would deploy a fleet to a different commit than the
/// one it is itself running.
#[tokio::test]
async fn an_installed_build_that_is_not_the_required_one_is_refused() {
    let (release, sha) = detached_release("wrong-build");
    let unit = write_unit(
        &release,
        &installed_coordinator_unit(&release, &format!("{sha}0")),
    );
    let failure = prove(&release, &sha, &unit)
        .await
        .expect_err("a mismatched installed build must refuse");
    assert_eq!(failure.code, codes::IDENTITY_UNPROVED);
    assert!(
        failure.message.contains("installed coordinator runs"),
        "got: {}",
        failure.message
    );
    let _ = std::fs::remove_dir_all(&release);
}

/// A dirty release tree ships a different tree than the commit it stamps, so it
/// is refused here rather than producing a machine that reports a build it is
/// not running — the failure that makes a fleet slowly unupdatable.
#[tokio::test]
async fn a_dirty_release_tree_is_refused() {
    let (release, sha) = detached_release("dirty");
    let unit = write_unit(&release, &installed_coordinator_unit(&release, &sha));
    std::fs::write(release.join("uncommitted"), "x\n").unwrap();
    let failure = prove(&release, &sha, &unit)
        .await
        .expect_err("a dirty release must refuse");
    assert_eq!(failure.code, codes::IDENTITY_UNPROVED);
    assert!(
        failure.message.contains("uncommitted changes"),
        "got: {}",
        failure.message
    );
    let _ = std::fs::remove_dir_all(&release);
}

/// A definition that stamps no build at all has nothing to match the required
/// build against, and is its own answer rather than a fallback to the upstream
/// proof this path exists to avoid.
#[tokio::test]
async fn a_definition_that_stamps_no_build_is_refused() {
    let (release, sha) = detached_release("no-stamp");
    let unit = write_unit(
        &release,
        &format!(
            "[Unit]\nDescription=Roost coordinator\n\n[Service]\n\
             WorkingDirectory={}\nExecStart=\"{}/bin/roost\" coord\n",
            release.display(),
            release.display()
        ),
    );
    let failure = prove(&release, &sha, &unit)
        .await
        .expect_err("a definition with no build stamp must refuse");
    assert_eq!(failure.code, codes::IDENTITY_UNPROVED);
    assert!(
        failure.message.contains("stamps no build"),
        "got: {}",
        failure.message
    );
    let _ = std::fs::remove_dir_all(&release);
}
