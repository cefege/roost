//! What INBOUND terminal material may join a session's lane.
//!
//! Owned by the Sync session; `cursor.rs` holds the lane's data. v2 kept both in
//! `sync-ws-v2-terminal.ts`; what changed is the shape, not the policy. The rule
//! the ingress exists to enforce is that ONE session's baseline is never
//! interleaved with its own deltas or with another session's snapshot, and that
//! under pressure the SESSION rebaselines rather than the socket closing --
//! because a fresh baseline is recoverable and lost semantic state is not.
use roost_proto::{FirehoseFrame, SyncDomain};

use crate::sync_ws::ack_window::BackpressureReason;
use crate::sync_ws::domain_table::{
    TERMINAL_LANE_MAX_DELTA_BYTES, TERMINAL_LANE_MAX_DELTA_FRAMES,
};
use crate::sync_ws::frame_meta::SyncFrameMeta;
use crate::sync_ws::retained_frame::{OwnedFrame, RetainedFrame, SharedCellFrame};
use crate::sync_ws::session::{SessionClose, SyncV2Session};

use super::cursor::{SnapshotCursor, TerminalLane};
use super::snapshot::{TerminalSnapshotHub, TerminalSnapshotSource};

/// What the terminal view hub's delta sink must do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalDeltaOutcome {
    /// The socket now holds the frame.
    Queued,
    /// The frame was dropped, but a rebaseline is already in flight or the
    /// domain moved under it. The hub owes this socket nothing.
    Handled,
    /// The frame was dropped and nothing is on its way. The hub must install a
    /// canonical full for this session.
    NeedsSnapshot,
}

impl SyncV2Session {
    /// One session's lane, if this socket carries terminal traffic for it.
    pub(in crate::sync_ws) fn terminal_lane(&self, session_id: &str) -> Option<&TerminalLane> {
        self.terminal_sessions.get(session_id)
    }

    /// Charge one terminal frame to this socket, or `None` when the budget is
    /// full. The caller decides what a refusal means: a cell rebaselines, a
    /// view-state closes the socket.
    pub(in crate::sync_ws) fn retain_terminal_frame(
        &mut self,
        frame: &FirehoseFrame,
        session_id: &str,
        now_ms: u64,
    ) -> Option<RetainedFrame> {
        let generation = self.domain_generation(SyncDomain::Terminal)?;
        let owned = OwnedFrame::of_copy(frame, SyncDomain::Terminal, generation);
        let charge = self.try_charge(&owned)?;
        Some(RetainedFrame::new(
            owned,
            SyncFrameMeta::cell(session_id),
            now_ms,
            charge,
        ))
    }

    /// Charge one materialized baseline part to a lane's cursor.
    pub(in crate::sync_ws) fn retain_cursor_part(
        &mut self,
        session_id: &str,
        cell: SharedCellFrame,
        now_ms: u64,
    ) -> Option<RetainedFrame> {
        let generation = self.domain_generation(SyncDomain::Terminal)?;
        let owned = OwnedFrame::of_shared_cell(cell, generation);
        let charge = self.try_charge(&owned)?;
        Some(RetainedFrame::new(
            owned,
            SyncFrameMeta::cell(session_id),
            now_ms,
            charge,
        ))
    }

    /// Start, or restart, a session's terminal stream on this socket.
    ///
    /// Returns `false` when the stream is already the one this lane carries,
    /// which is the idempotent case the view hub re-asks for on every claim. A
    /// DIFFERENT stream id is a new baseline: the queued cells, the cursor and
    /// the outstanding rebaseline are retired, and the first part of the new
    /// baseline may pass other sessions' deltas once.
    pub fn begin_terminal_stream(&mut self, session_id: &str, stream_id: &str) -> bool {
        if self.terminal_lane(session_id).is_some_and(|lane| lane.stream_id == stream_id) {
            return false;
        }
        if !self.terminal_sessions.contains_key(session_id) {
            self.terminal_sessions
                .insert(session_id.to_owned(), TerminalLane::new(stream_id, true));
        } else {
            self.drop_from_ready_ring(session_id);
            self.remove_terminal_queued(session_id, false);
            if let Some(mut lane) = self.terminal_sessions.remove(session_id) {
                lane.ready = false;
                lane.release_cursor(self);
                lane.stream_id = stream_id.to_owned();
                lane.rebaseline_pending = false;
                lane.attach_priority_pending = true;
                self.terminal_sessions.insert(session_id.to_owned(), lane);
            }
        }
        let owed_states = self
            .terminal_lane(session_id)
            .is_some_and(|lane| !lane.pending_states.is_empty() && !lane.state_queued);
        if owed_states {
            self.mark_terminal_lane_ready(session_id);
            self.pump_terminal_lane(session_id, 0, &mut super::snapshot::NoTerminalSnapshotHub);
        }
        true
    }

