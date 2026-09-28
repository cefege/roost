//! The door's two WebSocket routes (v2 `local-door/local-ui-server.test.ts`
//! socket cases and `local-attachment-ui.test.ts`): each upgrades only its own
//! subprotocol, carries binary frames both ways to its own owner, drops text,
//! refuses oversized frames unread, and tells its owner when the socket ends.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod door_support;

use door_support::{
    DoorEvent, DoorOptions, TestDoor, next_event, open_socket, request, start_door,
};
use futures_util::{SinkExt as _, StreamExt as _};
use roost_worker::door::{
    LOCAL_ATTACHMENT_MAX_PAYLOAD_BYTES, LOCAL_ATTACHMENT_PATH, LOCAL_ATTACHMENT_SUBPROTOCOL,
    LOCAL_TERMINAL_MAX_PAYLOAD_BYTES, LOCAL_TERMINAL_PATH, LOCAL_TERMINAL_SUBPROTOCOL,
    LoopbackSend,
};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

async fn opened(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<DoorEvent>,
) -> std::sync::Arc<door_support::RecordedPort> {
    match next_event(events).await {
        DoorEvent::Opened(port) => port,
        other => panic!("expected an open, got {other:?}"),
    }
}

/// Wait until the client sees its socket end, whatever the close looked like.
async fn ended(client: &mut door_support::Client) {
    let wait = async {
        while let Some(Ok(message)) = client.next().await {
            if matches!(message, Message::Close(_)) {
                break;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(5), wait)
        .await
        .expect("the door ended the socket");
}

#[tokio::test]
async fn the_terminal_socket_carries_binary_frames_both_ways_and_reports_close() {
    let mut door = start_door(DoorOptions::default()).await;

    let (mut client, protocol) =
        open_socket(&door, LOCAL_TERMINAL_PATH, &[LOCAL_TERMINAL_SUBPROTOCOL])
            .await
            .unwrap();
    let port = opened(&mut door.terminal).await;
    // Frame order is guaranteed, so the binary frame behind the text one
    // arriving is what proves the text was dropped rather than stalled.
    client.send(Message::text("not a frame")).await.unwrap();
    client.send(Message::binary(vec![1, 2, 3])).await.unwrap();
    let inbound = next_event(&mut door.terminal).await;
    assert_eq!(port.socket.send(vec![9, 8]), LoopbackSend::Written);
    let outbound = client.next().await.unwrap().unwrap();

    assert_eq!(protocol.as_deref(), Some(LOCAL_TERMINAL_SUBPROTOCOL));
    assert!(
        matches!(&inbound, DoorEvent::Frame(id, bytes) if *id == port.socket_id && bytes.as_slice() == [1, 2, 3])
    );
    assert_eq!(outbound, Message::binary(vec![9, 8]));
    assert!(port.socket.is_open());

    client.close(None).await.unwrap();
    assert!(
        matches!(next_event(&mut door.terminal).await, DoorEvent::Closed(id) if id == port.socket_id)
    );
    assert!(!port.socket.is_open());
}

#[tokio::test]
async fn the_terminal_path_refuses_a_plain_request_and_a_foreign_subprotocol() {
    let mut door = start_door(DoorOptions::default()).await;

    let plain = request(&door, "GET", LOCAL_TERMINAL_PATH, &[]).await;
    let posted = request(&door, "POST", LOCAL_TERMINAL_PATH, &[]).await;
    let foreign = open_socket(&door, LOCAL_TERMINAL_PATH, &["mallory-terminal"]).await;

    assert_eq!(plain.status, 400);
    assert_eq!(posted.status, 405);
    assert!(foreign.is_err(), "a foreign subprotocol never opens");
    assert!(door.terminal.try_recv().is_err(), "no owner saw a socket");
}

/// A frame past the payload ceiling ends the socket before any owner reads it.
#[tokio::test]
async fn a_terminal_frame_over_the_payload_ceiling_is_never_read() {
    let mut door = start_door(DoorOptions::default()).await;
    let (mut client, _) = open_socket(&door, LOCAL_TERMINAL_PATH, &[LOCAL_TERMINAL_SUBPROTOCOL])
        .await
        .unwrap();
    let port = opened(&mut door.terminal).await;

    let _ = client
        .send(Message::binary(vec![
            0;
            LOCAL_TERMINAL_MAX_PAYLOAD_BYTES + 1
        ]))
        .await;

    assert!(
        matches!(next_event(&mut door.terminal).await, DoorEvent::Closed(id) if id == port.socket_id)
    );
}

/// Without an attachment owner the attachment path is v2's 404, even for a
/// well-formed handshake.
#[tokio::test]
async fn an_absent_attachment_owner_answers_404() {
    let door = start_door(DoorOptions::default()).await;

    let plain = request(&door, "GET", LOCAL_ATTACHMENT_PATH, &[]).await;
    let handshake = open_socket(
        &door,
        LOCAL_ATTACHMENT_PATH,
        &[LOCAL_ATTACHMENT_SUBPROTOCOL],
    )
    .await;

    assert_eq!(plain.status, 404);
    assert!(handshake.is_err());
}

async fn attachment_door() -> TestDoor {
    start_door(DoorOptions {
        attachment: true,
        ..DoorOptions::default()
    })
    .await
}

#[tokio::test]
async fn the_attachment_route_uses_its_own_subprotocol_and_owner() {
    let mut door = attachment_door().await;
    let plain = request(&door, "GET", LOCAL_ATTACHMENT_PATH, &[]).await;
    let terminal_name =
        open_socket(&door, LOCAL_ATTACHMENT_PATH, &[LOCAL_TERMINAL_SUBPROTOCOL]).await;

    let (mut client, protocol) = open_socket(
        &door,
        LOCAL_ATTACHMENT_PATH,
        &[LOCAL_ATTACHMENT_SUBPROTOCOL],
    )
    .await
    .unwrap();
    let events = door.attachment.as_mut().unwrap();
    let port = opened(events).await;
    client.send(Message::binary(vec![1, 2, 3])).await.unwrap();
    let inbound = next_event(events).await;
    assert_eq!(port.socket.send(vec![9, 8]), LoopbackSend::Written);

    assert_eq!(plain.status, 400);
    assert!(terminal_name.is_err());
    assert_eq!(protocol.as_deref(), Some(LOCAL_ATTACHMENT_SUBPROTOCOL));
    assert!(matches!(&inbound, DoorEvent::Frame(_, bytes) if bytes.as_slice() == [1, 2, 3]));
    assert_eq!(
        client.next().await.unwrap().unwrap(),
        Message::binary(vec![9, 8])
    );
    assert!(
        door.terminal.try_recv().is_err(),
        "the terminal owner saw nothing"
    );
}

#[tokio::test]
async fn an_oversized_attachment_frame_closes_the_socket_before_its_owner_reads_it() {
    let mut door = attachment_door().await;
    let (mut client, _) = open_socket(
        &door,
        LOCAL_ATTACHMENT_PATH,
        &[LOCAL_ATTACHMENT_SUBPROTOCOL],
    )
    .await
    .unwrap();
    let events = door.attachment.as_mut().unwrap();
    let port = opened(events).await;

    let _ = client
        .send(Message::binary(vec![
            0;
            LOCAL_ATTACHMENT_MAX_PAYLOAD_BYTES + 1
        ]))
        .await;
    ended(&mut client).await;

    assert!(matches!(next_event(events).await, DoorEvent::Closed(id) if id == port.socket_id));
}

/// Closing the door ends every socket it upgraded and tells each owner, so a
/// stopping worker leaves no terminal socket behind (v2 `server.stop(true)`).
#[tokio::test]
async fn closing_the_door_ends_its_open_sockets_and_tells_their_owner() {
    let door = start_door(DoorOptions::default()).await;
    let (mut client, _) = open_socket(&door, LOCAL_TERMINAL_PATH, &[LOCAL_TERMINAL_SUBPROTOCOL])
        .await
        .unwrap();
    let TestDoor {
        server,
        mut terminal,
        ..
    } = door;
    let port = opened(&mut terminal).await;

    server.close();

    assert!(
        matches!(next_event(&mut terminal).await, DoorEvent::Closed(id) if id == port.socket_id)
    );
    ended(&mut client).await;
}
