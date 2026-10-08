#![cfg(unix)]
//! The keeper binary, run as a real process against a real socket. Everything
//! here is about the things a library test cannot show: that the daemon binds,
//! that it keeps running when a worker goes away, and that it stops when it is
//! told to.
//!
//! The one that matters most is `a_worker_disconnect_does_not_stop_the_keeper`:
//! the keeper exists so a worker restart costs a reconnect, and a daemon that
//! exited on disconnect would have defeated the entire design while passing
//! every unit test in the crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::time::Duration;

use roost_keeper::codec::MuxFrameType;

use support::daemon::{Keeper, TempDir, wait_until};
use support::{empty_frame, spawn_frame};

/// The daemon binds its endpoint and answers a real spawn. This is the whole
/// binary working, end to end, as a separate process.
#[test]
fn the_daemon_binds_and_serves_a_real_spawn() {
    let temp = TempDir::new("serve");
    let mut keeper = Keeper::start(&temp);

    let mut client = keeper.connect();
    client.send(&spawn_frame(1, 80, 24));
    client.read_until("a spawn ack", |frames| {
        frames
            .iter()
            .any(|f| f.frame_type == MuxFrameType::SpawnAck)
    });
    assert!(!keeper.has_exited());
}

/// THE PROPERTY. A worker going away must NOT take the keeper with it: the
/// keeper exists so a worker restart costs a reconnect, not a terminal. A
/// daemon that exited here would have defeated the design while passing every
/// unit test in the crate.
#[test]
fn a_worker_disconnect_does_not_stop_the_keeper() {
    let temp = TempDir::new("disconnect");
    let mut keeper = Keeper::start(&temp);

    {
        let mut client = keeper.connect();
        client.send(&spawn_frame(7, 80, 24));
        client.read_until("a spawn ack", |frames| {
            frames
                .iter()
                .any(|f| f.frame_type == MuxFrameType::SpawnAck)
        });
        // The connection drops here, with a live channel.
    }

    std::thread::sleep(Duration::from_millis(200));
    assert!(
        !keeper.has_exited(),
        "a disconnect is not a shutdown; the keeper must outlive the worker"
    );
    assert!(
        keeper.socket().exists(),
        "and its endpoint must survive too"
    );
}

/// A second worker must find the channel the first one left, which is the
/// cross-process resume the whole `ListChannels` frame exists for.
#[test]
fn a_reconnecting_worker_finds_the_surviving_channel() {
    let temp = TempDir::new("resume");
    let keeper = Keeper::start(&temp);

    {
        let mut first = keeper.connect();
        first.send(&spawn_frame(5, 80, 24));
        first.read_until("a spawn ack", |frames| {
            frames
                .iter()
                .any(|f| f.frame_type == MuxFrameType::SpawnAck)
        });
    }

    let mut second = keeper.connect();
    second.send(&empty_frame(MuxFrameType::ListChannels, 0));
    let seen = second.read_until("the channel list", |frames| {
        frames
            .iter()
            .any(|f| f.frame_type == MuxFrameType::ListChannelsResp)
    });
    let listed: roost_keeper::frames::ListChannelsResp = seen
        .iter()
        .find(|f| f.frame_type == MuxFrameType::ListChannelsResp)
        .expect("the list is in what was read")
        .parse_json()
        .expect("it decodes");
    assert_eq!(
        listed.channels.len(),
        1,
        "the PTY survived the worker that spawned it: {:?}",
        listed.channels
    );
    assert_eq!(listed.channels[0].channel_id, 5);
}

