//! Which machine each `ROOST_*` value in an installed definition describes, and
//! what a deploy is allowed to carry forward from one. The companion to
//! `deploy_release_path.rs`, which owns the ssh boundary and the release path;
//! this file owns the per-key rule, because that rule is the one a future reader
//! has to find when a machine comes up under the wrong name.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use roost_cli::deploy::codes;
use roost_cli::deploy::facts::{self, RemoteFacts};
use roost_cli::deploy::apply_release::staging_dir;
use roost_cli::deploy::identity_env::{
    self, EnvTarget, resolve_deploy_env_value, resolve_remote_deploy_identity,
};
use roost_cli::services::service_environment::{
    ENV_BOOTSTRAP_TOKEN, ENV_REACHABLE_ADDR, ENV_WORKER_LABEL,
};
use roost_host::build_identity::ROOST_GIT_SHA_ENV;
use roost_platform::KEEPER_FORCE_LIVE_RETIRE_ENV;
use roost_worker::runtime::boot::ENV_COORDINATOR_URL;
use roost_cli::deploy::installed::{
    installed_build_sha, installed_release_dir, launchd_program_argument,
    systemd_working_directory,
};
use roost_cli::deploy::manifest::{ApplyManifest, ApplyOutcome, ApplyReport};
use roost_cli::deploy::release::release_digest;
use roost_cli::deploy::retire::{Retirement, plan_retirement};
use roost_cli::deploy::ssh::{self, REMOTE_PATH_PREFIX, SSH_OPTS, read_remote_file};
use roost_cli::status::service_definition::parse_installed_environment;
use roost_host::HostPlatform;
use roost_protocol::keeper_update::KeeperContractV1;
use serde_json::json;

/// The two identity keys resolve from the invocation or the target's own
/// installed definition and from NOWHERE else. A remote deploy that adopted the
/// deploying shell's value installs the DEPLOYING box's label on the target, and
/// the coordinator then lists two workers under one name.
#[test]
fn an_identity_key_never_resolves_from_the_deploying_shell() {
    let ambient: BTreeMap<String, String> =
        BTreeMap::from([(ENV_WORKER_LABEL.to_string(), "this-box".to_string())]);
    let installed = BTreeMap::new();
    assert_eq!(
        resolve_deploy_env_value(
            ENV_WORKER_LABEL,
            &installed,
            None,
            EnvTarget::Remote,
            &ambient
        ),
        None
    );
    // A `self` deploy is the one case where the ambient value IS the target's.
    assert_eq!(
        resolve_deploy_env_value(
            ENV_WORKER_LABEL,
            &installed,
            None,
            EnvTarget::ThisHost,
            &ambient
        )
        .as_deref(),
        Some("this-box")
    );
    // The invocation flag still wins on a remote deploy.
    assert_eq!(
        resolve_deploy_env_value(
            ENV_WORKER_LABEL,
            &installed,
            Some("studio"),
            EnvTarget::Remote,
            &ambient
        )
        .as_deref(),
        Some("studio")
    );
    // And so does the target's own installed definition.
    let installed = BTreeMap::from([(ENV_WORKER_LABEL.to_string(), "studio".to_string())]);
    assert_eq!(
        resolve_deploy_env_value(
            ENV_WORKER_LABEL,
            &installed,
            None,
            EnvTarget::Remote,
            &ambient
        )
        .as_deref(),
        Some("studio")
    );
    // A fleet key keeps its ambient fallback: it describes no one machine.
    let ambient: BTreeMap<String, String> = BTreeMap::from([(
        ENV_COORDINATOR_URL.to_string(),
        "https://coord.test".to_string(),
    )]);
    assert_eq!(
        resolve_deploy_env_value(
            ENV_COORDINATOR_URL,
            &BTreeMap::new(),
            None,
            EnvTarget::Remote,
            &ambient
        )
        .as_deref(),
        Some("https://coord.test")
    );
}

