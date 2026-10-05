//! The native half of an upgraded loopback socket: v2's `nativeSocket` over
//! Bun's `ServerWebSocket` (`local-door/local-ui-server.ts`), the value the
//! terminal and attachment port adapters wrap. `door::loopback` builds one per
//! socket and drains its outbound queue into the WebSocket; the ports call
//! `send`/`close` from any thread without ever blocking or re-entering them.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use axum::body::Bytes;
use axum::extract::ws::{CloseFrame, Message};
use futures_util::{Sink, SinkExt as _};
use tokio::sync::mpsc;

/// Bun's `ServerWebSocket` default `backpressureLimit`, which v2 ran with:
/// once this much is buffered, a further send is dropped rather than queued.
const BACKPRESSURE_LIMIT_BYTES: usize = 16 * 1024 * 1024;

/// What one send did, in the three outcomes Bun's `ws.send` reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopbackSend {
    /// Nothing was queued ahead of it, so it went straight to the wire
    /// (Bun: a positive byte count).
    Written,
    /// It is queued behind earlier bytes the socket has not flushed yet
    /// (Bun: `-1`, enqueued under backpressure).
    Queued,
    /// It was not taken: the socket is closing or closed, or its backlog is
    /// already past the backpressure limit (Bun: `0`).
    Dropped,
}

/// One outbound item, in wire order.
#[derive(Debug)]
pub(crate) enum Outbound {
    Frame(Vec<u8>),
    Ping,
    Close { code: u16, reason: String },
}

#[derive(Debug)]
struct SocketState {
    open: AtomicBool,
    /// Bytes accepted by `send` that the writer has not flushed yet.
    queued: AtomicUsize,
}

/// The native socket a port adapter wraps. Cloning shares the one socket.
#[derive(Clone, Debug)]
pub struct LoopbackSocket {
    outbound: mpsc::UnboundedSender<Outbound>,
    state: Arc<SocketState>,
}

/// The receiving half `door::loopback` drains into the WebSocket.
#[derive(Debug)]
pub(crate) struct OutboundQueue {
    receiver: mpsc::UnboundedReceiver<Outbound>,
    state: Arc<SocketState>,
}

impl LoopbackSocket {
    /// A fresh open socket and the queue its writer drains.
    pub(crate) fn open() -> (Self, OutboundQueue) {
        let (outbound, receiver) = mpsc::unbounded_channel();
        let state = Arc::new(SocketState {
            open: AtomicBool::new(true),
            queued: AtomicUsize::new(0),
        });
        let queue = OutboundQueue {
            receiver,
            state: Arc::clone(&state),
        };
        (Self { outbound, state }, queue)
    }

    /// Queue one binary frame (v2 `ws.send(bytes)`).
    pub fn send(&self, bytes: Vec<u8>) -> LoopbackSend {
        if !self.is_open() {
            return LoopbackSend::Dropped;
        }
        let length = bytes.len();
        let before = self.state.queued.fetch_add(length, Ordering::SeqCst);
        if before > BACKPRESSURE_LIMIT_BYTES || self.outbound.send(Outbound::Frame(bytes)).is_err()
        {
            self.state.queued.fetch_sub(length, Ordering::SeqCst);
            return LoopbackSend::Dropped;
        }
        if before == 0 {
            LoopbackSend::Written
        } else {
            LoopbackSend::Queued
        }
    }

    /// Close with `code` and `reason` after the frames already queued (v2
    /// `ws.close(code, reason)`). A second close is a no-op, as on a socket
    /// that is already closing.
    pub fn close(&self, code: u16, reason: &str) {
        if self.state.open.swap(false, Ordering::SeqCst) {
            let reason = reason.to_owned();
            // A send error means the writer is already gone, and so is the socket.
            let _ = self.outbound.send(Outbound::Close { code, reason });
        }
    }

    /// Whether the socket still takes frames (v2 `readyState === OPEN`).
    pub fn is_open(&self) -> bool {
        self.state.open.load(Ordering::SeqCst)
    }

    /// Ask the peer to prove it is alive; a live browser answers with a pong.
    pub(crate) fn ping(&self) {
        if self.is_open() {
            let _ = self.outbound.send(Outbound::Ping);
        }
    }

    /// The socket has ended; nothing more is taken.
    pub(crate) fn mark_closed(&self) {
        self.state.open.store(false, Ordering::SeqCst);
    }
}

impl OutboundQueue {
    /// Write every queued item to `sink` in order until a close is sent or the
    /// transport fails, then mark the socket closed.
    pub(crate) async fn drain_into<S>(mut self, mut sink: S)
    where
        S: Sink<Message> + Unpin,
    {
        while let Some(item) = self.receiver.recv().await {
            let written = match item {
                Outbound::Frame(bytes) => {
                    let length = bytes.len();
                    let written = sink.send(Message::Binary(Bytes::from(bytes))).await;
                    self.state.queued.fetch_sub(length, Ordering::SeqCst);
                    tracing::debug!(target: "terminal_latency", bytes = length, "door_frame_written");
                    written.is_ok()
                }
                Outbound::Ping => sink.send(Message::Ping(Bytes::new())).await.is_ok(),
                Outbound::Close { code, reason } => {
                    let frame = CloseFrame {
                        code,
                        reason: reason.into(),
                    };
                    let _ = sink.send(Message::Close(Some(frame))).await;
                    false
                }
            };
            if !written {
                break;
            }
        }
        self.state.open.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{LoopbackSend, LoopbackSocket, Outbound};

    /// The port adapters turn these three answers into accepted, backpressured
    /// and refused, so the split between them is what a slow browser sees.
    #[test]
    fn a_send_behind_unflushed_bytes_is_queued_and_a_closed_socket_drops() {
        let (socket, mut queue) = LoopbackSocket::open();
        assert_eq!(socket.send(vec![1, 2]), LoopbackSend::Written);
        assert_eq!(socket.send(vec![3]), LoopbackSend::Queued);
        socket.close(1000, "done");
        socket.close(1011, "again");
        assert!(!socket.is_open());
        assert_eq!(socket.send(vec![4]), LoopbackSend::Dropped);

        let mut items = Vec::new();
        while let Ok(item) = queue.receiver.try_recv() {
            items.push(item);
        }
        assert!(matches!(&items[..], [
            Outbound::Frame(first),
            Outbound::Frame(second),
            Outbound::Close { code: 1000, reason },
        ] if first == &[1, 2] && second == &[3] && reason == "done"));
    }
}
