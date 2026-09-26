//! What a delivered terminal frame did to its session's lane: advancing the
//! baseline cursor, retiring a view-state, and the item accessors the pump needs
//! to take a lane's next piece of material out.
//!
//! Owned by the Sync session; `ready_ring.rs` holds the ring and the pumps. The
//! two halves are separated because they answer opposite questions -- "what
//! should go out next" and "what does a frame that just went out mean" -- and a
//! delivery that advances the wrong cursor is the failure neither question can
//! see.
//!
//! A DELIVERY THAT DOES NOT MATCH THE LANE IS IGNORED, NOT APPLIED. A frame
//! from a replaced stream, or a repeat of a part the cursor has already advanced
//! past, would otherwise rewind the baseline and re-send rows the client already
//! has.

use crate::sync_ws::frame_meta::SyncFrameMeta;
use crate::sync_ws::retained_frame::{RetainedFrame, SharedCellFrame};
use crate::sync_ws::session::SyncV2Session;

impl SyncV2Session {
    /// Advance the lane whose frame the socket just accepted, then put the lane
    /// back in the ready ring.
    pub(in crate::sync_ws) fn on_terminal_frame_delivered(&mut self, meta: &SyncFrameMeta) {
        let Some(session_id) = meta.session_id.as_deref() else {
            return;
        };
        if !self.terminal_sessions.contains_key(session_id) {
            return;
        }
        if meta.terminal_state {
            let state_queued = self
                .terminal_lane(session_id)
                .is_some_and(|lane| lane.state_queued);
            if !state_queued {
                return;
            }
            if let Some(lane) = self.terminal_sessions.get_mut(session_id) {
                lane.state_queued = false;
                if !lane.pending_states.is_empty() {
                    lane.pending_states.remove(0);
                }
            }
        } else {
            self.advance_cursor(session_id, meta);
        }
        self.mark_terminal_lane_ready(session_id);
    }

    fn advance_cursor(&mut self, session_id: &str, meta: &SyncFrameMeta) {
        let Some(sent_index) = meta.terminal_cursor_index else {
            return;
        };
        let Some(lane) = self.terminal_sessions.get_mut(session_id) else {
            return;
        };
        let Some(cursor) = lane.cursor.as_mut() else {
            return;
        };
        if cursor.stream_id != meta.terminal_stream_id.as_deref().unwrap_or_default() {
            return;
        }
        let part_count = cursor.part_count();
        let expected = if cursor.index < part_count {
            cursor.index
        } else {
            part_count
        };
        if sent_index != expected {
            return;
        }
        cursor.queued = false;
        if meta.attach_snapshot {
            lane.attach_priority_pending = false;
        }
        if cursor.index < part_count {
            cursor.index += 1;
            if cursor.index == part_count {
                // The last part is away, so this socket stops pinning the
                // canonical plan.
                cursor.source = None;
            }
            return;
        }
        if !cursor.delta_tail.is_empty() {
            let released = cursor.delta_tail[0].estimated_bytes();
            cursor.delta_bytes = cursor.delta_bytes.saturating_sub(released);
            cursor.delta_tail.remove(0);
        }
    }

    pub(in crate::sync_ws) fn mark_cursor_queued(&mut self, session_id: &str) {
        if let Some(cursor) = self
            .terminal_sessions
            .get_mut(session_id)
            .and_then(|lane| lane.cursor.as_mut())
        {
            cursor.queued = true;
        }
    }

    /// Drop a cursor whose baseline and delta tail are both empty.
    pub(in crate::sync_ws) fn release_drained_cursor(&mut self, session_id: &str) {
        if let Some(mut lane) = self.terminal_sessions.remove(session_id) {
            lane.release_cursor(self);
            self.terminal_sessions.insert(session_id.to_owned(), lane);
        }
    }

    /// Materialise and charge the lane's next baseline part.
    ///
    /// A source that cannot produce the part is a caller bug rather than a
    /// client-visible fault, and the lane stays BLOCKED so it is retried when a
    /// real source is installed, rather than being advanced past a hole.
    pub(in crate::sync_ws) fn materialize_cursor_part(
        &mut self,
        session_id: &str,
        now_ms: u64,
    ) -> Option<RetainedFrame> {
        let cell: SharedCellFrame = {
            let cursor = self.terminal_lane(session_id)?.cursor.as_ref()?;
            let source = cursor.source.as_ref()?;
            source.materialize(cursor.index)?
        };
        self.retain_cursor_part(session_id, cell, now_ms)
    }

    pub(in crate::sync_ws) fn take_cursor_materialization(
        &mut self,
        session_id: &str,
    ) -> Option<RetainedFrame> {
        self.terminal_sessions
            .get_mut(session_id)?
            .cursor
            .as_mut()?
            .materialized
            .take()
    }

    pub(in crate::sync_ws) fn restore_cursor_materialization(
        &mut self,
        session_id: &str,
        part: RetainedFrame,
    ) {
        if let Some(cursor) = self
            .terminal_sessions
            .get_mut(session_id)
            .and_then(|lane| lane.cursor.as_mut())
        {
            cursor.materialized = Some(part);
        }
    }

    pub(in crate::sync_ws) fn take_pending_state(
        &mut self,
        session_id: &str,
    ) -> Option<(String, RetainedFrame)> {
        let lane = self.terminal_sessions.get_mut(session_id)?;
        if lane.pending_states.is_empty() {
            return None;
        }
        let stream_id = lane.stream_id.clone();
        let state = lane.pending_states.remove(0);
        Some((stream_id, state))
    }

    pub(in crate::sync_ws) fn restore_pending_state(
        &mut self,
        session_id: &str,
        state: RetainedFrame,
    ) {
        if let Some(lane) = self.terminal_sessions.get_mut(session_id) {
            lane.pending_states.insert(0, state);
        }
    }

    pub(in crate::sync_ws) fn take_delta_tail_head(
        &mut self,
        session_id: &str,
    ) -> Option<RetainedFrame> {
        let cursor = self
            .terminal_sessions
            .get_mut(session_id)?
            .cursor
            .as_mut()?;
        if cursor.delta_tail.is_empty() {
            return None;
        }
        Some(cursor.delta_tail.remove(0))
    }
}
