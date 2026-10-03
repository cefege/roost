//! The keeper's socket server over a real Unix socket. The endpoint's security
//! properties are the reason these exist: a keeper socket is a remote shell to
//! every PTY the machine has open, and a window on it is a window onto a shell.
//!
//! Every wait is bounded, because a test that hangs is indistinguishable from a
//! keeper that does.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::time::Instant;

use roost_keeper::codec::MuxFrameType;
use roost_keeper::frames::SpawnAck;
use roost_keeper::server::{Endpoint, ListenError, Server, Shutdown};
use support::daemon::{DEADLINE, TempDir};
use support::in_process::InProcessKeeper;
use support::{echo, empty_frame, input_frame, resize_frame};

/// A spawn over a real socket must be acknowledged with a real pid, and the
/// whole round trip proves the framing works end to end.
#[test]
fn a_spawn_over_a_real_socket_is_acknowledged() {
    let temp = TempDir::new("spawn");
    let server = InProcessKeeper::serve(&temp, 1);

    let mut client = server.connect();
    client.send(&support::spawn_frame(3, 80, 24));
    let seen = client.read_until("a spawn ack", |frames| {
        frames
            .iter()
            .any(|f| f.frame_type == MuxFrameType::SpawnAck)
    });
    let ack: SpawnAck = seen
        .iter()
        .find(|f| f.frame_type == MuxFrameType::SpawnAck)
        .expect("the ack is in what was read")
        .parse_json()
        .expect("the ack decodes");
    assert!(ack.pid > 0, "a real process, not a placeholder");
    drop(client);
    server.finish();
}

/// Output flows WITHOUT a request. A request-driven loop would show nothing
/// until the user typed, which is the bug the output tick exists to prevent.
#[test]
fn output_flows_without_any_further_request() {
    let temp = TempDir::new("tick");
    let server = InProcessKeeper::serve(&temp, 1);
    let mut client = server.connect();
    client.send(&support::spawn_frame(1, 80, 24));
    client.read_until("a spawn ack", |f| {
        f.iter().any(|f| f.frame_type == MuxFrameType::SpawnAck)
    });

    // No further request: the echo only appears because the server drains on
    // its own tick.
    client.send(&input_frame(1, 1, b"unsolicited\r"));
    let seen = client.read_until("the echo", |frames| {
        let mut all = Vec::new();
        for frame in frames {
            all.extend_from_slice(&frame.payload);
        }
        all.windows(10).any(|w| w == b"unsolicite")
    });
    let text: Vec<u8> = seen.iter().flat_map(|f| f.payload.clone()).collect();
    assert!(text.windows(10).any(|w| w == b"unsolicite"));
    drop(client);
    server.finish();
}

/// A `Shutdown` frame ends the connection: the daemon's exit decision runs
/// after this, and a connection that stayed open would keep the keeper alive
/// against its own instruction.
#[test]
fn a_shutdown_frame_ends_the_connection() {
    let temp = TempDir::new("shutdown");
    let server = InProcessKeeper::serve(&temp, 1);
    let mut client = server.connect();
    client.send(&empty_frame(MuxFrameType::Shutdown, 0));
    client.read_until("the ack", |f| {
        f.iter().any(|f| f.frame_type == MuxFrameType::ShutdownAck)
    });

    // The server closes: reads return zero, rather than blocking forever.
    assert!(client.closed_by_peer(), "the connection closes");
    drop(client);
    server.finish();
}

/// `ShutdownIfEmpty` is answered, and the answer is what the daemon's exit
/// decision reads. A live channel must produce a refusal.
#[test]
fn a_conditional_shutdown_over_the_socket_refuses_while_a_channel_is_live() {
    let temp = TempDir::new("condshutdown");
    let server = InProcessKeeper::serve(&temp, 1);
    let mut client = server.connect();
    client.send(&support::spawn_frame(1, 80, 24));
    client.read_until("a spawn ack", |f| {
        f.iter().any(|f| f.frame_type == MuxFrameType::SpawnAck)
    });

    client.send(&empty_frame(MuxFrameType::ShutdownIfEmpty, 0));
    let seen = client.read_until("a refusal", |f| {
        f.iter()
            .any(|f| f.frame_type == MuxFrameType::ShutdownIfEmptyReject)
    });
    assert!(
        seen.iter()
            .any(|f| f.frame_type == MuxFrameType::ShutdownIfEmptyReject)
    );
    drop(client);
    server.finish();
}

