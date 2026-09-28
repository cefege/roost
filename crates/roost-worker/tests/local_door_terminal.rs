//! The terminal route end to end: the real door upgrading a real WebSocket
//! onto the real `LocalTerminalSockets` (v2 `local-door/local-ui-server.ts`
//! handing its socket to `local-terminal-socket.ts`). A Hello that names no
//! grant is closed with its reason, a granted one is answered, and a socket
//! that never says Hello is closed at the pre-hello deadline.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod door_support;
mod local_terminal_support;
mod terminal_stream_support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use door_support::{Client, DoorOptions, TestDoor, open_socket, start_door};
use futures_util::{SinkExt as _, StreamExt as _};
use local_terminal_support::{Fixture, GRANT_ID, SECRET, TAB, case, closed_reason, encode, hello};
use roost_proto::__buffa::oneof::local_terminal_server_frame::Frame as ServerFrame;
use roost_proto::LocalTerminalServerFrame;
use roost_proto::buffa::Message as _;
use roost_protocol::terminal_peer::peer::TERMINAL_PEER_HELLO_DEADLINE_MS;
use roost_worker::door::loopback::LoopbackOwner;
use roost_worker::door::{LOCAL_TERMINAL_PATH, LOCAL_TERMINAL_SUBPROTOCOL};
use tokio_tungstenite::tungstenite::Message;

async fn door_over(fixture: &Fixture) -> TestDoor {
    let owner = LoopbackOwner::terminal(Arc::clone(&fixture.sockets));
    start_door(DoorOptions {
        terminal: Some(owner),
        ..DoorOptions::default()
    })
    .await
}

async fn connect(door: &TestDoor) -> Client {
    let (client, _) = open_socket(door, LOCAL_TERMINAL_PATH, &[LOCAL_TERMINAL_SUBPROTOCOL])
        .await
        .expect("the terminal route upgrades");
    client
}

/// Every server frame up to the first `limit` or the socket's end, with the
/// close code when the socket ended.
async fn frames(client: &mut Client, limit: usize) -> (Vec<ServerFrame>, Option<u16>) {
    let read = async {
        let mut seen = Vec::new();
        while seen.len() < limit {
            match client.next().await {
                Some(Ok(Message::Binary(bytes))) => {
                    let frame = LocalTerminalServerFrame::decode_from_slice(&bytes).unwrap();
                    seen.push(frame.frame.expect("a server frame names its case"));
                }
                Some(Ok(Message::Close(close))) => {
                    return (seen, close.map(|close| u16::from(close.code)));
                }
                Some(Ok(_)) => {}
                Some(Err(_)) | None => return (seen, None),
            }
        }
        (seen, None)
    };
    tokio::time::timeout(Duration::from_secs(10), read)
        .await
        .expect("the socket answered")
}

#[tokio::test]
async fn a_hello_that_names_no_grant_is_closed_with_its_reason() {
    let fixture = Fixture::new();
    let door = door_over(&fixture).await;
    let mut client = connect(&door).await;

    let unknown = "0e0e0e0e-0e0e-4e0e-8e0e-0e0e0e0e0e0e";
    client
        .send(Message::binary(encode(hello(unknown, SECRET, TAB))))
        .await
        .unwrap();
    let (seen, code) = frames(&mut client, usize::MAX).await;

    assert!(
        seen.iter().all(|frame| case(frame) != "ready"),
        "{:?}",
        seen.iter().map(case).collect::<Vec<_>>()
    );
    assert_eq!(seen.last().map(case), Some("closed"));
    assert_eq!(code, Some(1000));
}

/// The contrast that makes the refusal mean something: the grant the fixture
/// installed is answered with `ready` over the same door.
#[tokio::test]
async fn a_hello_that_names_its_grant_is_answered_ready() {
    let fixture = Fixture::new();
    let door = door_over(&fixture).await;
    let mut client = connect(&door).await;

    client
        .send(Message::binary(encode(hello(GRANT_ID, SECRET, TAB))))
        .await
        .unwrap();
    let (seen, _) = frames(&mut client, 1).await;

    assert_eq!(seen.first().map(case), Some("ready"));
}

#[tokio::test]
async fn a_socket_that_never_says_hello_is_closed_at_the_deadline() {
    let fixture = Fixture::new();
    let door = door_over(&fixture).await;
    let mut client = connect(&door).await;
    let opened = Instant::now();

    let (seen, code) = frames(&mut client, usize::MAX).await;

    let deadline = Duration::from_millis(TERMINAL_PEER_HELLO_DEADLINE_MS);
    assert!(
        opened.elapsed() + Duration::from_millis(100) >= deadline,
        "closed early: {:?}",
        opened.elapsed()
    );
    assert_eq!(
        closed_reason(seen.last().expect("a closed frame")),
        "local terminal hello timed out"
    );
    assert_eq!(code, Some(1000));
}
