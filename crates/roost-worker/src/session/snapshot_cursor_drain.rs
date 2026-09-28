//! Draining one parked full to the sinks that owe it: the part-by-part send
//! loop, the cursor position read before each send, the advance that only
//! applies to the cursor it read, and the stream-wide completion answer.
//! `session::emit` drives it through `CellEmitter::drain_snapshot`, and
//! `session::snapshot_cursor` parks the cursor it walks. Depends on
//! `super::snapshot_cursor` for the part type and `super::cell_sink` for the
//! answers.

use std::sync::Arc;

use roost_protocol::wire::brand::ChannelId;

use super::cell_sink::{CellSinkResult, FrameTimings};
use super::emit::CellEmitter;
use super::snapshot_cursor::{ParkedPart, SnapshotDrain};

impl CellEmitter {
    /// Send parked parts to every sink that owes them, then answer for the
    /// stream.
    ///
    /// EACH SINK DRAINS ALONE, as v2's `installStreamBaseline` drains every
    /// sink's cursor in turn (`session-snapshot-cursor.ts:100,167`): one that
    /// refuses a part keeps its cursor parked, and its siblings still receive
    /// theirs. Stopping the sweep at the first refusal would leave a healthy
    /// sink without the baseline it could have taken, which blocks every delta.
    pub fn drain_snapshot(&mut self, channel_id: ChannelId) -> SnapshotDrain {
        let parked: Vec<String> = self
            .streams
            .get(&channel_id)
            .map(|stream| {
                stream
                    .deliveries
                    .iter()
                    .filter(|(_, delivery)| delivery.cursor.is_some())
                    .map(|(sink_id, _)| sink_id.clone())
                    .collect()
            })
            .unwrap_or_default();
        if parked.is_empty() {
            return SnapshotDrain::NoCursor;
        }
        for sink_id in parked {
            self.drain_sink_cursor(channel_id, &sink_id);
        }
        self.complete_stream_baseline(channel_id)
    }

    /// One sink's parked parts, until its last or its first refusal.
    fn drain_sink_cursor(&mut self, channel_id: ChannelId, sink_id: &str) {
        loop {
            if !self.sinks.contains(sink_id) {
                // Dropped for an overflow, possibly inside its own send just
                // now: a sink that is gone owes nothing and keeps no record, as
                // v2's `dropCellSink` forgets it (`session-cell-sinks.ts:250-253`).
                self.forget_sink_records(sink_id);
                return;
            }
            if !self.sinks.is_active(sink_id) {
                return;
            }
            let Some((parts, index, snapshot_id, timings)) =
                self.cursor_position(channel_id, sink_id)
            else {
                return;
            };
            let Some(part) = parts.get(index) else {
                return;
            };
            let answer = self
                .sinks
                .send_part_to_sink(channel_id, sink_id, part, timings);
            if answer == CellSinkResult::Sent {
                if !self.advance_cursor(channel_id, sink_id, &snapshot_id) {
                    return;
                }
            } else if self.sinks.contains(sink_id) {
                tracing::debug!(%channel_id, sink_id, part = index, "a sink refused a snapshot part; its cursor stays parked");
                return;
            }
        }
    }

    /// cannot borrow the delivery record it advances.
    fn cursor_position(
        &self,
        channel_id: ChannelId,
        sink_id: &str,
    ) -> Option<(Arc<Vec<ParkedPart>>, usize, String, FrameTimings)> {
        let cursor = self
            .streams
            .get(&channel_id)?
            .deliveries
            .get(sink_id)?
            .cursor
            .as_ref()?;
        Some((
            Arc::clone(&cursor.parts),
            cursor.next_part,
            cursor.snapshot_id.clone(),
            cursor.timings,
        ))
    }

    /// the cursor is no longer the one that was read, which a sink that retired
    /// the stream can cause from inside its own send: a cursor that no longer
    /// owns the channel is never advanced, even though the send succeeded.
    fn advance_cursor(&mut self, channel_id: ChannelId, sink_id: &str, snapshot_id: &str) -> bool {
        let Some(stream) = self.streams.get_mut(&channel_id) else {
            return false;
        };
        let Some(delivery) = stream.deliveries.get_mut(sink_id) else {
            return false;
        };
        let Some(cursor) = delivery.cursor.as_mut() else {
            return false;
        };
        if cursor.snapshot_id != snapshot_id {
            return false;
        }
        cursor.next_part += 1;
        if cursor.next_part < cursor.parts.len() {
            return true;
        }
        delivery.cursor = None;
        delivery.baseline_ready = true;
        tracing::debug!(%channel_id, sink_id, "a sink received the last part of its full and holds a baseline");
        true
    }

    fn complete_stream_baseline(&mut self, channel_id: ChannelId) -> SnapshotDrain {
        let aggregate = self.delivery_aggregate(channel_id);
        if aggregate.snapshot_pending || !aggregate.baseline_ready {
            return SnapshotDrain::Blocked;
        }
        let work_owed = aggregate.baseline_dirty || self.is_dirty(channel_id);
        if let Some(stream) = self.streams.get_mut(&channel_id) {
            stream.pending_repair = false;
            stream.pending_sync_snapshot = false;
            for delivery in stream.deliveries.values_mut() {
                delivery.baseline_dirty = false;
            }
        }
        SnapshotDrain::BaselineComplete { work_owed }
    }
}
