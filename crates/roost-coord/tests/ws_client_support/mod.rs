// A real WebSocket client for the coordinator's two upgrade tests: it dials an
// ephemeral-port listener, offers subprotocols exactly as a worker or browser
// does, and reads frames under a bound. Shared by the worker-link and Sync
// socket test binaries; depends on `tokio-tungstenite` and nothing in the crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code)]

use std::net::SocketAddr;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{Error as ClientError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// One open client socket.
pub type WsClient = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// What an upgrade attempt answered.
pub enum Dialed {
    /// `101 Switching Protocols`, with the subprotocol the server echoed.
    Upgraded {
        protocol: Option<String>,
        socket: Box<WsClient>,
    },
    /// Any other status, which is a refusal.
    Refused { status: u16 },
}

impl Dialed {
    /// The socket, for a test that requires the upgrade to succeed.
    pub fn socket(self) -> WsClient {
        match self {
            Self::Upgraded { socket, .. } => *socket,
            Self::Refused { status } => panic!("the upgrade was refused with {status}"),
        }
    }
}

/// Dial `path` on `address`, offering `protocols` in order as one
/// `sec-websocket-protocol` header.
pub async fn dial(address: SocketAddr, path: &str, protocols: &[&str]) -> Dialed {
    let mut request = format!("ws://{address}{path}")
        .into_client_request()
        .expect("a client request");
    if !protocols.is_empty() {
        request.headers_mut().insert(
            "sec-websocket-protocol",
            protocols.join(", ").parse().expect("a header value"),
        );
    }
    match tokio_tungstenite::connect_async(request).await {
        Ok((socket, response)) => Dialed::Upgraded {
            protocol: response
                .headers()
                .get("sec-websocket-protocol")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
            socket: Box::new(socket),
        },
        Err(ClientError::Http(response)) => Dialed::Refused {
            status: response.status().as_u16(),
        },
        Err(error) => panic!("the dial failed below HTTP: {error}"),
    }
}

/// Write one binary frame.
pub async fn send_binary(socket: &mut WsClient, bytes: Vec<u8>) {
    socket
        .send(Message::Binary(bytes.into()))
        .await
        .expect("a binary write");
}

/// The next data or close frame inside `bound`, skipping transport pings and
/// pongs. `None` when the bound passed or the stream ended without a close.
pub async fn next_frame(socket: &mut WsClient, bound: Duration) -> Option<Message> {
    let deadline = tokio::time::Instant::now() + bound;
    loop {
        let read = tokio::time::timeout_at(deadline, socket.next()).await;
        match read {
            Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => {}
            Ok(Some(Ok(message))) => return Some(message),
            Ok(Some(Err(_)) | None) | Err(_) => return None,
        }
    }
}

/// The close code the server sent, reading past any data frames still in
/// flight. `Some(None)` is a close with no code; `None` is no close at all.
pub async fn close_code(socket: &mut WsClient, bound: Duration) -> Option<Option<u16>> {
    loop {
        match next_frame(socket, bound).await? {
            Message::Close(frame) => return Some(frame.map(|frame| u16::from(frame.code))),
            _ => {}
        }
    }
}
