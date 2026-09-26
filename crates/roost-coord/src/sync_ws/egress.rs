//! Bounded queue admission and the flush turn: which frame this socket sends
//! next, and what the socket must be told when it cannot send any.
//!
//! Owned by the Sync session. v2 kept this in `sync-ws-v2-egress.ts` (396
//! lines) plus `sync-ws-v2-scheduler.ts`, which existed only to re-export it.
//! The scheduler earns no file here: the two things it named are the admission
//! below and the flush turn below, on one type, with nothing in between to
//! forward.
//!
//! THE FLUSH IS A TURN, NOT A LOOP, AND IT COMMITS BEFORE THE CALLER SENDS.
//! v2's `flushV2` sent up to 64 frames in one turn with `ws.send` in the middle:
//! it prepared the frame, checked the window, sent, and only then dequeued and
//! charged. A Rust port has no socket, so the turn hands the caller the next
//! frame. That is only sound because the turn keeps v2's ORDER -- prepare, then
//! check the window, then dequeue -- and the one rule that goes with it:
//! **a frame the turn returns must be delivered or the session retired.** There
//! is no path that does neither, and a caller that finds one should retire the
//! session rather than retry, because the terminal lane has already advanced.
//!
//! WHY A FULL WINDOW STALLS AND DOES NOT CLOSE. v2's v2 egress returns from the
//! flush when the unacknowledged window is full; it does not close, unlike the
//! v1 send path. A browser tab that stopped dispatching gets a paused socket
//! that resumes on the next acknowledgement, which is the difference between a
//! reconnect and a reload loop. The socket-buffer limits (high water, timeout)
//! and a dropped frame DO close, and those are the caller's to observe, because
//! only the caller can see a write fail.

use roost_proto::buffa::Message;
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::{FirehoseFrame, SyncDomain};

use crate::sync_ws::frame_meta::SyncFrameMeta;
use crate::sync_ws::retained_frame::{ChunkTransfer, RetainedFrame};
use crate::sync_ws::session::SyncV2Session;
use crate::sync_ws::terminal::snapshot::TerminalSnapshotHub;

/// What one flush turn produced.
#[derive(Debug)]
pub enum FlushStep {
    /// This frame is ready to encode and write. The session has dequeued it and
    /// advanced the terminal lane, so the caller must send it or retire the
    /// session.
    Send(SendableFrame),
    /// Nothing is eligible right now.
    Idle,
    /// The unacknowledged window is full. The socket is healthy and waiting for
    /// an acknowledgement; this is not a fault and closes nothing.
    Stalled,
}

/// One frame, stamped and measured, ready for the wire.
#[derive(Debug)]
pub struct SendableFrame {
    /// The delivery sequence this frame carries. Zero on a socket that did not
    /// negotiate flow control, which is the value a control carries too.
    pub delivery_seq: u64,
    /// The frame, with the envelope scalars and the fan-out stamp set.
    pub frame: FirehoseFrame,
    /// The oneof case name, for a close reason and a log line.
    pub kind: &'static str,
    /// Set for one part of a chunked baseline, so a flush can log how long the
    /// coordinator held it.
    pub chunk_transfer: Option<ChunkTransfer>,
}

impl SendableFrame {
    /// The exact encoded size, which is what the window is charged for a
    /// non-terminal frame and for the window's own accounting for any frame.
    #[must_use]
    pub fn encoded_len(&self) -> u64 {
        u64::from(self.frame.encoded_len())
    }
}

