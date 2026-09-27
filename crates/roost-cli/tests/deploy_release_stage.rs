//! What `roost deploy` does to a target's staging directory, run through a real
//! shell rather than read as a string.
//!
//! The defect this file exists for is invisible to inspection. `mv` onto an
//! existing *directory* is a nesting move, not a replacement, so a command that
//! reads correctly — extract beside the destination, then rename — quietly
//! produces `<destination>/<name>.staging` and fails the moment the destination
//! is already there. It worked on the first deploy to a machine and failed on
//! every one after it, which is the shape of bug a test that runs the command
//! once cannot find.
//!
//! So every case here runs the generated command through `sh` for real.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;

use roost_cli::deploy::release_stage::staging_command;

/// Run the generated staging command for real, with a tar of `tree` on stdin.
///
/// `label` names the source tree, and it has to be distinct per test: the
/// harness runs these in parallel inside one process, so a shared source
/// directory is four tests writing over each other and every assertion after
/// the first is reading somebody else's bytes.
fn run_stage(label: &str, remote_release_dir: &Path, tree: &[(&str, &str)]) -> bool {
    let staging = tree_path(label, tree);
    let tar = Command::new("tar")
        .arg("-C")
        .arg(&staging)
        .args(["-cf", "-", "."])
        .output()
        .expect("tar runs");
    let output = Command::new("sh")
        .arg("-c")
        .arg(staging_command(&remote_release_dir.display().to_string()))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            use std::process::Stdio;
            let _ = child.stdin.take().unwrap().write_all(&tar.stdout);
            child.wait_with_output()
        })
        .expect("sh runs");
    output.status.success()
}

fn tree_path(label: &str, tree: &[(&str, &str)]) -> PathBuf {
    let root = tempdir(&format!("tree-{label}"));
    std::fs::create_dir_all(root.join("bin")).unwrap();
    for (name, body) in tree {
        std::fs::write(root.join(name), body).unwrap();
    }
    root
}

/// A first deploy: the destination does not exist and the release lands.
#[test]
fn a_release_lands_when_the_destination_is_absent() {
    let home = tempdir("home-first");
    let destination = home.join(".roost-deploy").join("b1d1836a");
    assert!(
        run_stage(
            "first",
            &destination,
            &[("bin/roost", "first"), ("bin/roost-keeper", "first-keeper")]
        ),
        "a first deploy must succeed"
    );
    assert_eq!(
        std::fs::read_to_string(destination.join("bin/roost")).unwrap(),
        "first"
    );
    assert!(
        !destination.join(&format!("b1d1836a.staging")).exists(),
        "the temporary directory is renamed into place, not left beside it"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// The defect. A destination left over from an earlier attempt makes `mv` nest
/// instead of replace, and the deploy fails on every machine it has ever been
/// run to twice.
#[test]
fn a_re_deploy_replaces_the_destination_instead_of_nesting_inside_it() {
    let home = tempdir("home-redeploy");
    let destination = home.join(".roost-deploy").join("b1d1836a");
    let payload = || [("bin/roost", "first"), ("bin/roost-keeper", "first-keeper")];

    assert!(
        run_stage("redeploy-a", &destination, &payload()),
        "the first deploy succeeds"
    );
    // The leftover: something is already sitting at the destination.
    assert!(destination.join("bin/roost").is_file());

    // Now deploy different bytes to the same build identity, which is what a
    // re-run after a failed apply looks like.
    assert!(
        run_stage(
            "redeploy-b",
            &destination,
            &[
                ("bin/roost", "second"),
                ("bin/roost-keeper", "second-keeper")
            ]
        ),
        "a re-deploy must replace the destination, not fail on it"
    );
    assert_eq!(
        std::fs::read_to_string(destination.join("bin/roost")).unwrap(),
        "second",
        "the release is replaced, not merged with what was there"
    );
    assert!(
        !destination.join("b1d1836a.staging").exists(),
        "nothing is nested inside the destination: {:?}",
        std::fs::read_dir(&destination)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// A half-extracted temporary directory from a target that lost power is
/// cleared, not appended to: a release is wholly there or wholly absent.
#[test]
fn a_half_extracted_temporary_directory_does_not_contaminate_the_release() {
    let home = tempdir("home-torn");
    let destination = home.join(".roost-deploy").join("b1d1836a");
    let leftover = home.join(".roost-deploy").join("b1d1836a.staging");
    std::fs::create_dir_all(leftover.join("bin")).unwrap();
    std::fs::write(leftover.join("bin/roost"), "torn").unwrap();

    assert!(
        run_stage(
            "torn",
            &destination,
            &[("bin/roost", "clean"), ("bin/roost-keeper", "clean-keeper")]
        ),
        "a leftover temporary directory must not fail the deploy"
    );
    assert_eq!(
        std::fs::read_to_string(destination.join("bin/roost")).unwrap(),
        "clean",
        "the torn copy is discarded, not shipped beside the real one"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// A home with a space in it is the case a hardcoded path gets wrong, and the
/// destination is a value this command must not split.
#[test]
fn a_destination_with_a_space_is_quoted_and_lands_intact() {
    let home = tempdir("ho me");
    let destination = home.join(".roost-deploy").join("b1d1836a");
    assert!(
        run_stage(
            "spaced",
            &destination,
            &[
                ("bin/roost", "spaced"),
                ("bin/roost-keeper", "spaced-keeper")
            ]
        ),
        "a home with a space must still stage"
    );
    assert_eq!(
        std::fs::read_to_string(destination.join("bin/roost")).unwrap(),
        "spaced"
    );
    let _ = std::fs::remove_dir_all(&home);
}

fn tempdir(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("roost-stage-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}
