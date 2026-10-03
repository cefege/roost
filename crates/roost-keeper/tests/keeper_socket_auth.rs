//! The keeper serves nothing to a connection that has not proved the
//! capability: the first frame must be a verifying `Hello`, sent within the
//! byte cap and the timeout, and until then no frame is answered and no PTY
//! byte is written. A keeper socket is a shell for whoever can drive it, so
//! these are the security boundary of every terminal on the machine.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::time::Instant;

use roost_keeper::capability::KeeperCapability;
use roost_keeper::codec::{MuxFrame, MuxFrameType};
use roost_keeper::frames::ListChannelsResp;
use roost_keeper::payloads::KEEPER_PROTOCOL_VERSION;
use roost_keeper::server::{ConnectionEnd, UNAUTHENTICATED_MAX_BYTES, UNAUTHENTICATED_TIMEOUT};
use support::daemon::{Client, DEADLINE, TempDir};
use support::in_process::InProcessKeeper;
use support::{echo, empty_frame, hello_frame, idle, running, spawn_frame, spawn_with};

/// A well-formed 64-hex value that is not `capability`.
fn other_secret(capability: &KeeperCapability) -> String {
    let mut secret = capability.as_str().as_bytes().to_vec();
    secret[0] = if secret[0] == b'0' { b'1' } else { b'0' };
    String::from_utf8(secret).expect("hex is utf-8")
}

/// Send a spawn for `channel_id` over an authenticated connection and wait for
/// its ack, so the channel exists before the next connection arrives.
fn spawn_and_wait(client: &mut Client, channel_id: u16, frame: &MuxFrame) {
    client.send(frame);
    client.read_until("a spawn ack", |frames| {
        frames.iter().any(|frame| {
            frame.frame_type == MuxFrameType::SpawnAck && frame.channel_id == channel_id
        })
    });
}

/// A connection whose first frame is not a `Hello` is closed unanswered, its
/// frame is never acted on, and the keeper goes on serving the next worker.
#[test]
fn a_first_frame_that_is_not_a_hello_is_refused() {
    let temp = TempDir::new("auth-not-hello");
    let server = InProcessKeeper::serve(&temp, 2);

    let mut intruder = server.connect_unauthenticated();
    intruder.send(&spawn_frame(1, 80, 24));
    assert_eq!(
        intruder.read_until_closed(),
        Some(Vec::new()),
        "a refused connection is closed without a byte"
    );
    assert_eq!(server.next_end(), ConnectionEnd::NotAuthenticated);

    let mut worker = server.connect();
    assert!(
        worker.hello_response().bindings.is_empty(),
        "the refused spawn created nothing"
    );
    spawn_and_wait(&mut worker, 2, &spawn_frame(2, 80, 24));
    drop(worker);
    assert_eq!(server.next_end(), ConnectionEnd::ClientDisconnected);
    server.finish();
}

/// A `Hello` presenting any other secret is closed with no `HelloResp`.
#[test]
fn a_hello_with_the_wrong_capability_is_refused() {
    let temp = TempDir::new("auth-wrong-cap");
    let server = InProcessKeeper::serve(&temp, 1);

    let mut intruder = server.connect_unauthenticated();
    intruder.send(&hello_frame(&other_secret(server.capability())));
    assert_eq!(
        intruder.read_until_closed(),
        Some(Vec::new()),
        "no HelloResp, nor anything else"
    );
    assert_eq!(server.next_end(), ConnectionEnd::NotAuthenticated);
    server.finish();
}

/// A connection that says nothing is closed once the authentication timeout
/// has passed, and not before: it is what stops one silent peer holding the
/// single-connection keeper away from its worker forever.
#[test]
fn a_silent_connection_is_closed_at_the_timeout() {
    let temp = TempDir::new("auth-silent");
    let server = InProcessKeeper::serve(&temp, 1);

    let connecting = Instant::now();
    let mut silent = server.connect_unauthenticated();
    let received = silent
        .read_until_closed()
        .expect("the keeper closes a silent connection");
    let waited = connecting.elapsed();

    assert!(received.is_empty(), "nothing was written to it");
    assert!(
        waited >= UNAUTHENTICATED_TIMEOUT,
        "closed after {waited:?}, before the {UNAUTHENTICATED_TIMEOUT:?} timeout"
    );
    assert!(
        waited < UNAUTHENTICATED_TIMEOUT + DEADLINE,
        "closed only after {waited:?}"
    );
    assert_eq!(server.next_end(), ConnectionEnd::NotAuthenticated);
    server.finish();
}

