//! The keeper binary's command-line surface: what it prints, and what it does
//! when it cannot start. Split from the lifecycle tests because a failure here
//! is about how the daemon TALKS to whoever invoked it, not about what it does
//! once running.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::process::Command;

use support::daemon::{TempDir, keeper_binary};

/// A missing `--socket` is a usage error with a non-zero status, not a panic
/// and not a silent default. A keeper with a guessed socket path is a keeper
/// nobody can find.
#[test]
fn a_missing_socket_argument_is_a_usage_error() {
    let output = Command::new(keeper_binary())
        .output()
        .expect("the binary runs");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--socket is required"), "{stderr}");
    assert!(stderr.contains("USAGE"), "and must print how to use it");
}

/// A keeper with no capability would have to serve every peer that reached its
/// socket, so a missing `--capability-file` is a usage error before anything
/// binds, never an open keeper.
#[test]
fn a_missing_capability_file_is_a_usage_error() {
    let temp = TempDir::new("cli-nocap");
    let output = Command::new(keeper_binary())
        .arg("--socket")
        .arg(temp.socket())
        .output()
        .expect("the binary runs");
    assert_eq!(output.status.code(), Some(2), "a usage error exits 2");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--capability-file"), "{stderr}");
    assert!(
        !temp.socket().exists(),
        "and nothing was bound before the refusal"
    );
}

/// `--help` succeeds and says what the daemon is for and what it needs.
#[test]
fn help_succeeds_and_explains_itself() {
    let output = Command::new(keeper_binary())
        .arg("--help")
        .output()
        .expect("the binary runs");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("roost-keeper"), "{stdout}");
    assert!(stdout.contains("--socket"), "{stdout}");
    assert!(stdout.contains("--capability-file"), "{stdout}");
}
