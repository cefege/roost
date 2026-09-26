//! The deduplicated per-socket ring of terminal lanes that have something to
//! send, and the one-head materialisation that walks them.
//!
//! Owned by the Sync session. v2 kept this in `sync-ws-v2-terminal-ready.ts`
//! and the reason it was split out survives the port: egress asks this owner
//! for the lane it just delivered to, or for one deferred retry, and NEVER
//! scans every session. A socket carrying twelve terminals must not cost
//! twelve cursor walks per flushed frame.
//!
//! ONE HEAD AT A TIME IS THE POINT. A lane's baseline parts, its view-states
//! and its deltas go out in a strict order per session, and the only reason
//! another session's frame may pass one of them is the priority insert in
//! `send_queue`. Everything else waits its turn, which is why the ring is
//! deduplicated and why a lane that cannot send right now re-enters it at the
//! BACK rather than being retried in place.
//!
//! EVERY TURN IS DECIDED IN TWO PHASES. Deciding needs an immutable read of the
//! lane; charging a frame writes the socket-wide counters. Rust cannot lend the
//! lane while the counters are written, so each pump first decides what the lane
//! WOULD send, then charges, then mutates. Nothing runs between those phases
//! that could change the lane, so the decision stays true.

use roost_proto::SyncDomain;

use crate::sync_ws::frame_meta::{FeedLane, SyncFrameMeta};
use crate::sync_ws::session::SyncV2Session;

use super::snapshot::TerminalSnapshotHub;

/// What one turn of the cursor pump achieved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CursorPump {
    /// A frame is now queued for this lane.
    Queued,
    /// The lane could not queue anything and should be tried again later.
    Blocked,
    /// The lane has nothing left to send.
    Empty,
}

/// What a lane would send next, decided before anything is charged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CursorPlan {
    /// The next part of the lane's baseline.
    BaselinePart,
    /// The lane's oldest buffered delta.
    Delta,
    /// Nothing: the baseline is done and the tail is empty.
    Drained,
    /// A part is already in flight for this lane.
    InFlight,
}

impl SyncV2Session {
    /// Put a lane in the ready ring, if it is not already in it.
    pub(in crate::sync_ws) fn mark_terminal_lane_ready(&mut self, session_id: &str) {
        let Some(lane) = self.terminal_sessions.get_mut(session_id) else {
            return;
        };
        if lane.ready {
            return;
        }
        lane.ready = true;
        if !self
            .terminal_ready_sessions
            .iter()
            .any(|ready| ready == session_id)
        {
            self.terminal_ready_sessions.push(session_id.to_owned());
        }
    }

    /// Take one lane out of the ready ring, in ring order or by name.
    ///
    /// Taking CLEARS the lane's `ready` flag, which is what makes a later
    /// `mark` put it back at the END of the ring: a lane that has just been
    /// pumped and failed must not be retried ahead of lanes that have been
    /// waiting longer.
    pub(in crate::sync_ws) fn take_ready_lane(&mut self, preferred: Option<&str>) -> Option<String> {
        let session_id = match preferred {
            Some(preferred) => {
                if !self
                    .terminal_lane(preferred)
                    .is_some_and(|lane| lane.ready)
                {
                    return None;
                }
                preferred.to_owned()
            }
            None => self.terminal_ready_sessions.first()?.clone(),
        };
        self.drop_from_ready_ring(&session_id);
        let lane = self.terminal_sessions.get_mut(&session_id)?;
        lane.ready = false;
        Some(session_id)
    }

    /// Remove a session from the ready ring without pumping it.
    pub(in crate::sync_ws) fn drop_from_ready_ring(&mut self, session_id: &str) {
        self.terminal_ready_sessions
            .retain(|ready| ready != session_id);
    }

    /// Pump the oldest ready lane. This is the egress retry, and it pumps at
    /// most ONE lane per call.
    pub(in crate::sync_ws) fn pump_next_ready_lane(
        &mut self,
        now_ms: u64,
        hub: &mut dyn TerminalSnapshotHub,
    ) {
        if let Some(session_id) = self.take_ready_lane(None) {
            self.pump_lane(&session_id, now_ms, hub);
        }
    }

    /// Pump one named lane, and only if it is ready.
    ///
    /// The terminal ingress calls this after marking a lane ready, which is why
    /// a delta arriving while the lane's baseline is still streaming is buffered
    /// rather than sent.
    pub(in crate::sync_ws) fn pump_terminal_lane(
        &mut self,
        session_id: &str,
        now_ms: u64,
        hub: &mut dyn TerminalSnapshotHub,
    ) {
        if let Some(taken) = self.take_ready_lane(Some(session_id)) {
            self.pump_lane(&taken, now_ms, hub);
        }
    }

    fn pump_lane(&mut self, session_id: &str, now_ms: u64, hub: &mut dyn TerminalSnapshotHub) {
        if !self.terminal_sessions.contains_key(session_id) {
            return;
        }
        if self
            .terminal_lane(session_id)
            .is_some_and(|lane| lane.state_queued)
        {
            return;
        }
        let has_states = self
            .terminal_lane(session_id)
            .is_some_and(|lane| !lane.pending_states.is_empty());
        if has_states {
            if !self.pump_state(session_id, now_ms, hub) {
                self.mark_terminal_lane_ready(session_id);
            }
            return;
        }
        match self.pump_cursor(session_id, now_ms, hub) {
            CursorPump::Queued => {}
            CursorPump::Blocked => self.mark_terminal_lane_ready(session_id),
            CursorPump::Empty => {
                let rebaseline_pending = self
                    .terminal_lane(session_id)
                    .is_some_and(|lane| lane.rebaseline_pending);
                if rebaseline_pending {
                    let socket_id = self.socket_id.clone();
                    if !hub.request_rebaseline(&socket_id, session_id) {
                        self.mark_terminal_lane_ready(session_id);
                    }
                }
            }
        }
    }

