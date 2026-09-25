//! The keeper binary's command-line surface: what it prints, and what it does
//! when it cannot start. Split from the lifecycle tests because a failure here
//! is about how the daemon TALKS to whoever invoked it, not about what it does
//! once running.

use std::path::PathBuf;
use std::process::Command;

/// The keeper binary, built by cargo as part of this test run.
fn keeper_binary() -> PathBuf {
    // `CARGO_BIN_EXE_<name>` is set by cargo for integration tests of a crate
    // with binaries, so this is the binary cargo just built rather than a
    // guess at where it landed.
    PathBuf::from(env!("CARGO_BIN_EXE_roost-keeper"))
}

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

/// `--help` succeeds and says what the daemon is for.
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
}
