//! The frames a worker link lets past a durable append that is still in flight,
//! and the bounded ordered backlog every other frame waits in meanwhile.
//! Ports the completion-frame lane of `apps/coord/src/workers/worker-ws-handler.ts:235-254`
//! and its bounded slow-path queue (`:347-349`) over `worker_link::frame_queue`.
//! Owned by `worker_link::link_session`; `worker_link::connection` drains the backlog.

use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket};
use roost_protocol::wire::coord_worker::CoordWorkerUpstream;

use crate::worker_link::conn_types::SocketClose;
use crate::worker_link::dispatch::{DispatchFuture, DispatchOutcome, FrameDispatch, InboundFrame};
use crate::worker_link::frame_dispatch::WorkerFrameDispatcher;
use crate::worker_link::frame_queue::{FrameQueue, Queued, QueuedFrame};
use crate::worker_link::upstream_frame::{LinkFrame, decode_link_frame, message_bytes};

/// Whether a frame may overtake a durable append still in flight.
///
/// v2's list: a typed completion has no durable effect and no channel-order
/// dependency, so it must not wait out a slow append its waiter never asked
/// about; the two owner-mode view frames install the expectation a stream's
/// first cells are checked against. v2 also lists `terminalStreamResult`, whose
/// control is not ported here (`terminal_view/mod.rs`), so its refusal waits
/// its turn like any other frame.
fn bypasses_ordered_lane(frame: &CoordWorkerUpstream) -> bool {
    matches!(
        frame,
        CoordWorkerUpstream::InputResult(_)
            | CoordWorkerUpstream::TerminalViewState(_)
            | CoordWorkerUpstream::TerminalViewProjection(_)
    )
}

/// The completion lane beside one socket's ordered lane.
pub(super) struct ResultLane {
    /// A dispatcher over the same handle and process state as the ordered one.
    /// Only its synchronous arm is called, so the two share every piece of
    /// state; a second value exists because the ordered one is mutably borrowed
    /// by the append this lane runs beside.
    dispatcher: WorkerFrameDispatcher,
    /// Everything else that arrived during an append, in arrival order.
    backlog: FrameQueue,
    /// A close this lane decided while the append ran, applied once it settles.
    close: Option<SocketClose>,
}

impl ResultLane {
    /// A lane over this socket's second dispatcher, with v2's queue bounds.
    pub(super) fn new(dispatcher: WorkerFrameDispatcher) -> Self {
        Self {
            dispatcher,
            backlog: FrameQueue::new(),
            close: None,
        }
    }

    /// Await one durable append while the socket keeps delivering.
    ///
    /// The append is never cancelled: an overflow or a peer that went away
    /// stops the reading, and the close waits for the append to settle, so a
    /// committed event is never torn from its acknowledgement and publication.
    pub(super) async fn await_append(
        &mut self,
        mut append: DispatchFuture<'_>,
        socket: &mut WebSocket,
    ) -> DispatchOutcome {
        let mut reading = !self.backlog.is_latched();
        loop {
            tokio::select! {
                biased;
                outcome = &mut append => return outcome,
                received = socket.recv(), if reading => {
                    reading = match received {
                        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => self.discard_backlog(),
                        Some(Ok(message)) => message_bytes(message).is_none_or(|bytes| self.accept(bytes)),
                    };
                }
            }
        }
    }

    /// The oldest frame that waited behind an append; its bytes stay charged
    /// until [`Self::release_backlogged`].
    pub(super) fn take_backlogged(&mut self) -> Option<QueuedFrame> {
        self.backlog.take_front()
    }

    /// Give back a drained frame's bytes once its dispatch settled.
    pub(super) fn release_backlogged(&mut self, frame: &QueuedFrame) {
        self.backlog.release(frame);
    }

    /// The close decided while an append ran, if any.
    pub(super) fn take_close(&mut self) -> Option<SocketClose> {
        self.close.take()
    }

    /// One message read during an append; false stops the reading.
    fn accept(&mut self, bytes: Bytes) -> bool {
        let frame = match decode_link_frame(&bytes) {
            Ok(frame) => frame,
            Err(error) => {
                tracing::warn!(worker_fp = %self.dispatcher.handle.worker_fp, %error,
                    "worker link: decode_failed; the frame is ignored");
                return true;
            }
        };
        if let LinkFrame::Dispatch(inbound) = frame
            && bypasses_ordered_lane(&inbound.frame)
        {
            self.dispatch_now(*inbound);
            return true;
        }
        // Stored as bytes, the queue's one representation, and decoded again
        // when drained: only a frame that arrived during an append pays that.
        match self.backlog.push(QueuedFrame::new(bytes.to_vec(), 0)) {
            Queued::Admitted { .. } => true,
            Queued::Refused(_) => {
                tracing::warn!(worker_fp = %self.dispatcher.handle.worker_fp,
                    depth = self.backlog.depth(), charged_bytes = self.backlog.charged_bytes(),
                    "worker link: queue_overflow; closing");
                self.close.get_or_insert(SocketClose::QueueOverflow);
                false
            }
        }
    }

    /// One completion, through the same readiness gate the ordered lane applies.
    fn dispatch_now(&mut self, frame: InboundFrame) {
        let handle = &self.dispatcher.handle;
        if handle.is_revoked() || !handle.is_ready() {
            tracing::debug!(worker_fp = %handle.worker_fp, frame = frame.frame.kind(),
                "worker link: frame_before_snapshot_ready; dropped");
            return;
        }
        if let DispatchOutcome::Close(close) =
            self.dispatcher.handle_now(handle.worker_fp.as_str(), frame)
        {
            self.close.get_or_insert(close);
        }
    }

    /// The peer went away mid-append: v2 closes the queue, dropping what waited.
    fn discard_backlog(&mut self) -> bool {
        self.backlog.latch();
        while let Some(frame) = self.backlog.take_front() {
            self.backlog.release(&frame);
        }
        tracing::debug!(worker_fp = %self.dispatcher.handle.worker_fp,
            "worker link: the peer ended during an append; its backlog is dropped");
        false
    }
}