/// An ambient identity export over a fresh target is a REFUSAL that names the
/// flag, not a guess about which machine the operator meant.
#[test]
fn an_ambient_identity_export_refuses_and_names_the_flag() {
    let ambient: BTreeMap<String, String> =
        BTreeMap::from([(ENV_WORKER_LABEL.to_string(), "this-box".to_string())]);
    let failure = resolve_remote_deploy_identity("studio", &BTreeMap::new(), None, None, &ambient)
        .expect_err("an ambient identity over a fresh target must refuse");
    assert_eq!(failure.code, codes::NO_COORDINATOR_URL);
    assert!(
        failure.message.contains(ENV_WORKER_LABEL),
        "got: {}",
        failure.message
    );
    assert!(
        failure.message.contains("--label"),
        "got: {}",
        failure.message
    );
    assert!(
        failure.message.contains("studio"),
        "the refusal must name the target: got: {}",
        failure.message
    );

    // With the flag supplied, the same shell resolves and the deploy proceeds.
    let resolved = resolve_remote_deploy_identity(
        "studio",
        &BTreeMap::new(),
        Some("studio"),
        Some("studio.test:4113"),
        &ambient,
    )
    .expect("an explicit identity resolves");
    assert_eq!(
        resolved.get(ENV_WORKER_LABEL).map(String::as_str),
        Some("studio")
    );
    assert_eq!(
        resolved.get(ENV_REACHABLE_ADDR).map(String::as_str),
        Some("studio.test:4113")
    );
}

/// Unresolvable identity with no ambient export is not an error: the worker
/// derives its own hostname, which is the documented fresh-target path.
#[test]
fn an_unresolvable_identity_with_nothing_exported_is_allowed() {
    let resolved =
        resolve_remote_deploy_identity("studio", &BTreeMap::new(), None, None, &BTreeMap::new())
            .expect("a fresh target with nothing exported is the normal case");
    assert!(resolved.is_empty());
}

/// Neither one-shot grant is ever carried into a definition. A definition that
/// still carried the retire authorization would re-authorize destroying a
/// keeper's live channels on every later restart.
#[test]
fn a_deploy_never_carries_a_one_shot_grant_forward() {
    let installed = BTreeMap::from([
        (ENV_BOOTSTRAP_TOKEN.to_string(), "secret".to_string()),
        (KEEPER_FORCE_LIVE_RETIRE_ENV.to_string(), "1".to_string()),
        (
            ENV_COORDINATOR_URL.to_string(),
            "https://old.test".to_string(),
        ),
        (
            roost_host::build_identity::ROOST_GIT_SHA_ENV.to_string(),
            "oldsha".to_string(),
        ),
    ]);
    let values = identity_env::worker_install_environment(
        &installed,
        &BTreeMap::new(),
        "b1d1836a",
        &BTreeMap::new(),
    );
    assert!(!values.contains_key(ENV_BOOTSTRAP_TOKEN));
    assert!(!values.contains_key(KEEPER_FORCE_LIVE_RETIRE_ENV));
    // The build stamp is not carried forward, it is REPLACED: the definition has
    // to name the build this deploy installed, and carrying the old one forward
    // would make a machine report a build it is not running.
    assert_eq!(
        values
            .get(roost_host::build_identity::ROOST_GIT_SHA_ENV)
            .map(String::as_str),
        Some("b1d1836a")
    );
    assert_eq!(
        values.get(ENV_COORDINATOR_URL).map(String::as_str),
        Some("https://old.test"),
        "a fleet key the operator set is preserved"
    );
    assert_eq!(values.get("GIT_SHA").map(String::as_str), Some("b1d1836a"));

    // …and an override that names a grant is still dropped, because the grant is
    // a one-shot and this function is not the place that arms one.
    let values = identity_env::worker_install_environment(
        &BTreeMap::new(),
        &BTreeMap::from([(ENV_BOOTSTRAP_TOKEN.to_string(), "secret".to_string())]),
        "b1d1836a",
        &BTreeMap::new(),
    );
    assert!(!values.contains_key(ENV_BOOTSTRAP_TOKEN));
}

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
\t\t<string>worker</string>\n\t</array>\n\
\t<key>EnvironmentVariables</key>\n\t<dict>\n\
\t\t<key>ROOST_GIT_SHA</key>\n\t\t<string>b1d1836a</string>\n\t</dict>\n\
</dict>\n</plist>\n";
    assert_eq!(
        launchd_program_argument(plist).as_deref(),
        Some("/srv/roost/versions/b1d1836a/bin/roost")
    );
    assert_eq!(
        installed_release_dir(plist, HostPlatform::MacOs).as_deref(),
        Some(Path::new("/srv/roost/versions/b1d1836a/bin"))
    );

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
