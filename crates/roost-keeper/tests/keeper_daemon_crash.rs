//! What a keeper daemon that DIED leaves behind, and what the next one makes of
//! it. Split from the lifecycle tests because a crash is a different event from
//! a clean stop, and the leftovers are a different kind of problem: a clean
//! stop cleans up after itself, and the interesting case is the one where it
//! could not.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use roost_keeper::codec::MuxFrameType;
use support::daemon::{Keeper, TempDir, wait_until};
use support::empty_frame;

/// A killed daemon leaves its socket file behind, because closing a listener
/// does not unlink its path. The next daemon must take it over, or one crash
/// would be a permanent "address in use".
#[test]
fn a_daemon_that_died_leaves_a_socket_the_next_one_reclaims() {
    let temp = TempDir::new("crash");
    {
        let mut crashed = Keeper::start(&temp);
        let pid = i32::try_from(crashed.pid()).expect("a child pid fits a pid_t");
        // SIGKILL leaves the socket file exactly as a crash does.
        // SAFETY: `kill` with a valid pid and SIGKILL is the documented use, and
        // the child is this test's own.
        unsafe { libc::kill(pid, libc::SIGKILL) };
        wait_until("the killed daemon to exit", || crashed.has_exited());
    }
    assert!(
        temp.socket().exists(),
        "a killed daemon leaves its socket file"
    );

    let mut replacement = Keeper::start(&temp);
    assert!(
        !replacement.has_exited(),
        "and the next daemon must take it over"
    );
    let mut worker = replacement.connect();
    worker.send(&empty_frame(MuxFrameType::Ping, 0));
    worker.read_until("a pong from the replacement", |frames| {
        frames
            .iter()
            .any(|frame| frame.frame_type == MuxFrameType::Pong)
    });
}