    /// Answer a `terminalViewState` for one session.
    ///
    /// The state is the semantic part of the terminal domain: it says which
    /// stream a view resolved to, and a client cannot reconstruct it. So it
    /// goes in ahead of queued cells, and if it does not fit even after the
    /// lane's own unsent material is discarded, the socket closes rather than
    /// leaving the client waiting out a deadline for an answer that will never
    /// come (`sync-ws-v2-terminal.ts:174-206`).
    pub fn enqueue_terminal_state(
        &mut self,
        frame: &FirehoseFrame,
        session_id: &str,
        now_ms: u64,
        hub: &mut dyn TerminalSnapshotHub,
    ) -> Result<(), SessionClose> {
        if self.domain(SyncDomain::Terminal).is_none() {
            return Ok(());
        }
        if !self.terminal_sessions.contains_key(session_id) {
            self.terminal_sessions
                .insert(session_id.to_owned(), TerminalLane::new("", false));
        }
        let retained = match self.retain_terminal_frame(frame, session_id, now_ms) {
            Some(retained) => Some(retained),
            None => {
                self.request_scoped_rebaseline(session_id, "terminal_state_priority");
                self.retain_terminal_frame(frame, session_id, now_ms)
            }
        };
        let Some(retained) = retained else {
            let close = SessionClose::Backpressure {
                reason: BackpressureReason::ByteLimit,
                stats: self.window_stats(now_ms),
            };
            self.fault(close);
            return Err(close);
        };
        if let Some(lane) = self.terminal_sessions.get_mut(session_id) {
            lane.pending_states.push(retained);
        }
        self.mark_terminal_lane_ready(session_id);
        self.pump_terminal_lane(session_id, now_ms, hub);
        Ok(())
    }

    /// Install a canonical full for one session, replacing any baseline the
    /// lane had not started sending.
    ///
    /// Returns `false` when the stream is not this lane's, when the source has
    /// no parts, or when a baseline is already part-way out -- in that last case
    /// the lane records that a rebaseline is owed and waits for the current
    /// baseline to finish, because abandoning a half-delivered snapshot leaves
    /// the client with rows from two different snapshots
    /// (`sync-ws-v2-terminal.ts:208-241`).
    pub fn replace_terminal_snapshot(
        &mut self,
        session_id: &str,
        stream_id: &str,
        source: &dyn TerminalSnapshotSource,
        snapshot_id: &str,
        now_ms: u64,
        hub: &mut dyn TerminalSnapshotHub,
    ) -> bool {
        let Some(lane) = self.terminal_lane(session_id) else {
            return false;
        };
        if lane.stream_id != stream_id {
            return false;
        }
        if lane.cursor.as_ref().is_some_and(SnapshotCursor::is_mid_baseline) {
            if let Some(lane) = self.terminal_sessions.get_mut(session_id) {
                lane.rebaseline_pending = true;
            }
            self.mark_terminal_lane_ready(session_id);
            return false;
        }
        let Some(cursor) = source.create_cursor(snapshot_id) else {
            return false;
        };
        if cursor.part_count() == 0 {
            return false;
        }
        self.discard_unsent_materialization(session_id);
        if let Some(mut lane) = self.terminal_sessions.remove(session_id) {
            lane.cursor = Some(SnapshotCursor::of_snapshot(stream_id, cursor));
            lane.rebaseline_pending = false;
            self.terminal_sessions.insert(session_id.to_owned(), lane);
        }
        self.mark_terminal_lane_ready(session_id);
        self.pump_terminal_lane(session_id, now_ms, hub);
        true
    }

    /// Buffer one delta for a session, or rebaseline the session instead.
    pub fn enqueue_terminal_delta(
        &mut self,
        session_id: &str,
        stream_id: &str,
        frame: &FirehoseFrame,
        now_ms: u64,
        hub: &mut dyn TerminalSnapshotHub,
    ) -> TerminalDeltaOutcome {
        let generation = self.domain_generation(SyncDomain::Terminal);
        if self.admit_terminal_delta(session_id, stream_id, frame, now_ms, hub) {
            return TerminalDeltaOutcome::Queued;
        }
        if self.terminal_rebaseline_pending(session_id, stream_id) {
            return TerminalDeltaOutcome::Handled;
        }
        // A domain that moved under the call means a reset is already in
        // flight, so this frame belongs to a generation that no longer exists
        // and the hub owes this socket nothing more.
        if self.pressure_closing
            || generation.is_none()
            || self.domain_generation(SyncDomain::Terminal) != generation
        {
            return TerminalDeltaOutcome::Handled;
        }
        TerminalDeltaOutcome::NeedsSnapshot
    }

    /// Whether this session's lane owes a canonical full for this stream.
    pub fn terminal_rebaseline_pending(&self, session_id: &str, stream_id: &str) -> bool {
        self.terminal_lane(session_id)
            .is_some_and(|lane| lane.stream_id == stream_id && lane.rebaseline_pending)
    }

    /// Forget a session's terminal lane and everything it still holds.
    pub fn drop_terminal_session(&mut self, session_id: &str) {
        self.delete_terminal_session(session_id);
    }

    /// Forget every session's terminal lane, for a terminal domain reset.
    pub fn clear_terminal_sessions(&mut self) {
        let lanes = std::mem::take(&mut self.terminal_sessions);
        for mut lane in lanes.into_values() {
            lane.release_all(self);
        }
        self.terminal_ready_sessions.clear();
    }