impl SyncV2Session {
    /// The next frame this socket may send, or why it may send nothing.
    ///
    /// This is one turn of v2's flush loop, and it does the loop's bookkeeping
    /// in the loop's order: select; if nothing was eligible, pump one deferred
    /// terminal lane and try again; check the window; stamp; dequeue; advance
    /// the lane; pump once more, so a lane that just delivered something can
    /// send its next frame without waiting for the next enqueue.
    pub fn take_next_sendable(
        &mut self,
        now_ms: u64,
        hub: &mut dyn TerminalSnapshotHub,
    ) -> FlushStep {
        if self.pressure_closing {
            return FlushStep::Idle;
        }
        let ack_seq = self.acknowledged_sequence();
        let mut selection = self.select_candidate(now_ms, ack_seq);
        if selection.is_none() {
            self.pump_next_ready_lane(now_ms, hub);
            selection = self.select_candidate(now_ms, ack_seq);
        }
        let Some(selection) = selection else {
            return FlushStep::Idle;
        };
        let candidate = selection.candidate;
        let Some(domain) = self.domains.get(candidate.slot) else {
            return FlushStep::Idle;
        };
        let terminal = domain.domain == SyncDomain::Terminal;
        let (estimated_bytes, meta) = {
            let queued = &domain.queue[candidate.index];
            (queued.estimated_bytes(), queued.meta.clone())
        };
        let delivery_seq = self.next_delivery_seq();
        // A terminal frame is measured against its CONSERVATIVE estimate, which
        // is at least its real encoded size because the estimate was taken with
        // the widest scalars the stamp can occupy. A non-terminal frame is
        // measured against the frame that is about to be written, so the check
        // below re-runs with the real size. Both leave the frame QUEUED, which
        // is what makes a stall recoverable on the next acknowledgement.
        if terminal && !self.window_has_room(estimated_bytes) {
            return FlushStep::Stalled;
        }
        let fanout_ms = self.snapshot_fanout_stamp(&meta, now_ms);
        let frame = self.domains[candidate.slot].queue[candidate.index]
            .frame
            .outbound_copy(delivery_seq, fanout_ms);
        if !terminal && !self.window_has_room(u64::from(frame.encoded_len())) {
            return FlushStep::Stalled;
        }
        let kind = frame_kind(&frame);
        let chunk_transfer = chunk_transfer_of(&frame, now_ms, fanout_ms);
        let delivered = self.take_selected(candidate);
        self.apply_delivered_lifecycle(&delivered, delivery_seq);
        self.on_terminal_frame_delivered(&meta);
        self.pump_next_ready_lane(now_ms, hub);
        FlushStep::Send(SendableFrame {
            delivery_seq,
            frame,
            kind,
            chunk_transfer,
        })
    }

    /// Apply what a delivered frame ASSERTS: which sessions it announces, and
    /// which it closes.
    ///
    /// The announcement is recorded with the sequence it went out under, and a
    /// cell may not pass it until the client has ACKed that sequence -- a client
    /// that has not processed the `opened` event folds a cell for an unknown
    /// session by discarding it, so delivering the cell first loses the frame
    /// rather than reordering it. A close releases the session's announcement
    /// AND drops the cells already queued for it: they describe output from a
    /// session that no longer exists.
    fn apply_delivered_lifecycle(&mut self, delivered: &RetainedFrame, delivery_seq: u64) {
        for session_id in &delivered.meta.announces {
            self.announced_sessions.insert(session_id.clone());
            self.pending_session_announcements
                .insert(session_id.clone(), delivery_seq);
        }
        if delivered.meta.closes.is_empty() {
            return;
        }
        for session_id in &delivered.meta.closes {
            self.announced_sessions.remove(session_id);
            self.pending_session_announcements.remove(session_id);
            self.delete_terminal_session(session_id);
        }
        self.remove_queued_cells(&delivered.meta.closes);
    }