    /// Queue this session's oldest view-state. One in flight at a time, so a
    /// state can never overtake the state before it.
    fn pump_state(
        &mut self,
        session_id: &str,
        now_ms: u64,
        hub: &mut dyn TerminalSnapshotHub,
    ) -> bool {
        let Some((stream_id, state)) = self.take_pending_state(session_id) else {
            return false;
        };
        let meta = SyncFrameMeta {
            domain: Some(SyncDomain::Terminal),
            lane: FeedLane::Cell,
            session_id: Some(session_id.to_owned()),
            terminal_stream_id: Some(stream_id),
            terminal_state: true,
            ..SyncFrameMeta::default()
        };
        // The frame moves into the queue on success and back to the head of the
        // lane's FIFO on failure, so a state that did not fit is retried rather
        // than lost.
        match self.try_enqueue_lane_frame(state, &meta, now_ms, hub) {
            Ok(()) => {
                if let Some(lane) = self.terminal_sessions.get_mut(session_id) {
                    lane.state_queued = true;
                }
                true
            }
            Err(returned) => {
                self.restore_pending_state(session_id, *returned);
                false
            }
        }
    }

    /// Queue this session's next baseline part, or its oldest buffered delta.
    fn pump_cursor(
        &mut self,
        session_id: &str,
        now_ms: u64,
        hub: &mut dyn TerminalSnapshotHub,
    ) -> CursorPump {
        match self.plan_cursor(session_id) {
            None | Some(CursorPlan::Drained) => {
                self.release_drained_cursor(session_id);
                CursorPump::Empty
            }
            Some(CursorPlan::InFlight) => CursorPump::Blocked,
            Some(CursorPlan::BaselinePart) => self.pump_baseline_part(session_id, now_ms, hub),
            Some(CursorPlan::Delta) => self.pump_delta(session_id, now_ms, hub),
        }
    }

    fn plan_cursor(&self, session_id: &str) -> Option<CursorPlan> {
        let lane = self.terminal_lane(session_id)?;
        let cursor = lane.cursor.as_ref()?;
        if lane.stream_id != cursor.stream_id {
            return None;
        }
        if cursor.queued {
            return Some(CursorPlan::InFlight);
        }
        if cursor.index < cursor.part_count() {
            return Some(CursorPlan::BaselinePart);
        }
        if cursor.delta_tail.is_empty() {
            return Some(CursorPlan::Drained);
        }
        Some(CursorPlan::Delta)
    }

    /// Materialise and queue the lane's next baseline part.
    fn pump_baseline_part(
        &mut self,
        session_id: &str,
        now_ms: u64,
        hub: &mut dyn TerminalSnapshotHub,
    ) -> CursorPump {
        let attach_priority = self
            .terminal_lane(session_id)
            .is_some_and(|lane| lane.attach_priority_pending);
        // A part an earlier turn materialised but could not queue is reused
        // rather than re-materialised, so a blocked lane does not pay for the
        // same part on every retry.
        let materialized = match self.take_cursor_materialization(session_id) {
            Some(already_materialized) => Some(already_materialized),
            None => self.materialize_cursor_part(session_id, now_ms),
        };
        let Some(materialized) = materialized else {
            return CursorPump::Blocked;
        };
        let (stream_id, index) = self
            .terminal_lane(session_id)
            .and_then(|lane| lane.cursor.as_ref())
            .map_or_else(|| (String::new(), 0), |cursor| (cursor.stream_id.clone(), cursor.index));
        let attach_snapshot = attach_priority && materialized.frame.is_snapshot();
        let meta = SyncFrameMeta {
            domain: Some(SyncDomain::Terminal),
            lane: FeedLane::Cell,
            session_id: Some(session_id.to_owned()),
            terminal_stream_id: Some(stream_id),
            terminal_cursor_index: Some(index),
            attach_snapshot,
            ..SyncFrameMeta::default()
        };
        match self.try_enqueue_lane_frame(materialized, &meta, now_ms, hub) {
            Ok(()) => {
                self.mark_cursor_queued(session_id);
                CursorPump::Queued
            }
            Err(returned) => {
                self.restore_cursor_materialization(session_id, *returned);
                CursorPump::Blocked
            }
        }
    }

    /// Queue the lane's oldest buffered delta.
    fn pump_delta(
        &mut self,
        session_id: &str,
        now_ms: u64,
        hub: &mut dyn TerminalSnapshotHub,
    ) -> CursorPump {
        let cursor_index = self
            .terminal_lane(session_id)
            .and_then(|lane| lane.cursor.as_ref())
            .map_or(0, |cursor| cursor.part_count());
        let Some(delta) = self.take_delta_tail_head(session_id) else {
            return CursorPump::Blocked;
        };
        let meta = SyncFrameMeta {
            domain: Some(SyncDomain::Terminal),
            lane: FeedLane::Cell,
            session_id: Some(session_id.to_owned()),
            terminal_cursor_index: Some(cursor_index),
            ..SyncFrameMeta::default()
        };
        match self.try_enqueue_lane_frame(delta, &meta, now_ms, hub) {
            Ok(()) => {
                self.mark_cursor_queued(session_id);
                CursorPump::Queued
            }
            // A delta the queue would not take is dropped, not re-buffered: it is
            // the newest output for the session, and a stale delta in front of a
            // later full is worse than a gap the next frame closes.
            Err(_) => CursorPump::Blocked,
        }
    }
}
