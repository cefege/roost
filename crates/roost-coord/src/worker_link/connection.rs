//! One admitted worker socket, read and written by one task to its end.
//!
//! Called by `http::upgrade::worker_upgrade` once the upgrade completes. Ports
//! the socket lifecycle of `apps/coord/src/workers/worker-ws-handler.ts`
//! (`open`, `message`, `close`) and the transport half of `worker-conn.ts`
//! (`send`, `requestClose`). What a frame MEANS is `worker_link::link_session`
//! and the dispatcher behind it; this file owns only the socket.
//!
//! ONE OWNER, NO SPLIT. `WebSocket::recv` and `send` both take `&mut self` and
//! axum's `WebSocket` is not a `Sink`, so the socket is never divided: the loop
//! waits on the close request, the outbound queue, the next timer and the next
//! message, and each dispatch is awaited before the next read. That await is
//! what keeps durable frames in arrival order without v2's explicit queue —
//! the runtime here awaits the handler that Bun would not.
//!
//! `WorkerHandle::send` is a synchronous closure, so it ENQUEUES; the loop's
//! outbound arm is what writes, as protobuf binary through the link's one codec.

use std::sync::Arc;

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use roost_protocol::proto_adapters::coord_worker_proto::encode_downstream;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use tokio::sync::{Notify, mpsc};
use tokio::time::Instant;

use crate::services::CoordServices;
use crate::worker_link::conn_types::SocketClose;
use crate::worker_link::handshake::read_hello;
use crate::worker_link::link_session::{LinkSession, LinkStep, LinkTransport};
use crate::worker_link::upgrade_admission::{UpgradeDecision, VerifiedWorkerCaller};
use crate::worker_link::upstream_frame::message_bytes;

/// Serve one admitted worker socket to its end.
///
/// Takes the whole decision rather than the `Admitted` variant, so a caller that
/// hands over a refusal gets a logged no-op rather than a panic on an arm
/// nothing can reach.
pub async fn serve_socket(
    socket: WebSocket,
    decision: UpgradeDecision,
    services: &Arc<CoordServices>,
) {
    let caller = match decision {
        UpgradeDecision::Admitted { caller, .. } => caller,
        UpgradeDecision::Refused(refusal) => {
            tracing::warn!(
                ?refusal,
                "worker link: a refusal reached the socket; nothing to serve"
            );
            return;
        }
    };
    // v2's `open` re-checks the key generation, because a revocation can land
    // between the upgrade's verification and the socket opening
    // (`worker-ws-handler.ts:155-158`).
    if !services
        .jwt_keys
        .generation_is_current(&caller.fingerprint, caller.key_generation)
    {
        tracing::warn!(worker_fp = %caller.fingerprint, "worker link: revoked before open");
        let mut socket = socket;
        close_socket(&mut socket, SocketClose::Revoked).await;
        return;
    }
    tracing::info!(worker_fp = %caller.fingerprint, key_generation = caller.key_generation,
        label = %caller.label, "worker link: open");
    run_link(socket, &caller, services).await;
}

/// The link: the hello, then the loop, then the teardown.
async fn run_link(
    mut socket: WebSocket,
    caller: &VerifiedWorkerCaller,
    services: &Arc<CoordServices>,
) {
    let Some(hello) = read_hello(&mut socket, services, caller).await else {
        close_socket(&mut socket, SocketClose::Default).await;
        return;
    };
    let (outbound, mut outbox) = mpsc::unbounded_channel::<CoordWorkerDownstream>();
    let close_requested = Arc::new(Notify::new());
    let transport = LinkTransport {
        outbound,
        close_requested: Arc::clone(&close_requested),
    };
    let Some(mut session) = LinkSession::claim(services, hello, transport) else {
        close_socket(&mut socket, SocketClose::Default).await;
        return;
    };

    let ending = loop {
        let wake = session.next_wake();
        tokio::select! {
            // Biased so a superseded socket closes before it reads another frame.
            biased;
            () = close_requested.notified() => {
                tracing::info!(worker_fp = %session.worker_fp(),
                    "worker link: a newer hello superseded this socket");
                break Ending::Close(SocketClose::Default);
            }
            queued = outbox.recv() => {
                let Some(frame) = queued else {
                    break Ending::Close(SocketClose::Default);
                };
                if let Err(close) = write_frame(&mut socket, &frame, &session).await {
                    break close;
                }
            }
            () = sleep_until(wake) => {
                if let LinkStep::Close(close) = session.on_wake(Instant::now()) {
                    break Ending::Close(close);
                }
            }
            received = socket.recv() => match received {
                None | Some(Ok(Message::Close(_))) => break Ending::PeerClosed,
                Some(Err(error)) => {
                    tracing::info!(worker_fp = %session.worker_fp(), %error,
                        "worker link: the socket failed");
                    break Ending::PeerClosed;
                }
                Some(Ok(message)) => {
                    let Some(bytes) = message_bytes(message) else {
                        continue;
                    };
                    if let LinkStep::Close(close) = session.on_bytes(&bytes).await {
                        break Ending::Close(close);
                    }
                }
            },
        }
    };
    session.end();
    if let Ending::Close(close) = ending {
        close_socket(&mut socket, close).await;
    }
}

/// How the loop ended: by a close this side sends, or by the peer.
enum Ending {
    Close(SocketClose),
    PeerClosed,
}

/// Write one queued frame as protobuf binary.
///
/// A frame that does not encode, or a write the socket refuses, ends the link
/// with no code: v2's `sendProtocolFrame` closes on a thrown send so the worker
/// reconnects and replays whatever was left unacknowledged.
async fn write_frame(
    socket: &mut WebSocket,
    frame: &CoordWorkerDownstream,
    session: &LinkSession,
) -> Result<(), Ending> {
    let bytes = match encode_downstream(frame) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(worker_fp = %session.worker_fp(), what = frame.kind(), %error,
                "worker link: send_failed; the frame did not encode");
            return Err(Ending::Close(SocketClose::Default));
        }
    };
    socket
        .send(Message::Binary(bytes.into()))
        .await
        .map_err(|error| {
            tracing::warn!(worker_fp = %session.worker_fp(), what = frame.kind(), %error,
                "worker link: send_failed");
            Ending::PeerClosed
        })
}

/// Sleep until `wake`, or forever when nothing is scheduled.
async fn sleep_until(wake: Option<Instant>) {
    match wake {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// Send this side's close frame, best effort: the peer may already be gone.
async fn close_socket(socket: &mut WebSocket, close: SocketClose) {
    let frame = close.into_frame().map(|(code, reason)| CloseFrame {
        code,
        reason: reason.into(),
    });
    if let Err(error) = socket.send(Message::Close(frame)).await {
        tracing::debug!(%error, "worker link: the close frame did not reach the peer");
    }
}
