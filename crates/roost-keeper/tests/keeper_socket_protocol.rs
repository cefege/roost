//! How the keeper behaves over a real socket when the conversation is about
//! the PROTOCOL rather than about a terminal: a tag it predates, a shutdown, an
//! exit. Split from the byte-carrying cases because a failure here is a framing
//! or lifecycle bug, not a terminal bug.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_keeper::codec::MuxFrameType;
use roost_keeper::frames::ExitFrame;

mod support;

use support::daemon::TempDir;
use support::empty_frame;
use support::in_process::InProcessKeeper;

/// An unknown tag must not end the connection. It is how a newer keeper says it
/// has a frame this build predates, and the length has already been read, so
/// skipping it is safe and dropping a live terminal over it is not.
#[test]
fn an_unknown_tag_does_not_end_the_connection() {
    let temp = TempDir::new("unknowntag");
    let server = InProcessKeeper::serve(&temp, 1);
    let mut client = server.connect();
    // A well-formed frame carrying a tag this build has never heard of.
    let mut raw = Vec::new();
    raw.extend_from_slice(&4u32.to_be_bytes());
    raw.push(0xEF);
    raw.extend_from_slice(&0u16.to_be_bytes());
    raw.push(0xAA);
    client.send_raw(&raw).expect("a write");

    // The connection still works afterwards.
    client.send(&empty_frame(MuxFrameType::Ping, 0));
    client.read_until("a pong", |f| {
        f.iter().any(|f| f.frame_type == MuxFrameType::Pong)
    });
    drop(client);
    server.finish();
}

/// An exit is reported over the socket once the channel's output has drained,
/// so a client never loses the last thing a process printed.
#[test]
fn an_exit_is_reported_over_the_socket() {
    let temp = TempDir::new("exit");
    let server = InProcessKeeper::serve(&temp, 1);
    let mut client = server.connect();
    client.send(&support::spawn_with(
        1,
        support::running("echo farewell; exit 5"),
        80,
        24,
    ));
    client.read_until("a spawn ack", |f| {
        f.iter().any(|f| f.frame_type == MuxFrameType::SpawnAck)
    });

    let seen = client.read_until("the exit", |frames| {
        let said = frames.iter().any(|f| {
            f.frame_type == MuxFrameType::PtyOut && f.payload.windows(8).any(|w| w == b"farewell")
        });
        said && frames.iter().any(|f| f.frame_type == MuxFrameType::Exit)
    });
    let exit_frame = seen
        .iter()
        .find(|f| f.frame_type == MuxFrameType::Exit)
        .expect("the exit is in what was read");
    let exit: ExitFrame = exit_frame.parse_json().expect("it decodes");
    assert_eq!(exit.exit_code, Some(5));
    drop(client);
    server.finish();
}
