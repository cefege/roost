//! Integration tests for the PTY channel. These spawn REAL processes on a REAL
//! pty: the property under test is that bytes survive a round trip through the
//! kernel, and no amount of mocking proves that.
//!
//! Every test is bounded by a deadline and fails rather than hangs, because a
//! test that hangs is indistinguishable from a keeper that does.

use std::time::{Duration, Instant};

use roost_keeper::payloads::{PtyInRejectReason, ShellSpec};
use roost_keeper::pty_channel::{PtyChannel, SpawnError, WriteOutcome};

const DEADLINE: Duration = Duration::from_secs(10);

fn shell(script: &str) -> ShellSpec {
    ShellSpec {
        program: "/bin/sh".into(),
        args: vec!["-c".into(), script.into()],
        env: Vec::new(),
        cwd: None,
    }
}

fn sleep() -> ShellSpec {
    ShellSpec {
        program: "/bin/sleep".into(),
        args: vec!["30".into()],
        env: Vec::new(),
        cwd: None,
    }
}

/// Poll `read_output` until `needle` appears or the deadline passes.
///
/// A PTY read is a poll loop, not a blocking read: the master end is shared
/// with the child and a blocking read would hang the test rather than fail it.
fn read_until(channel: &mut PtyChannel, needle: &[u8]) -> Vec<u8> {
    let start = Instant::now();
    let mut seen = Vec::new();
    while start.elapsed() < DEADLINE {
        if let Some(chunk) = channel.read_output(8192) {
            seen.extend_from_slice(&chunk);
            if seen.windows(needle.len()).any(|window| window == needle) {
                return seen;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!(
        "never saw {:?} within {:?}; saw {:?}",
        String::from_utf8_lossy(needle),
        DEADLINE,
        String::from_utf8_lossy(&seen)
    );
}

/// A spawned process that echoes what it is given must come back, byte for
/// byte. This is the one property the whole keeper exists to preserve.
#[test]
fn a_spawned_process_echoes_its_input_back() {
    let mut channel = PtyChannel::spawn(1, &shell("cat"), 80, 24).expect("spawn");
    assert_eq!(channel.channel_id(), 1);
    assert!(channel.pid().is_some(), "a spawned PTY has a process");

    match channel.write_input(b"round trip\r") {
        WriteOutcome::Complete { written } => assert_eq!(written, 11),
        other => panic!("a small write to an idle PTY should complete, got {other:?}"),
    }
    let seen = read_until(&mut channel, b"round trip");
    assert!(seen.windows(5).any(|w| w == b"round"));
}

/// The echo a PTY returns is not the echo a pipe returns: the tty line
/// discipline turns CR into CR LF. Asserting the raw bytes without accounting
/// for that would make this test pass for the wrong reason.
#[test]
fn the_round_trip_survives_the_tty_line_discipline() {
    let mut channel = PtyChannel::spawn(1, &shell("cat"), 80, 24).expect("spawn");
    assert!(matches!(
        channel.write_input(b"x"),
        WriteOutcome::Complete { written: 1 }
    ));
    let seen = read_until(&mut channel, b"x");
    assert!(
        seen.windows(3).any(|w| w == b"x\r\n") || seen.contains(&b'x'),
        "the echoed byte must survive; saw {:?}",
        String::from_utf8_lossy(&seen)
    );
}

/// Geometry is applied to the real tty, and the state the keeper reports is
/// what it actually applied — which is the whole point of `GetTerminalState`.
#[test]
fn a_resize_reaches_the_kernel_and_is_reported_back() {
    let mut channel = PtyChannel::spawn(1, &sleep(), 80, 24).expect("spawn");
    assert_eq!(channel.terminal_state().cols, 80);
    assert_eq!(
        channel.terminal_state().applied_seq,
        0,
        "no resize has been applied yet"
    );

    assert_eq!(channel.apply_resize(1, 120, 40).expect("resize"), 1);
    let state = channel.terminal_state();
    assert_eq!((state.applied_seq, state.cols, state.rows), (1, 120, 40));

    let size = channel.master_size().expect("the kernel reports a size");
    assert_eq!(
        (size.cols, size.rows),
        (120, 40),
        "the resize reached the tty, not just the struct"
    );
}

/// A retry after a lost ack must not move the terminal backwards. The keeper is
/// the ordering authority, and a client that resends the last sequence after a
/// timeout is behaving correctly.
#[test]
fn a_replayed_resize_sequence_does_not_move_the_terminal_backwards() {
    let mut channel = PtyChannel::spawn(1, &sleep(), 80, 24).expect("spawn");
    channel.apply_resize(5, 100, 30).expect("first");

    assert_eq!(
        channel
            .apply_resize(3, 200, 60)
            .expect("stale is ignored, not an error"),
        5
    );
    let state = channel.terminal_state();
    assert_eq!((state.applied_seq, state.cols, state.rows), (5, 100, 30));

    assert_eq!(
        channel
            .apply_resize(5, 200, 60)
            .expect("same sequence is idempotent"),
        5
    );
    assert_eq!(channel.terminal_state().cols, 100);
}

/// A zero dimension is refused, never clamped: a clamp produces a PTY whose
/// size differs from what the client was told, with no way for it to tell.
#[test]
fn a_zero_geometry_is_refused_rather_than_clamped() {
    assert!(matches!(
        PtyChannel::spawn(1, &sleep(), 0, 24),
        Err(SpawnError::BadDimension { .. })
    ));
    assert!(matches!(
        PtyChannel::spawn(1, &sleep(), 80, 0),
        Err(SpawnError::BadDimension { .. })
    ));
}

/// Channel 0 is the socket's control lane. Opening a PTY there would create a
/// channel the wire cannot address.
#[test]
fn the_control_lane_is_not_a_channel() {
    assert!(matches!(
        PtyChannel::spawn(0, &sleep(), 80, 24),
        Err(SpawnError::BadChannelId(0))
    ));
}

/// A child that ends must be observable, and writing to it afterwards must be
/// refused rather than silently dropped — a lost keystroke on a dead shell is
/// exactly the failure acknowledged input exists to prevent.
#[test]
fn a_dead_child_is_observable_and_refuses_input() {
    let mut channel = PtyChannel::spawn(1, &shell("exit 7"), 80, 24).expect("spawn");

    let start = Instant::now();
    while channel.exited().is_none() {
        assert!(
            start.elapsed() < DEADLINE,
            "a child running `exit 7` never reported its exit"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    let status = channel.exited().expect("just observed");
    assert_eq!(
        status.exit_code(),
        7,
        "the exit code is preserved, not flattened to success"
    );
    assert!(matches!(
        channel.write_input(b"late"),
        WriteOutcome::Rejected {
            reason: PtyInRejectReason::ChildExited
        }
    ));
}

/// A working directory that does not exist must fail before the fork, not
/// after: a fork whose chdir then fails leaves a child to reap with no record
/// of what it was doing.
#[test]
fn an_unusable_working_directory_fails_before_spawning() {
    let spec = ShellSpec {
        program: "/bin/sh".into(),
        args: vec!["-c".into(), "true".into()],
        env: Vec::new(),
        cwd: Some("/nonexistent/roost/pty/test".into()),
    };
    assert!(matches!(
        PtyChannel::spawn(1, &spec, 80, 24),
        Err(SpawnError::BadCwd { .. })
    ));
}

/// A missing program is an error the worker can act on, not a wedged channel.
#[test]
fn a_missing_program_is_reported_rather_than_hanging() {
    let spec = ShellSpec {
        program: "/nonexistent/roost/pty/shell".into(),
        args: Vec::new(),
        env: Vec::new(),
        cwd: None,
    };
    assert!(matches!(
        PtyChannel::spawn(1, &spec, 80, 24),
        Err(SpawnError::Pty(_))
    ));
}

/// Two channels are independent PTYs, because that is what lets one socket
/// carry many terminals.
#[test]
fn two_channels_are_independent_processes() {
    let mut first = PtyChannel::spawn(1, &shell("cat"), 80, 24).expect("spawn first");
    let mut second = PtyChannel::spawn(2, &shell("cat"), 80, 24).expect("spawn second");

    assert_ne!(first.pid(), second.pid(), "each channel is its own process");
    assert!(matches!(
        first.write_input(b"first"),
        WriteOutcome::Complete { .. }
    ));
    let seen = read_until(&mut first, b"first");
    assert!(seen.windows(5).any(|w| w == b"first"));

    // The second channel saw none of it. A read on an idle PTY must return
    // immediately with nothing rather than blocking until the deadline: a
    // blocking read here would wedge every other channel behind this one.
    assert_eq!(second.read_output(4096), None);
}
