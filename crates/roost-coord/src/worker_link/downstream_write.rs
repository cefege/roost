//! Writing one queued coordinator→worker frame onto the worker socket, and
//! how a link loop ends. Called by `worker_link::connection`'s outbound arm and
//! by `worker_link::result_lane` while a durable append holds the read loop.
//! Depends on the link's one protobuf codec.

use axum::extract::ws::{Message, WebSocket};
use roost_protocol::proto_adapters::coord_worker_proto::encode_downstream;
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;

use crate::worker_link::conn_types::SocketClose;

/// The queue `WorkerHandle::send` fills and the link writes from.
pub(super) type DownstreamOutbox = tokio::sync::mpsc::UnboundedReceiver<CoordWorkerDownstream>;

/// How the loop ended: by a close this side sends, or by the peer.
pub(super) enum Ending {
    Close(SocketClose),
    PeerClosed,
}

/// Write one queued frame as protobuf binary.
///
/// A frame that does not encode, or a write the socket refuses, ends the link
/// with no code: v2's `sendProtocolFrame` closes on a thrown send so the worker
/// reconnects and replays whatever was left unacknowledged.
pub(super) async fn write_downstream(
    socket: &mut WebSocket,
    frame: &CoordWorkerDownstream,
    worker_fp: &WorkerFp,
) -> Result<(), Ending> {
    let bytes = match encode_downstream(frame) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(%worker_fp, what = frame.kind(), %error,
                "worker link: send_failed; the frame did not encode");
            return Err(Ending::Close(SocketClose::Default));
        }
    };
    socket
        .send(Message::Binary(bytes.into()))
        .await
        .map_err(|error| {
            tracing::warn!(%worker_fp, what = frame.kind(), %error,
                "worker link: send_failed");
            Ending::PeerClosed
        })
}
