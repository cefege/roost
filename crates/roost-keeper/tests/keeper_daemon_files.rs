//! What the keeper daemon PUBLISHES on the filesystem: the endpoint's
//! permissions, the pid file's permissions, and the pid file's lifetime. Split
//! from the lifecycle tests because a failure in one is a permissions or
//! cleanup regression, and they are worth reading as a group.
//!
//! A keeper socket is a remote shell to every PTY on the machine. A readable
//! pid file tells any local process where to send a signal. Both are
//! filesystem properties, which is why they are tested without a conversation.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::os::unix::fs::PermissionsExt;

use roost_keeper::codec::MuxFrameType;
use support::daemon::{Keeper, TempDir, wait_until};
use support::{empty_frame, input_frame, spawn_frame};

/// Ask the daemon to stop and wait for it to close the connection, which is
/// what a shutdown produces.
fn request_shutdown(keeper: &Keeper) {
    let mut client = keeper.connect();
    client.send(&empty_frame(MuxFrameType::Shutdown, 0));
    client.read_until("the shutdown ack", |frames| {
        frames
            .iter()
            .any(|frame| frame.frame_type == MuxFrameType::ShutdownAck)
    });
    assert!(client.closed_by_peer(), "a shutdown closes the connection");
}

/// The endpoint is owner-only from the moment it exists. A keeper socket is a
/// remote shell to every PTY on the machine, so a world-readable one is a shell
/// for anyone on the box.
#[test]
fn the_daemon_publishes_an_owner_only_endpoint() {
    let temp = TempDir::new("perms");
    let _keeper = Keeper::start(&temp);
    let mode = std::fs::metadata(temp.socket())
        .expect("the socket")
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o777,
        0o600,
        "the socket is owner-only, not {mode:o}"
    );
}

/// The pid file is owner-only for the same reason, one step removed: a readable
/// pid file tells any local process where to send a signal.
#[test]
fn the_daemon_writes_an_owner_only_pid_file() {
    let temp = TempDir::new("pidperm");
    let keeper = Keeper::start_with_pid_file(&temp);
    let mode = std::fs::metadata(temp.pid_file())
        .expect("the pid file")
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o777,
        0o600,
        "the pid file is owner-only, not {mode:o}"
    );
    let recorded = std::fs::read_to_string(temp.pid_file()).expect("the pid file is readable");
    assert_eq!(
        recorded.trim(),
        keeper.pid().to_string(),
        "and names this daemon"
    );
}

/// The pid file is published BEFORE the socket, so a worker that can connect can
/// always already find the process it just connected to. The other order leaves
/// a window in which the pid is not yet readable, which is exactly the moment
/// tooling needs it.
#[test]
fn the_pid_file_exists_before_the_socket_is_published() {
    let temp = TempDir::new("ordering");
    let _keeper = Keeper::start_with_pid_file(&temp);
    // `start` returns only once the socket is connectable. The pid file being
    // present at that moment is the ordering claim, asserted.
    assert!(
        temp.pid_file().exists(),
        "the pid file must already be there"
    );
    assert!(temp.socket().exists());
}

/// A clean stop takes the pid file with it, so a stale pid cannot point a
/// service manager at a process that no longer exists.
#[test]
fn a_clean_stop_removes_the_pid_file() {
    let temp = TempDir::new("pidclean");
    let mut keeper = Keeper::start_with_pid_file(&temp);
    assert!(temp.pid_file().exists());
    assert!(!keeper.has_exited());

    request_shutdown(&keeper);
    wait_until("the daemon to stop", || keeper.has_exited());
    wait_until("a stopped daemon to remove its pid file", || {
        !temp.pid_file().exists()
    });
}

/// A clean stop also takes the endpoint, so the next daemon does not have to
/// decide whether the leftover is stale.
#[test]
fn a_clean_stop_removes_the_socket() {
    let temp = TempDir::new("sockclean");
    let keeper = Keeper::start(&temp);
    assert!(temp.socket().exists());

    request_shutdown(&keeper);
    wait_until("a stopped daemon to remove its socket", || {
        !temp.socket().exists()
    });
}

/// The daemon runs fine with no pid file at all, because a pid file is a
/// convenience for tooling and refusing to own PTYs over one would be worse
/// than running without it.
#[test]
fn a_pid_file_is_optional() {
    let temp = TempDir::new("nopid");
    let mut keeper = Keeper::start(&temp);
    assert!(
        temp.socket().exists(),
        "the daemon must start without a pid file"
    );
    assert!(!keeper.has_exited(), "and must stay running");
    assert!(!temp.pid_file().exists(), "and it wrote none");
}

/// A spawn's input still reaches the PTY with no pid file configured, which is
/// the same "it works without it" claim from the other side.
#[test]
fn a_terminal_works_without_a_pid_file() {
    let temp = TempDir::new("nopidterm");
    let keeper = Keeper::start(&temp);
    let mut client = keeper.connect();
    client.send(&spawn_frame(1, 80, 24));
    client.send(&input_frame(1, 1, b"no-pid-file\r"));
    client.read_until("the echo", |frames| {
        frames.iter().any(|frame| {
            frame.frame_type == MuxFrameType::PtyOut
                && frame.payload.windows(11).any(|w| w == b"no-pid-file")
        })
    });
}