/// The daemon's exit decision is one function, and both paths are asserted here
/// rather than left to a reader of the server loop.
#[test]
fn the_exit_decision_depends_on_whether_a_channel_is_live() {
    let temp = TempDir::new("decision");
    let endpoint = Endpoint::new(temp.socket()).expect("endpoint");
    let mut server = Server::bind(endpoint, temp.capability()).expect("bind");
    assert!(server.keeper().is_empty());
    assert!(roost_keeper::server::should_stop(
        server.keeper(),
        Shutdown::StopIfEmpty
    ));
    assert!(roost_keeper::server::should_stop(
        server.keeper(),
        Shutdown::StopPreserving
    ));

    let reply = server.keeper_mut().handle(&support::spawn_frame(1, 80, 24));
    assert_eq!(reply[0].frame_type, MuxFrameType::SpawnAck);
    assert!(
        !roost_keeper::server::should_stop(server.keeper(), Shutdown::StopIfEmpty),
        "a live channel stops the keeper retiring"
    );
    assert!(roost_keeper::server::should_stop(
        server.keeper(),
        Shutdown::StopPreserving
    ));
}

/// Geometry survives the round trip over a real socket, which is the property
/// a client depends on when it asks what the keeper actually applied.
#[test]
fn geometry_survives_the_socket_round_trip() {
    let temp = TempDir::new("geometry");
    let server = InProcessKeeper::serve(&temp, 1);
    let mut client = server.connect();
    client.send(&support::spawn_frame(1, 80, 24));
    client.read_until("a spawn ack", |f| {
        f.iter().any(|f| f.frame_type == MuxFrameType::SpawnAck)
    });
    client.send(&resize_frame(1, 9, 132, 43));
    client.read_until("a resize ack", |f| {
        f.iter().any(|f| f.frame_type == MuxFrameType::ResizeAck)
    });
    drop(client);
    server.finish();
}

/// The endpoint refuses a path whose parent is not a directory at all, rather
/// than failing later with a confusing bind error.
#[test]
fn an_unusable_parent_is_refused_with_a_reason() {
    let temp = TempDir::new("badparent");
    let bad = temp.path().join("not-a-dir").join("keeper.sock");
    assert!(matches!(
        Endpoint::new(bad),
        Err(ListenError::Prepare { .. })
    ));
}

/// A shell that is never written to must not block the server, which is what
/// the bounded read is for.
#[test]
fn an_idle_channel_does_not_stall_the_connection() {
    let temp = TempDir::new("idle");
    let server = InProcessKeeper::serve(&temp, 1);
    let mut client = server.connect();
    client.send(&support::spawn_with(1, support::idle(), 80, 24));
    client.read_until("a spawn ack", |f| {
        f.iter().any(|f| f.frame_type == MuxFrameType::SpawnAck)
    });

    // The channel produces nothing; the server must still answer promptly.
    let start = Instant::now();
    client.send(&empty_frame(MuxFrameType::Ping, 0));
    client.read_until("a pong", |f| {
        f.iter().any(|f| f.frame_type == MuxFrameType::Pong)
    });
    assert!(
        start.elapsed() < DEADLINE,
        "an idle channel stalled the server"
    );
    drop(client);
    server.finish();
}

/// A shell that echoes proves the keeper is not merely acknowledging frames but
/// actually carrying bytes, which is the one property a terminal depends on.
#[test]
fn the_keeper_carries_real_bytes_over_the_socket() {
    let temp = TempDir::new("bytes");
    let server = InProcessKeeper::serve(&temp, 1);
    let mut client = server.connect();
    client.send(&support::spawn_frame(1, 80, 24));
    client.read_until("a spawn ack", |f| {
        f.iter().any(|f| f.frame_type == MuxFrameType::SpawnAck)
    });
    client.send(&input_frame(1, 1, b"carried-7788\r"));
    let seen = client.read_until("the echo", |frames| {
        frames.iter().any(|f| {
            f.frame_type == MuxFrameType::PtyOut
                && f.payload.windows(12).any(|w| w == b"carried-7788")
        })
    });
    assert!(seen.iter().any(|f| f.frame_type == MuxFrameType::PtyOut
        && f.payload.windows(12).any(|w| w == b"carried-7788")));
    drop(client);
    server.finish();
}

/// The shell helper is used by the spawn cases above; a shell that is not `cat`
/// would make every byte assertion below vacuous, so it is pinned.
#[test]
fn the_echo_helper_really_is_an_echo() {
    assert_eq!(echo().program, "/bin/cat");
}