/// A conditional shutdown with a live channel must leave the daemon running.
#[test]
fn a_conditional_shutdown_with_a_live_channel_keeps_the_daemon_up() {
    let temp = TempDir::new("condlive");
    let mut keeper = Keeper::start(&temp);

    let mut client = keeper.connect();
    client.send(&spawn_frame(1, 80, 24));
    client.read_until("a spawn ack", |frames| {
        frames
            .iter()
            .any(|f| f.frame_type == MuxFrameType::SpawnAck)
    });
    client.send(&empty_frame(MuxFrameType::ShutdownIfEmpty, 0));
    client.read_until("a refusal", |frames| {
        frames
            .iter()
            .any(|f| f.frame_type == MuxFrameType::ShutdownIfEmptyReject)
    });

    std::thread::sleep(Duration::from_millis(200));
    assert!(
        !keeper.has_exited(),
        "a refusal means the keeper holds live PTYs, so it must not retire"
    );
}

/// A conditional shutdown on an empty keeper must stop the daemon. This is the
/// automatic boot-replacement path, and it only works if the exit decision
/// actually reads the answer rather than the request.
#[test]
fn a_conditional_shutdown_on_an_empty_keeper_stops_the_daemon() {
    let temp = TempDir::new("condempty");
    let mut keeper = Keeper::start(&temp);

    let mut client = keeper.connect();
    client.send(&empty_frame(MuxFrameType::ShutdownIfEmpty, 0));
    client.read_until("an ack", |frames| {
        frames
            .iter()
            .any(|f| f.frame_type == MuxFrameType::ShutdownIfEmptyAck)
    });

    wait_until("the daemon to stop", || keeper.has_exited());
}

/// An unconditional shutdown stops the daemon, because it is the deliberate
/// offline maintenance path and it does not consult the channel table.
#[test]
fn an_unconditional_shutdown_stops_the_daemon() {
    let temp = TempDir::new("uncond");
    let mut keeper = Keeper::start(&temp);

    let mut client = keeper.connect();
    client.send(&empty_frame(MuxFrameType::Shutdown, 0));
    client.read_until("an ack", |frames| {
        frames
            .iter()
            .any(|f| f.frame_type == MuxFrameType::ShutdownAck)
    });

    wait_until("the daemon to stop", || keeper.has_exited());
}

/// A real terminal works through the daemon: bytes go in, bytes come back. This
/// is the one property the whole program exists to provide.
#[test]
fn a_real_terminal_works_through_the_daemon() {
    let temp = TempDir::new("terminal");
    let keeper = Keeper::start(&temp);

    let mut client = keeper.connect();
    client.send(&spawn_frame(1, 100, 30));
    client.read_until("a spawn ack", |frames| {
        frames
            .iter()
            .any(|f| f.frame_type == MuxFrameType::SpawnAck)
    });
    client.send(&support::input_frame(1, 1, b"through-the-daemon\r"));
    client.read_until("the echo", |frames| {
        frames.iter().any(|f| {
            f.frame_type == MuxFrameType::PtyOut
                && f.payload.windows(18).any(|w| w == b"through-the-daemon")
        })
    });
}

/// The spawn ack carries the CHILD's pid, not the daemon's, which is what
/// lets an operator find the process a channel is attached to. A daemon that
/// reported its own pid here would send every operator to the wrong process.
#[test]
fn a_spawn_ack_carries_a_real_child_pid() {
    let temp = TempDir::new("childpid");
    let mut keeper = Keeper::start(&temp);
    let daemon_pid = keeper.pid();
    assert!(!keeper.has_exited());

    let mut client = keeper.connect();
    client.send(&support::spawn_frame(1, 80, 24));
    let seen = client.read_until("a spawn ack", |frames| {
        frames
            .iter()
            .any(|f| f.frame_type == MuxFrameType::SpawnAck)
    });
    let ack: roost_keeper::frames::SpawnAck = seen
        .iter()
        .find(|f| f.frame_type == MuxFrameType::SpawnAck)
        .expect("the ack is in what was read")
        .parse_json()
        .expect("it decodes");
    assert_ne!(ack.pid, 0);
    assert_ne!(
        ack.pid,
        std::process::id(),
        "the child is not this test process"
    );
    assert_ne!(ack.pid, daemon_pid, "and not the daemon either");
}
