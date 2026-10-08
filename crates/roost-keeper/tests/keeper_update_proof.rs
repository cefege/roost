#![cfg(unix)]
//! What a keeper update rests on, against the real daemon: the `Hello` proves
//! the daemon's own pid and ONE process epoch across every connection, so a
//! worker restart finds the same keeper holding the same channels; and an
//! unconditional shutdown ends the daemon AND every PTY it held, even a child
//! that ignores the hangup. Ports the keeper half of v2
//! `apps/worker/tests/keeper-force-live-refresh.test.ts` and the pid/epoch
//! identity `keeper-update-action.test.ts` assumes.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::time::{Duration, Instant};

use roost_keeper::client::{KeeperClient, connect};
use roost_keeper::codec::MuxFrameType;
use roost_keeper::frames::ShellSpec;
use support::daemon::{Keeper, TempDir, wait_until};
use support::idle;

fn is_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks existence; the guard keeps it off 0 and -1.
    pid > 1 && unsafe { libc::kill(pid, 0) } == 0
}

/// A shell that ignores SIGHUP (inherited across the exec), says it is ready,
/// and then sleeps: the keeper's exit alone does not end it.
fn hangup_immune(marker: &str) -> ShellSpec {
    ShellSpec {
        program: "/bin/sh".into(),
        args: vec![
            "-c".into(),
            format!("trap '' HUP; printf '%s\\n' {marker}; exec /bin/sleep 60"),
        ],
        env: vec![("PATH".into(), "/usr/bin:/bin".into())],
        cwd: None,
    }
}

fn saw(client: &KeeperClient, channel_id: u16, needle: &str) -> bool {
    let start = Instant::now();
    let mut seen = String::new();
    while start.elapsed() < Duration::from_secs(8) {
        if let Some(frame) = client.next_event(Duration::from_millis(50))
            && frame.frame_type == MuxFrameType::PtyOut
            && frame.channel_id == channel_id
        {
            seen.push_str(&String::from_utf8_lossy(&frame.payload));
        }
        if seen.contains(needle) {
            return true;
        }
    }
    false
}

#[test]
fn the_hello_proves_one_keeper_identity_and_its_channels_across_reconnects() {
    let temp = TempDir::new("proof-identity");
    let keeper = Keeper::start(&temp);
    let first = connect(&keeper.endpoint()).expect("a handshake");
    let before = first.hello_response().expect("an authenticated hello");
    assert_eq!(
        before.pid,
        keeper.pid(),
        "the hello names the daemon's own pid"
    );
    let epoch = before
        .process_epoch
        .clone()
        .expect("the hello names its process epoch");
    roost_protocol::validate::uuid("process_epoch", &epoch).expect("the epoch is a uuid");
    first.spawn(3, idle(), 80, 24).expect("a spawn");
    first.spawn(4, idle(), 80, 24).expect("a spawn");
    let channels = first.list_channels().expect("the channels").channels;
    assert_eq!(channels.len(), 2);

    // A worker restart: its connection goes, a new one arrives.
    drop(first);
    let second = connect(&keeper.endpoint()).expect("a second handshake");
    let after = second.hello_response().expect("an authenticated hello");
    assert_eq!(after.pid, keeper.pid());
    assert_eq!(
        after.bindings, channels,
        "the hello names the channels that survived the reconnect"
    );
    assert_eq!(
        after.process_epoch,
        Some(epoch),
        "one epoch for the daemon's whole life"
    );
    assert_eq!(
        second.list_channels().expect("the channels").channels,
        channels
    );
}

#[test]
fn a_forced_shutdown_ends_the_keeper_and_every_pty_it_held() {
    let temp = TempDir::new("proof-forced");
    let mut keeper = Keeper::start(&temp);
    let client = connect(&keeper.endpoint()).expect("a handshake");
    let shell_pid = client
        .spawn(5, hangup_immune("ROOST_FORCE_LIVE_PTY"), 80, 24)
        .expect("a spawn");
    assert!(
        saw(&client, 5, "ROOST_FORCE_LIVE_PTY"),
        "the live PTY never printed"
    );

    // The empty-only shutdown refuses a keeper holding a live channel, and
    // ends this connection either way.
    assert!(!client.shutdown_if_empty().expect("an answer"));
    drop(client);
    assert!(is_alive(shell_pid), "a refused shutdown touched nothing");
    assert!(!keeper.has_exited());

    let client = connect(&keeper.endpoint()).expect("the keeper still serves");
    client
        .shutdown()
        .expect("the keeper acknowledges an unconditional shutdown");
    wait_until("the keeper exited", || keeper.has_exited());
    wait_until("the PTY's child was reaped with its keeper", || {
        !is_alive(shell_pid)
    });
}
