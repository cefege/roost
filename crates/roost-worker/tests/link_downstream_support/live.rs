//! A real `LinkLoop` over the production protobuf codec, dialling a loopback
//! coordinator the test drives frame by frame. Ephemeral port only.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use roost_protocol::proto_adapters::coord_worker_proto::{decode_upstream, encode_downstream};
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::coord_worker::{
    CoordWorkerDownstream as Down, CoordWorkerUpstream as Up, EventAck,
};
use roost_protocol::wire::event::SessionEvent;
use roost_worker::link_dial::CoordinatorEndpoint;
use roost_worker::link_dial::WORKER_AUTH_SUBPROTOCOL;
use roost_worker::link_ports::DownstreamOwners;
use roost_worker::runtime::credential::{CredentialError, CredentialSource};
use roost_worker::runtime::link_loop::{
    BrowserLink, CoordinatorCellSink, LinkLoop, WorkerIdentity,
};
use roost_worker::runtime::link_wire::ProtoLinkWire;
use roost_worker::runtime::snapshot_source::{SnapshotError, SnapshotSource};
use roost_worker::runtime::stop::{StopReason, StopRequests};
use roost_worker::uplink::Uplink;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::HeaderValue;

use super::Fakes;

pub const FINGERPRINT: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
pub const PATIENCE: Duration = Duration::from_secs(10);

pub type Socket = WebSocketStream<TcpStream>;

#[derive(Debug)]
struct FixedCredential;

impl CredentialSource for FixedCredential {
    fn mint(&self) -> Result<String, CredentialError> {
        Ok("a-test-credential".to_owned())
    }
}

#[derive(Debug)]
struct FixedSnapshot;

impl SnapshotSource for FixedSnapshot {
    fn is_active(&self) -> bool {
        true
    }
    fn snapshot(&self) -> Result<SessionEvent, SnapshotError> {
        Ok(snapshot_event())
    }
}

/// The loopback worker's session set: empty, as the coordinator end reads it.
/// Stamped like v2's `buildSnapshot` (`Date.now()`): the wire refuses a
/// snapshot at the epoch (`session_event.snapshot.ts` must be at least 1).
pub fn snapshot_event() -> SessionEvent {
    SessionEvent::Snapshot {
        worker_fp: WorkerFp::try_from(FINGERPRINT).unwrap(),
        sessions: Vec::new(),
        ts: 1_700_000_000_000,
        trace_id: None,
    }
}

/// v2's snapshot frame: `Event{snapshot, client_seq}`, acknowledged like a row.
pub fn snapshot_frame(client_seq: u64) -> Up {
    Up::Event {
        event: snapshot_event(),
        client_seq,
        trace_id: None,
    }
}

/// One running worker link and the coordinator end the test holds.
pub struct LiveLink {
    pub listener: TcpListener,
    pub uplink: Uplink,
    requests: StopRequests,
    worker: JoinHandle<StopReason>,
}

impl LiveLink {
    pub async fn start(fakes: &Fakes, sink: Option<Arc<CoordinatorCellSink>>) -> Self {
        Self::start_with(fakes.owners(), sink).await
    }

    /// A link whose owners the test composed itself.
    pub async fn start_with(
        owners: DownstreamOwners,
        sink: Option<Arc<CoordinatorCellSink>>,
    ) -> Self {
        Self::start_configured(owners, |link| {
            if let Some(sink) = sink {
                link.attach_cell_sink(sink);
            }
        })
        .await
    }

    /// A link the test attaches more to (a durable outbox) before it runs.
    pub async fn start_configured(
        owners: DownstreamOwners,
        configure: impl FnOnce(&mut LinkLoop),
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (uplink, receiver) = roost_worker::uplink::channel();
        let mut link = LinkLoop::new(
            CoordinatorEndpoint::new(format!("http://{address}"), FINGERPRINT).unwrap(),
            WorkerIdentity {
                worker_fp: WorkerFp::try_from(FINGERPRINT).unwrap(),
                version: "test".to_owned(),
                process_epoch: "test-epoch".to_owned(),
            },
            Arc::new(ProtoLinkWire),
            Arc::new(FixedSnapshot),
            Arc::new(FixedCredential),
            BrowserLink::detached(),
            receiver,
        );
        link.attach_owners(owners);
        configure(&mut link);
        let (requests, stop) = StopRequests::channel();
        let worker = tokio::spawn(link.run(stop));
        Self {
            listener,
            uplink,
            requests,
            worker,
        }
    }

    /// Accept the next dial, selecting the auth marker the worker requires the
    /// server to echo.
    pub async fn accept(&self) -> Socket {
        let (stream, _) = tokio::time::timeout(PATIENCE, self.listener.accept())
            .await
            .unwrap()
            .unwrap();
        // The signature is tungstenite's `Callback`; its error type is fixed.
        #[allow(clippy::result_large_err)]
        let echo = |_: &Request, mut response: Response| -> Result<Response, ErrorResponse> {
            response.headers_mut().insert(
                "Sec-WebSocket-Protocol",
                HeaderValue::from_static(WORKER_AUTH_SUBPROTOCOL),
            );
            Ok(response)
        };
        tokio_tungstenite::accept_hdr_async(stream, echo)
            .await
            .unwrap()
    }

    pub async fn stop(self) {
        assert!(self.requests.request(StopReason::ShutdownFrame));
        let reason = tokio::time::timeout(PATIENCE, self.worker)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reason, StopReason::ShutdownFrame);
    }
}

pub async fn next_bytes(socket: &mut Socket) -> Vec<u8> {
    loop {
        let message = tokio::time::timeout(PATIENCE, socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Message::Binary(bytes) = message {
            return bytes.to_vec();
        }
    }
}

pub async fn next_frame(socket: &mut Socket) -> Up {
    decode_upstream(&next_bytes(socket).await).unwrap()
}

pub async fn send(socket: &mut Socket, frame: &Down) {
    send_bytes(socket, encode_downstream(frame).unwrap()).await;
}

pub async fn send_bytes(socket: &mut Socket, bytes: Vec<u8>) {
    socket.send(Message::Binary(bytes.into())).await.unwrap();
}

/// Read the hello, acknowledge it with `capabilities`, read the snapshot and
/// acknowledge it: the link is live when this returns. Returns the hello.
pub async fn go_live(socket: &mut Socket, capabilities: Vec<String>) -> Up {
    let hello = next_frame(socket).await;
    assert!(
        matches!(hello, Up::Hello { .. }),
        "the first frame is the hello"
    );
    send(
        socket,
        &Down::HelloAck {
            capabilities,
            trace_id: None,
        },
    )
    .await;
    assert_eq!(
        next_frame(socket).await,
        snapshot_frame(1),
        "the snapshot follows the hello-ack"
    );
    // A fresh pump's snapshot takes the first sequence.
    send(socket, &Down::EventAck(EventAck { client_seq: 1 })).await;
    hello
}

/// Wait, bounded, until `condition` holds.
pub async fn eventually(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(PATIENCE, async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
