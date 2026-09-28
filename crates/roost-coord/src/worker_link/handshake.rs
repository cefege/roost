//! The pre-hello handshake: the forced first frame, and the bound it is read
//! under. Called by `worker_link::connection` before the handle exists.
//! Depends only on the socket, the wire codec and the spec's own constant.

use std::collections::BTreeSet;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use roost_protocol::wire::WorkerFp;

/// connects, authenticates and never sends a hello waits this long and is then
/// closed with the no-code default** — it is not admitted and it is not
/// dropped, because `:28` says the pre-hello state was never usable.
pub(super) const PRE_HELLO_WAIT: Duration = crate::worker_link::keepalive::STALE_LINK_TIMEOUT;

/// The negotiated facts a `WHello` carries, which are the handle's arguments.
pub(super) struct Hello {
    pub(super) process_epoch: String,
    pub(super) capabilities: BTreeSet<String>,
}

/// Read the forced first frame, under the pre-hello bound.
pub(super) async fn read_hello(socket: &mut WebSocket, worker_fp: &WorkerFp) -> Option<Hello> {
    let deadline = tokio::time::Instant::now() + PRE_HELLO_WAIT;
    loop {
        let received = tokio::time::timeout_at(deadline, socket.recv()).await;
        let Ok(Some(Ok(message))) = received else {
            tracing::warn!(%worker_fp, "worker link: no hello inside the pre-hello bound");
            return None;
        };
        let text = match message {
            Message::Text(text) => text.as_str().as_bytes().to_vec(),
            Message::Binary(bytes) => bytes.to_vec(),
            _ => continue,
        };
        // `decode_upstream` — `roost_protocol::proto_adapters::coord_worker_proto:55`,
        // the link's EXISTING codec. Never a second decoder.
        let frame = match roost_protocol::proto_adapters::coord_worker_proto::decode_upstream(&text)
        {
            Ok(frame) => frame,
            Err(error) => {
                tracing::warn!(%worker_fp, %error, "worker link: a frame did not decode");
                return None;
            }
        };
        // `CoordWorkerUpstream::Hello` — `roost_protocol::wire::coord_worker::upstream.rs:36`,
        // carrying `capabilities` and `process_epoch`.
        if let roost_protocol::wire::coord_worker::CoordWorkerUpstream::Hello {
            capabilities,
            process_epoch,
            ..
        } = frame
        {
            return Some(Hello {
                process_epoch,
                capabilities: capabilities.into_iter().collect(),
            });
        }
    }
}