/// A peer streaming bytes that never complete a first frame is closed once it
/// passes the byte cap, well before the timer could have done it.
#[test]
fn a_connection_past_the_byte_cap_is_closed_before_the_timeout() {
    let temp = TempDir::new("auth-flood");
    let server = InProcessKeeper::serve(&temp, 1);
    // A `Hello` whose length prefix claims a 1 MiB body, cut off past the cap:
    // the keeper never sees a whole frame, only bytes.
    let oversized = MuxFrame::new(MuxFrameType::Hello, 0, vec![b'x'; 1024 * 1024])
        .expect("a 1 MiB frame is under the frame bound")
        .encode();
    let sent = &oversized[..UNAUTHENTICATED_MAX_BYTES + 4096];

    let connecting = Instant::now();
    let mut flood = server.connect_unauthenticated();
    // The keeper may close mid-write, which is the point; the write's own
    // result says nothing the close below does not.
    let _ = flood.send_raw(sent);
    let received = flood
        .read_until_closed()
        .expect("the keeper closes a flooding connection");
    let waited = connecting.elapsed();

    assert!(received.is_empty(), "nothing was written to it");
    assert!(
        waited < UNAUTHENTICATED_TIMEOUT,
        "closed after {waited:?}: the timer, not the byte cap"
    );
    assert_eq!(server.next_end(), ConnectionEnd::NotAuthenticated);
    server.finish();
}

/// While a channel is printing, a connection that never authenticates reads
/// not one byte of it, and the output it was refused is still there for the
/// next worker that does authenticate.
#[test]
fn an_unauthenticated_connection_reads_no_terminal_output() {
    let temp = TempDir::new("auth-held-output");
    let server = InProcessKeeper::serve(&temp, 3);
    let ticking = running("i=0; while [ $i -lt 400 ]; do echo tick; sleep 0.05; i=$((i+1)); done");
    {
        let mut spawner = server.connect();
        spawn_and_wait(&mut spawner, 1, &spawn_with(1, ticking, 80, 24));
        spawner.read_until("the channel printing", |frames| {
            frames.iter().any(|frame| {
                frame.frame_type == MuxFrameType::PtyOut
                    && frame.payload.windows(4).any(|w| w == b"tick")
            })
        });
    }
    // Disconnect or a failed write of the next tick: either way not a refusal.
    assert_ne!(server.next_end(), ConnectionEnd::NotAuthenticated);

    let mut intruder = server.connect_unauthenticated();
    let received = intruder
        .read_until_closed()
        .expect("the keeper closes the unauthenticated connection");
    assert!(
        received.is_empty(),
        "an unauthenticated peer read {} bytes of terminal output",
        received.len()
    );
    assert_eq!(server.next_end(), ConnectionEnd::NotAuthenticated);

    let mut worker = server.connect();
    worker.read_until("the channel's output", |frames| {
        frames
            .iter()
            .any(|frame| frame.frame_type == MuxFrameType::PtyOut && frame.channel_id == 1)
    });
    drop(worker);
    server.next_end();
    server.finish();
}

/// The `HelloResp` an authenticated worker gets is v2's: authenticated, this
/// protocol version, the keeper's own pid, and the same bindings the channel
/// list reports.
#[test]
fn a_verified_hello_reports_the_keeper_and_its_bindings() {
    let temp = TempDir::new("auth-hello");
    let server = InProcessKeeper::serve(&temp, 2);
    {
        let mut spawner = server.connect();
        spawn_and_wait(&mut spawner, 4, &spawn_with(4, idle(), 80, 24));
        spawn_and_wait(&mut spawner, 2, &spawn_with(2, echo(), 80, 24));
    }
    server.next_end();

    let mut worker = server.connect();
    let hello = worker.hello_response().clone();
    assert!(hello.authenticated);
    assert_eq!(hello.version, KEEPER_PROTOCOL_VERSION);
    assert_eq!(hello.pid, std::process::id(), "the keeper's own pid");
    worker.send(&empty_frame(MuxFrameType::ListChannels, 0));
    let seen = worker.read_until("the channel list", |frames| {
        frames
            .iter()
            .any(|frame| frame.frame_type == MuxFrameType::ListChannelsResp)
    });
    let listed: ListChannelsResp = seen
        .iter()
        .find(|frame| frame.frame_type == MuxFrameType::ListChannelsResp)
        .expect("the list is in what was read")
        .parse_json()
        .expect("the list decodes");
    assert_eq!(hello.bindings.len(), 2, "{:?}", hello.bindings);
    assert_eq!(hello.bindings, listed.channels);
    drop(worker);
    server.next_end();
    server.finish();
}