    /// The fan-out stamp for one frame, taken once per chunked snapshot.
    ///
    /// Every part of one baseline must carry the SAME stamp, or a browser that
    /// sums the transfer times of a snapshot sees the coordinator's queueing
    /// delay counted once per part. Deltas are stamped per frame, because a
    /// delta has no snapshot to belong to.
    fn snapshot_fanout_stamp(&mut self, meta: &SyncFrameMeta, now_ms: u64) -> u64 {
        let (Some(session_id), Some(sent_index)) =
            (meta.session_id.as_deref(), meta.terminal_cursor_index)
        else {
            return now_ms;
        };
        let stream_matches = self
            .terminal_lane(session_id)
            .and_then(|lane| lane.cursor.as_ref())
            .is_some_and(|cursor| {
                cursor.source.is_some()
                    && cursor.index == sent_index
                    && Some(cursor.stream_id.as_str()) == meta.terminal_stream_id.as_deref()
            });
        if !stream_matches {
            return now_ms;
        }
        let snapshot_ms = self
            .terminal_lane(session_id)
            .and_then(|lane| lane.cursor.as_ref())
            .and_then(|cursor| cursor.fanout_ms)
            .unwrap_or(now_ms);
        if let Some(cursor) = self
            .terminal_sessions
            .get_mut(session_id)
            .and_then(|lane| lane.cursor.as_mut())
        {
            cursor.fanout_ms = Some(snapshot_ms);
        }
        snapshot_ms
    }
}

/// The one part of a chunked baseline a frame carries, for one log line.
fn chunk_transfer_of(
    frame: &FirehoseFrame,
    sent_at_ms: u64,
    fanout_ms: u64,
) -> Option<ChunkTransfer> {
    let Some(Frame::CellGridChunk(chunk)) = &frame.frame else {
        return None;
    };
    let part = chunk.part.as_option()?;
    Some(ChunkTransfer {
        session_id: part.session_id.clone(),
        snapshot_id: chunk.snapshot_id.clone(),
        chunk_index: chunk.chunk_index,
        chunk_count: chunk.chunk_count,
        transfer_ms: sent_at_ms.saturating_sub(fanout_ms),
    })
}

/// The oneof case name, for a close reason and a log line.
#[must_use]
pub fn frame_kind(frame: &FirehoseFrame) -> &'static str {
    match &frame.frame {
        Some(Frame::Sessions(_)) => "sessions",
        Some(Frame::SessionPresence(_)) => "session_presence",
        Some(Frame::AuditRow(_)) => "audit_row",
        Some(Frame::SessionEvent(_)) => "session_event",
        Some(Frame::WorkspaceDelta(_)) => "workspace_delta",
        Some(Frame::TaskDelta(_)) => "task_delta",
        Some(Frame::McpMsg(_)) => "mcp_msg",
        Some(Frame::WorkerPresence(_)) => "worker_presence",
        Some(Frame::WorkerRoutable(_)) => "worker_routable",
        Some(Frame::CellGrid(_)) => "cell_grid",
        Some(Frame::CellGridChunk(_)) => "cell_grid_chunk",
        Some(Frame::TerminalTitle(_)) => "terminal_title",
        Some(Frame::LastActivity(_)) => "last_activity",
        Some(Frame::PairRequestDelta(_)) => "pair_request_delta",
        Some(Frame::UiState(_)) => "ui_state",
        Some(Frame::UiCommand(_)) => "ui_command",
        Some(Frame::Keepalive(_)) => "keepalive",
        Some(Frame::CoordinatorRelocation(_)) => "coordinator_relocation",
        Some(Frame::AgentStatus(_)) => "agent_status",
        Some(Frame::Subscribed(_)) => "subscribed",
        Some(Frame::DomainReset(_)) => "domain_reset",
        Some(Frame::InputAccepted(_)) => "input_accepted",
        Some(Frame::InputRejected(_)) => "input_rejected",
        Some(Frame::InputAmbiguous(_)) => "input_ambiguous",
        Some(Frame::TerminalViewState(_)) => "terminal_view_state",
        Some(Frame::InputRouteResult(_)) => "input_route_result",
        Some(Frame::TerminalTransportProbeResult(_)) => "terminal_transport_probe_result",
        None => "application",
    }
}
