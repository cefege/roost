//! `install.sh`: which job a run does. A grant makes it `roost join`; its
//! absence makes it `roost quickstart`, with the arguments after `bash -s --`;
//! a coordinator URL without a grant installs nothing. Runs the real script
//! against the sandbox in `install_script_support`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod install_script_support;

use std::path::Path;

use install_script_support::{
    Invocation, Sandbox, install_script, run_script_against, write_fake_roost,
};

/// A first install: with no grant, the fetched release runs `quickstart`, and
/// the arguments after `bash -s --` reach it unchanged. A run that joined
/// instead would enroll against nothing; a run that dropped the arguments would
/// turn `--dry-run` into a real install.
#[test]
fn without_a_grant_the_fetched_release_runs_quickstart_with_the_arguments() {
    let sandbox = Sandbox::new("quickstart");
    let (api, origin) = sandbox.publish_fake_release();
    let empty_path = sandbox.root.join("empty-bin");
    std::fs::create_dir_all(&empty_path).expect("an empty PATH entry");

    let (ok, _stdout, stderr) = run_script_against(
        &sandbox,
        &empty_path,
        &api,
        &origin,
        &Invocation {
            grant: false,
            args: &["--dry-run"],
            channel: None,
        },
    );

    assert!(
        ok,
        "a first install with a published release succeeds: {stderr}"
    );
    let execed = std::fs::read_to_string(sandbox.log()).unwrap_or_default();
    let mut words = execed.split_whitespace().skip(1);
    let staged = words.next().expect("the staged roost was run");
    assert_eq!(
        words.collect::<Vec<_>>(),
        ["quickstart", "--dry-run"],
        "no grant means quickstart, with the arguments passed through: {execed}"
    );
    assert!(
        !Path::new(staged).exists(),
        "the staged pair is removed once quickstart returns: {staged}"
    );
}

/// A door with no grant is a pasted join line that lost half of itself. Setting
/// up a coordinator in its place would be the wrong machine, installed quietly,
/// so nothing runs at all.
#[test]
fn a_coordinator_url_without_a_grant_installs_nothing() {
    let sandbox = Sandbox::new("half-a-join");
    let path_dir = sandbox.root.join("usr-local-bin");
    write_fake_roost(&sandbox.home().join(".local/bin/roost"), true);

    let output = std::process::Command::new("bash")
        .arg(install_script())
        .env_remove("ROOST_BOOTSTRAP_TOKEN")
        .env("HOME", sandbox.home())
        .env("PATH", sandbox.path(&path_dir))
        .env("ROOST_COORDINATOR_URL", "https://coordinator.example")
        .env("ROOST_TEST_JOIN_LOG", sandbox.log())
        .stdin(std::process::Stdio::null())
        .output()
        .expect("bash runs the script");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "half a join line must fail");
    assert_eq!(
        std::fs::read_to_string(sandbox.log()).unwrap_or_default(),
        "",
        "neither join nor quickstart runs on half a join line"
    );
    assert!(
        stderr.contains("ROOST_BOOTSTRAP_TOKEN"),
        "the refusal names the missing half: {stderr}"
    );
}
