//! What an INSTALLED service definition on the target says, and what a deploy
//! does about the release it names.
//!
//! Split from `deploy_remote_identity.rs`, which owns the per-key rule for which
//! machine a `ROOST_*` value describes. This file owns the other half of the
//! same question: once a value has been decided, it has to be READ back out of
//! the definition an earlier release wrote, in each platform's own format. A
//! deploy reads a definition written by a different build, which is why the
//! release directory comes from the platform's own directive rather than from
//! this build's idea of the layout — and why a reader that strips quotes, or
//! takes a dictionary's first value for a key's value, is a deploy that retires
//! a directory nothing is running from.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use roost_cli::deploy::codes;
use roost_cli::deploy::facts;
use roost_cli::deploy::installed::{
    installed_build_sha, installed_release_dir, launchd_program_argument, systemd_working_directory,
};
use roost_cli::deploy::retire::{Retirement, plan_retirement};
use roost_host::HostPlatform;
use serde_json::json;

/// A target's platform, definition path and release root are read out of ITS
/// definition, not derived from the deploying box's idea of where they live.
#[test]
fn the_installed_release_is_read_out_of_the_installed_definition() {
    let unit = "[Unit]\nDescription=x\nWorkingDirectory=/srv/roost/versions/b1d1836a/bin\n\
                ExecStart=\"/srv/roost/versions/b1d1836a/bin/roost\" worker\n\
                Environment=\"ROOST_GIT_SHA=b1d1836a\"\n";
    assert_eq!(
        systemd_working_directory(unit).as_deref(),
        Some("/srv/roost/versions/b1d1836a/bin")
    );
    assert_eq!(
        installed_release_dir(unit, HostPlatform::Linux).as_deref(),
        Some(Path::new("/srv/roost/versions/b1d1836a/bin"))
    );

    let plist = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<plist version=\"1.0\">\n\
<dict>\n\
\t<key>Label</key>\n\t<string>com.roost.worker-v3</string>\n\
\t<key>ProgramArguments</key>\n\t<array>\n\
\t\t<string>/srv/roost/versions/b1d1836a/bin/roost</string>\n\
\t\t<string>worker</string>\n\
\t</array>\n\
\t<key>EnvironmentVariables</key>\n\t<dict>\n\
\t\t<key>ROOST_GIT_SHA</key>\n\
\t\t<string>b1d1836a</string>\n\
\t</dict>\n\
</dict>\n</plist>\n";
    assert_eq!(
        launchd_program_argument(plist).as_deref(),
        Some("/srv/roost/versions/b1d1836a/bin/roost")
    );
    assert_eq!(
        installed_release_dir(plist, HostPlatform::MacOs).as_deref(),
        Some(Path::new("/srv/roost/versions/b1d1836a/bin"))
    );

    // The build stamp comes out of the same plist, and the environment reader is
    // the one shared with `roost status`: a reader that mistook the first
    // `<string>` inside `EnvironmentVariables` for a key's own value would drop
    // every variable after it, and a deploy would then exit 6 on every install
    // after the first.
    let environment = roost_cli::status::service_definition::parse_installed_environment(
        plist,
        HostPlatform::MacOs,
    );
    assert_eq!(
        installed_build_sha(&environment).as_deref(),
        Some("b1d1836a")
    );
}

/// `WorkingDirectory=` is NOT a quoted systemd directive. A reader that stripped
/// quotes from a raw value would report a path the unit never had, and settlement
/// would then look for a release directory that does not exist.
#[test]
fn a_working_directory_is_read_raw() {
    let unit = "WorkingDirectory=\"/srv/roost/versions/x/bin\"\n";
    assert_eq!(
        systemd_working_directory(unit).as_deref(),
        Some("\"/srv/roost/versions/x/bin\""),
        "a quoted value is quoted in the unit and stays that way here"
    );
}

