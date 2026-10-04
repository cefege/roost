//! The reconnect half of the durable mirror's contract: a row the socket took
//! but the coordinator never acknowledged goes out again under its own
//! sequence on the next link. Drives the real drain over a loopback socket,
//! because the defect lived in what the drain did after a successful send.

use futures_util::StreamExt;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use super::*;
use crate::link_dial::Link;
use crate::runtime::downstream::DownstreamLink;
use crate::runtime::link_drain::drain;
use crate::runtime::link_wire::LinkWire;

/// One loopback socket pair: the worker's [`Link`] and the far end it writes to.
async fn loopback() -> (Link, WebSocketStream<TcpStream>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let accept = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        tokio_tungstenite::accept_async(stream)
            .await
            .expect("upgrade")
    });
    let stream = TcpStream::connect(address).await.expect("connect");
    let (client, _) =
        tokio_tungstenite::client_async(format!("ws://{address}/"), MaybeTlsStream::Plain(stream))
            .await
            .expect("handshake");
    (Link::new(client, None), accept.await.expect("accept task"))
}

async fn next_binary(far: &mut WebSocketStream<TcpStream>) -> Vec<u8> {
    match far.next().await {
        Some(Ok(Message::Binary(bytes))) => bytes.to_vec(),
        other => panic!("expected one binary frame, got {other:?}"),
    }
}

fn event_bytes(event: SessionEvent, client_seq: u64) -> Vec<u8> {
    ProtoLinkWire
        .encode_upstream(&CoordWorkerUpstream::Event {
            event,
            client_seq,
            trace_id: None,
        })
        .expect("encodes")
}

/// The stall this pins: the mirror let its head go when the socket took it,
/// so after a reconnect the pump re-released sequence 1 against row 2's bytes,
/// the coordinator acknowledged 2, the pump ignored it, and row 1 was stranded
/// until the worker restarted.
#[tokio::test]
async fn a_row_sent_but_not_acknowledged_is_resent_under_its_own_sequence_after_a_reconnect() {
    let scratch = Scratch::new();
    let mut link = link_for_test();
    let journal = Journal::open(&scratch.0.join(DATABASE_FILE_NAME))
        .await
        .expect("a fresh outbox opens");
    link.attach_durable_outbox(std::sync::Arc::new(journal), delivery());
    let first = link.publish_durable_event(&opened()).await.expect("first");
    let second = link.publish_durable_event(&closed()).await.expect("second");
    assert_eq!((first.client_seq, second.client_seq), (1, 2));

    let (mut socket, mut far) = loopback().await;
    let action = link.pump.on_hello_ack();
    crate::runtime::link_drain::apply_to(&mut link, action);
    assert!(drain(&mut link, &mut socket).await.is_none());
    assert_eq!(next_binary(&mut far).await, event_bytes(opened(), 1));

    // The coordinator restarts before it acknowledges anything.
    drop((socket, far));
    link.detach_link();

    let (mut socket, mut far) = loopback().await;
    let _hello = link.pump.on_open();
    let action = link.pump.on_hello_ack();
    crate::runtime::link_drain::apply_to(&mut link, action);
    assert!(drain(&mut link, &mut socket).await.is_none());
    assert_eq!(
        next_binary(&mut far).await,
        event_bytes(opened(), 1),
        "the next link did not resend the unacknowledged row under its own sequence"
    );

    link.event_acknowledged(1);
    assert!(drain(&mut link, &mut socket).await.is_none());
    assert_eq!(
        next_binary(&mut far).await,
        event_bytes(closed(), 2),
        "acknowledging the first row did not release the second"
    );
    assert_eq!(
        link.durable_pending(),
        1,
        "the mirror let go of an unacknowledged row"
    );
}