    /// Forget one session's lane, dropping its queued frames with it.
    pub(in crate::sync_ws) fn delete_terminal_session(&mut self, session_id: &str) {
        if !self.terminal_sessions.contains_key(session_id) {
            return;
        }
        self.remove_terminal_queued(session_id, true);
        if let Some(mut lane) = self.terminal_sessions.remove(session_id) {
            lane.release_all(self);
        }
        self.drop_from_ready_ring(session_id);
    }

    fn admit_terminal_delta(
        &mut self,
        session_id: &str,
        stream_id: &str,
        frame: &FirehoseFrame,
        now_ms: u64,
        hub: &mut dyn TerminalSnapshotHub,
    ) -> bool {
        let admissible = self
            .terminal_lane(session_id)
            .is_some_and(|lane| lane.stream_id == stream_id && !lane.rebaseline_pending);
        if !admissible {
            return false;
        }
        // A delta for a stream with no baseline yet opens a delta-only cursor,
        // which is what lets a session stream before it has ever been attached.
        if self
            .terminal_lane(session_id)
            .and_then(|lane| lane.cursor.as_ref())
            .is_none()
            && let Some(mut lane) = self.terminal_sessions.remove(session_id)
        {
            lane.cursor = Some(SnapshotCursor::of_stream(stream_id));
            self.terminal_sessions.insert(session_id.to_owned(), lane);
        }
        // The frame count is checked before anything is charged and the byte
        // count after, which is the only order that lets a single huge delta be
        // refused without first taking a charge it would immediately release.
        let too_many = self
            .terminal_lane(session_id)
            .and_then(|lane| lane.cursor.as_ref())
            .is_some_and(|cursor| cursor.delta_tail.len() + 1 > TERMINAL_LANE_MAX_DELTA_FRAMES);
        if too_many {
            self.request_scoped_rebaseline(session_id, "terminal_delta_lane_pressure");
            return false;
        }
        let Some(retained) = self.retain_terminal_frame(frame, session_id, now_ms) else {
            self.request_scoped_rebaseline(session_id, "terminal_aggregate_pressure");
            return false;
        };
        let estimated_bytes = retained.estimated_bytes();
        let too_wide = self
            .terminal_lane(session_id)
            .and_then(|lane| lane.cursor.as_ref())
            .is_some_and(|cursor| {
                cursor.delta_bytes + estimated_bytes > TERMINAL_LANE_MAX_DELTA_BYTES
            });
        if too_wide {
            let mut retained = retained;
            self.release_charge(&mut retained);
            self.request_scoped_rebaseline(session_id, "terminal_delta_lane_pressure");
            return false;
        }
        if let Some(lane) = self.terminal_sessions.get_mut(session_id)
            && let Some(cursor) = lane.cursor.as_mut()
        {
            cursor.delta_tail.push(retained);
            cursor.delta_bytes += estimated_bytes;
        }
        self.mark_terminal_lane_ready(session_id);
        self.pump_terminal_lane(session_id, now_ms, hub);
        true
    }

    /// Record that this session owes a canonical full, discarding whatever it
    /// had not sent yet.
    ///
    /// The log line is emitted only on the FIRST request for a session, because
    /// a lane under pressure asks repeatedly and one line per request is how a
    /// log becomes unreadable exactly when it is needed.
    pub(in crate::sync_ws) fn request_scoped_rebaseline(&mut self, session_id: &str, reason: &'static str) {
        let already_pending = self
            .terminal_lane(session_id)
            .is_some_and(|lane| lane.rebaseline_pending);
        if !already_pending {
            tracing::warn!(
                event = "sync-ws",
                action = "terminal_lane_rebaseline",
                session_id,
                reason,
                "a terminal lane is dropping its own material and asking for a full"
            );
        }
        if let Some(lane) = self.terminal_sessions.get_mut(session_id) {
            lane.rebaseline_pending = true;
        }
        self.discard_unsent_materialization(session_id);
        self.mark_terminal_lane_ready(session_id);
    }

    /// Throw away what this lane has not sent.
    ///
    /// A baseline that is part-way out is KEPT and only its buffered deltas go,
    /// because the client already holds earlier parts of that snapshot and
    /// dropping them leaves two snapshots interleaved in one grid.
    pub(in crate::sync_ws) fn discard_unsent_materialization(&mut self, session_id: &str) {
        let mid_baseline = self
            .terminal_lane(session_id)
            .and_then(|lane| lane.cursor.as_ref())
            .is_some_and(SnapshotCursor::is_mid_baseline);
        if mid_baseline {
            if let Some(mut lane) = self.terminal_sessions.remove(session_id) {
                lane.release_delta_tail(self);
                self.terminal_sessions.insert(session_id.to_owned(), lane);
            }
            return;
        }
        self.remove_terminal_queued(session_id, false);
        if let Some(mut lane) = self.terminal_sessions.remove(session_id) {
            lane.release_cursor(self);
            self.terminal_sessions.insert(session_id.to_owned(), lane);
        }
    }
}