/// The prior release is retired with git's own removal when it IS a registered
/// worktree, and with a plain removal when it is not — because every release a
/// real host has was staged by a copy, and asking git to remove a worktree fails
/// on exactly the machine that matters, at settlement, after the new release is
/// already serving.
#[test]
fn a_staged_prior_release_is_retired_without_being_a_worktree() {
    let root = tempdir("retire");
    let prior = root.join("b1d1836a");
    std::fs::create_dir_all(&prior).unwrap();
    assert_eq!(
        plan_retirement(&root, &prior, &[]).unwrap(),
        Retirement::PlainDirectory
    );
    assert_eq!(
        plan_retirement(&root, &prior, std::slice::from_ref(&prior)).unwrap(),
        Retirement::GitWorktree
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Confinement and the symlink refusal run BEFORE the worktree question, because
/// they are what makes a plain recursive removal safe at all.
#[test]
fn retirement_is_confined_to_the_release_root() {
    let root = tempdir("confine");
    let outside = root.parent().unwrap().join("roost-not-a-release");
    std::fs::create_dir_all(&outside).unwrap();
    let nested = root.join("b1d1836a").join("extra");
    std::fs::create_dir_all(&nested).unwrap();

    assert!(
        plan_retirement(&root, &root, &[]).is_err(),
        "the release root itself must never be retired"
    );
    assert!(
        plan_retirement(&root, &outside, &[]).is_err(),
        "a directory outside the release root must be refused"
    );
    assert!(
        plan_retirement(&root, &nested, &[]).is_err(),
        "a nested directory is not a release"
    );
    let link = root.join("linked");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    assert!(
        plan_retirement(&root, &link, &[]).is_err(),
        "a symlink could name something that is not a release"
    );
    assert!(
        plan_retirement(&root, &root.join("absent"), &[]).is_err(),
        "a release that is not there cannot be retired"
    );
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&outside);
}

/// The facts a target reports are read from its own definition, and a target
/// whose facts this build cannot read is a target whose paths this build would
/// guess — so the schema is checked rather than defaulted.
#[test]
fn facts_from_another_vocabulary_are_refused() {
    let line = format!(
        "{}{}",
        facts::FACTS_PREFIX,
        json!({"schema": 42, "platform": "linux"})
    );
    assert!(facts::decode(&line).is_err());
    assert!(
        facts::decode("uname: nothing here").is_err(),
        "silence is not a set of facts"
    );
}

/// The launcher for the installed release is shell that reads the two formats'
/// own conventions, and it says "not installed" with a code the caller can tell
/// from a transport failure.
#[test]
fn the_installed_launcher_finds_a_program_and_says_when_there_is_none() {
    let launcher = facts::installed_launcher("__remote-facts");
    assert!(
        launcher.contains("exec \"$program\" '__remote-facts'"),
        "got: {launcher}"
    );
    assert!(launcher.contains("exit 66"), "got: {launcher}");
    assert_eq!(facts::NO_INSTALLED_RELEASE, 66);
    // A program that is named but not executable is not an install, because a
    // release whose binary cannot run cannot report anything.
    assert!(launcher.contains("test -x \"$program\""), "got: {launcher}");
}

/// The exit codes are distinct and named, because a script that cannot tell a
/// "refused, do not retry" from a "failed, try again" retries the one thing that
/// must never be retried.
#[test]
fn the_reserved_exit_codes_are_distinct_and_named() {
    let reserved = [
        codes::USAGE,
        codes::SSH_UNREACHABLE,
        codes::NO_REMOTE_RUNTIME,
        codes::BUILD_FAILED,
        codes::KEEPER_NOT_ADOPTABLE,
        codes::NO_COORDINATOR_URL,
        codes::IDENTITY_UNPROVED,
        codes::SETTLEMENT_FAILED,
        codes::REMOTE_LOST,
    ];
    for (index, code) in reserved.iter().enumerate() {
        for other in &reserved[index + 1..] {
            assert_ne!(code, other, "two meanings share exit code {code}");
        }
    }
    assert_eq!(codes::KEEPER_NOT_ADOPTABLE, 5);
    assert_eq!(codes::NO_COORDINATOR_URL, 6);
    assert_eq!(codes::IDENTITY_UNPROVED, 7);
    assert_eq!(codes::SETTLEMENT_FAILED, 8);
    assert_eq!(codes::REMOTE_LOST, 9);
}

fn tempdir(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("roost-deploy-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}
