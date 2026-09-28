//! A direct upload over the real loopback door lands a file: a WebSocket on the
//! attachment subprotocol reaches the direct receiver through the door's
//! shared pump, the hello is admitted against a dedicated grant, and the final
//! ack names the committed path. The Rust acceptance proof for
//! `local-ui-attachment-socket.ts` + `attachment-direct-socket.ts` together.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "attachment_direct_support/mod.rs"]
mod attachment_direct_support;
mod door_support;

use std::sync::Arc;
use std::time::Duration;

use attachment_direct_support::DirectFixture;
use door_support::{DoorOptions, open_socket, start_door};
use futures_util::{SinkExt as _, StreamExt as _};
use roost_proto::__buffa::oneof::attachment_transfer_server_frame::Frame as ServerFrame;
use roost_proto::AttachmentTransferServerFrame;
use roost_proto::buffa::Message as _;
use roost_worker::door::loopback::LoopbackOwner;
use roost_worker::door::{LOCAL_ATTACHMENT_PATH, LOCAL_ATTACHMENT_SUBPROTOCOL};
use tokio_tungstenite::tungstenite::Message;

async fn next_server_frame<S>(client: &mut S) -> ServerFrame
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), client.next())
            .await
            .expect("the worker answers in time")
            .expect("the socket stays open")
            .unwrap();
        if let Message::Binary(bytes) = message {
            return AttachmentTransferServerFrame::decode_from_slice(&bytes)
                .unwrap()
                .frame
                .unwrap();
        }
    }
}

#[tokio::test]
async fn a_direct_upload_over_the_loopback_door_lands_the_file() {
    let fixture = DirectFixture::new(5);
    let door = start_door(DoorOptions {
        attachment_owner: Some(LoopbackOwner::attachment(Arc::new(fixture.sockets.clone()))),
        ..DoorOptions::default()
    })
    .await;
    let (mut client, protocol) = open_socket(
        &door,
        LOCAL_ATTACHMENT_PATH,
        &[LOCAL_ATTACHMENT_SUBPROTOCOL],
    )
    .await
    .unwrap();
    assert_eq!(protocol.as_deref(), Some(LOCAL_ATTACHMENT_SUBPROTOCOL));

    client
        .send(Message::binary(fixture.hello(&fixture.secret)))
        .await
        .unwrap();
    let ServerFrame::Ready(ready) = next_server_frame(&mut client).await else {
        panic!("the hello is admitted with a ready frame");
    };
    assert_eq!(ready.upload_id, fixture.upload_id);

    let (first, _) = fixture.chunk(0, 0, &[1, 2], false);
    client.send(Message::binary(first)).await.unwrap();
    let ServerFrame::Ack(progress) = next_server_frame(&mut client).await else {
        panic!("a non-final chunk is acknowledged");
    };
    assert_eq!(
        (progress.bytes_received, progress.abs_path.as_str()),
        (2, "")
    );

    let (last, digest) = fixture.chunk(1, 2, &[3, 4, 5], true);
    client.send(Message::binary(last)).await.unwrap();
    let ServerFrame::Ack(done) = next_server_frame(&mut client).await else {
        panic!("the final chunk is acknowledged");
    };
    assert_eq!(
        (done.bytes_received, done.chunk_sha256.as_str()),
        (5, digest.as_str())
    );
    assert_eq!(std::fs::read(&done.abs_path).unwrap(), vec![1, 2, 3, 4, 5]);
    let ServerFrame::Closed(closed) = next_server_frame(&mut client).await else {
        panic!("the socket closes after the final ack");
    };
    assert_eq!(closed.reason, "complete");
}
