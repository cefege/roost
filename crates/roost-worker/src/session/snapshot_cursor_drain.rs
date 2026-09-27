//! Draining one parked full to the sinks that owe it: the part-by-part send
//! loop, the cursor position read before each send, the advance that only
//! applies to the cursor it read, and the stream-wide completion answer.
//! `session::emit` drives it through `CellEmitter::drain_snapshot`, and
//! `session::snapshot_cursor` parks the cursor it walks. Depends on
//! `roost_protocol` for the part type and `super::cell_sink` for the answers.

use std::sync::Arc;

use roost_protocol::cell::frame_chunks::CellGridSnapshotPart;
use roost_protocol::wire::brand::ChannelId;

use super::cell_sink::{CellSinkResult, FrameTimings};
use super::emit::CellEmitter;
use super::snapshot_cursor::SnapshotDrain;

impl CellEmitter {
    /// Send parked parts until every sink has its last, or one refuses.
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
            loop {
                if !self.sinks.is_active(&sink_id) {
                    return SnapshotDrain::Blocked;
                }
                let Some((parts, index, snapshot_id, timings)) =
                    self.cursor_position(channel_id, &sink_id)
                else {
                    break;
                };
                let Some(part) = parts.get(index) else {
                    break;
                };
                let answer = self
                    .sinks
                    .send_part_to_sink(channel_id, &sink_id, part, timings);
                if answer != CellSinkResult::Sent {
                    return SnapshotDrain::Blocked;
                }
                if !self.sinks.contains(&sink_id) {
                    self.forget_sink_records(&sink_id);
                    return SnapshotDrain::Blocked;
                }
                if !self.advance_cursor(channel_id, &sink_id, &snapshot_id) {
                    break;
                }
            }
        }
        self.complete_stream_baseline(channel_id)
    }

    /// cannot borrow the delivery record it advances.
    fn cursor_position(
        &self,
        channel_id: ChannelId,
        sink_id: &str,
    ) -> Option<(Arc<Vec<CellGridSnapshotPart>>, usize, String, FrameTimings)> {
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
