//! The frames a worker link lets past a durable append that is still in flight,
//! and the bounded ordered backlog every other frame waits in meanwhile.
//! Ports the completion-frame lane of `apps/coord/src/workers/worker-ws-handler.ts:235-254`,
//! its terminal fast path through the announced-channel barrier (`:281-338`, in
//! `worker_link::announced_lane`, which charges the backlog's budget) and its
//! bounded slow-path queue (`:347-349`) over `worker_link::frame_queue`.
//! Owned by `worker_link::link_session`; `worker_link::connection` drains the backlog.

use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket};
use roost_protocol::wire::coord_worker::CoordWorkerUpstream;

use crate::worker_link::announced_lane::{AnnouncedLane, Announcement, is_terminal_frame};
use crate::worker_link::conn_types::SocketClose;
use crate::worker_link::dispatch::{DispatchFuture, DispatchOutcome, FrameDispatch, InboundFrame};
use crate::worker_link::downstream_write::{DownstreamOutbox, write_downstream};
use crate::worker_link::frame_dispatch::WorkerFrameDispatcher;
use crate::worker_link::frame_queue::{FrameQueue, Queued, QueuedFrame};
use crate::worker_link::retained_budget::RetainOutcome;
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
    /// Everything else that arrived during an append, in arrival order. Its
    /// budget is the socket's, shared with the announced lane.
    backlog: FrameQueue,
    /// The announced-channel barrier and its parked semantic metadata.
    announced: AnnouncedLane,
    /// A close this lane decided while the append ran, applied once it settles.
    close: Option<SocketClose>,
}

impl ResultLane {
    /// A lane over this socket's second dispatcher, with v2's queue bounds.
    pub(super) fn new(dispatcher: WorkerFrameDispatcher) -> Self {
        Self {
            announced: AnnouncedLane::new(&dispatcher),
            dispatcher,
            backlog: FrameQueue::new(),
            close: None,
        }
    }

    /// Await one durable append while the socket keeps delivering and the
    /// outbound queue keeps draining, so input bound for this worker never
    /// waits out a database write.
    ///
    /// The append is never cancelled: an overflow or a peer that went away
    /// stops the reading, and the close waits for the append to settle, so a
    /// committed event is never torn from its acknowledgement and publication.
    pub(super) async fn await_append(
        &mut self,
        mut append: DispatchFuture<'_>,
        socket: &mut WebSocket,
        outbox: &mut DownstreamOutbox,
    ) -> DispatchOutcome {
        let started = std::time::Instant::now();
        let mut reading = !self.backlog.is_latched();
        let outcome = loop {
            tokio::select! {
                biased;
                outcome = &mut append => break outcome,
                queued = outbox.recv(), if self.close.is_none() => {
                    let written = match queued {
                        Some(frame) => {
                            write_downstream(socket, &frame, &self.dispatcher.handle.worker_fp).await.is_ok()
                        }
                        None => false,
                    };
                    if !written {
                        self.close.get_or_insert(SocketClose::Default);
                        reading = self.discard_backlog();
                    }
                }
                received = socket.recv(), if reading => {
                    reading = match received {
                        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => self.discard_backlog(),
                        Some(Ok(message)) => message_bytes(message).is_none_or(|bytes| self.accept(bytes)),
                    };
                }
            }
        };
        let append_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        tracing::debug!(target: "worker_link", worker_fp = %self.dispatcher.handle.worker_fp,
            append_ms, "a durable append settled");
        outcome
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

    /// Announce a channel a durable frame will bind, as it is read.
    pub(super) fn announce(&mut self, announcement: &Announcement) {
        self.announced
            .announce(announcement, self.backlog.budget_mut());
    }

    /// v2's terminal fast path: `Some` publishes now; `None` is held, retained
    /// or dropped by the barrier (whose overflow closes the socket 1009).
    pub(super) fn route_terminal(
        &mut self,
        inbound: InboundFrame,
        encoded_bytes: usize,
    ) -> Option<InboundFrame> {
        let encoded_bytes = u64::try_from(encoded_bytes).unwrap_or(u64::MAX);
        let budget = self.backlog.budget_mut();
        let routed =
            self.announced
                .route_terminal(inbound, encoded_bytes, &self.dispatcher, budget);
        self.note_budget_overflow();
        routed
    }

    /// After a durable append settled, commit or fail what it announced.
    pub(super) fn commit_announced(
        &mut self,
        announcement: Option<Announcement>,
        appended: DispatchOutcome,
    ) -> DispatchOutcome {
        let budget = self.backlog.budget_mut();
        self.announced
            .commit_after_append(announcement, appended, &self.dispatcher, budget)
    }

    /// Charge the durable frame whose append is about to run, as v2's queue
    /// charges its in-flight frame; false when the socket budget refused it.
    pub(super) fn charge_in_flight(&mut self, encoded_bytes: usize) -> bool {
        let bytes = u64::try_from(encoded_bytes).unwrap_or(u64::MAX);
        let charged = self.backlog.budget_mut().retain(bytes) == RetainOutcome::Retained;
        self.note_budget_overflow();
        charged
    }

    /// Give back the in-flight charge once the append and its commit settled.
    pub(super) fn release_in_flight(&mut self, encoded_bytes: usize) {
        let bytes = u64::try_from(encoded_bytes).unwrap_or(u64::MAX);
        self.backlog.budget_mut().release(bytes);
    }

    /// End every announced wait that ran out.
    pub(super) fn expire_announced(&mut self, now: tokio::time::Instant) {
        self.announced.expire(now, self.backlog.budget_mut());
    }

    /// When the next announced wait ends, for the read loop's timer.
    pub(super) fn next_announced_deadline(&self) -> Option<tokio::time::Instant> {
        self.announced.next_deadline()
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
        if let LinkFrame::Dispatch(inbound) = frame {
            if bypasses_ordered_lane(&inbound.frame) {
                self.dispatch_now(*inbound);
                return true;
            }
            if is_terminal_frame(&inbound.frame) {
                if self.is_live() {
                    if let Some(now) = self.route_terminal(*inbound, bytes.len()) {
                        self.dispatch_now(now);
                    }
                } else {
                    tracing::debug!(worker_fp = %self.dispatcher.handle.worker_fp,
                        "worker link: frame_before_snapshot_ready; dropped");
                }
                return self.close != Some(SocketClose::QueueOverflow);
            }
            if let Some(announcement) =
                Announcement::of(&inbound, &self.dispatcher.handle.worker_fp)
            {
                self.announce(&announcement);
            }
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

    /// Whether this generation may carry live frames: current and ready.
    fn is_live(&self) -> bool {
        !self.dispatcher.handle.is_revoked() && self.dispatcher.handle.is_ready()
    }

    /// The socket budget latched on an overflow: log it once and close 1009.
    fn note_budget_overflow(&mut self) {
        let Some(overflow) = self.backlog.budget().overflow() else {
            return;
        };
        if self.close == Some(SocketClose::QueueOverflow) {
            return;
        }
        tracing::warn!(worker_fp = %self.dispatcher.handle.worker_fp, frames = overflow.frames,
            bytes = overflow.bytes, rejected_bytes = overflow.rejected_bytes,
            announced = ?self.announced.stats(), "worker link: queue_overflow; closing");
        self.close = Some(SocketClose::QueueOverflow);
    }

    /// One completion, through the same readiness gate the ordered lane applies.
    fn dispatch_now(&mut self, frame: InboundFrame) {
        let handle = &self.dispatcher.handle;
        if !self.is_live() {
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
