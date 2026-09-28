//! Cell-sink lifecycle with v2's side effects: `apps/worker/src/session/
//! session-cell-sinks.ts` (`registerCellSink`, `unregisterCellSink`,
//! `suspendCellSink`, `resumeCellSink`, `forceBaselineForSink`) and
//! `session-emit.ts` `resumeTerminalSnapshots`. The cadence and the link
//! lifecycle call these; the registry and cursor halves live in
//! `super::{cell_sink, snapshot_cursor}`.

use std::sync::Arc;

use roost_protocol::wire::brand::ChannelId;

use super::cell_scheduler::CellEmissionSchedule;
use super::cell_sink::CellSink;
use super::emit::CellEmitter;
use super::snapshot_cursor::SnapshotDrain;

impl CellEmitter {
    /// v2 `registerCellSink`: a same-id registration is dropped first, then one
    /// forced full is owed per watched channel.
    pub fn register_cell_sink(&mut self, sink: Arc<dyn CellSink>) {
        let sink_id = sink.id().to_owned();
        if self.sinks.contains(&sink_id) {
            self.unregister_sink(&sink_id);
        }
        self.register_sink(sink);
        self.request_baselines_for_watched_streams();
        tracing::info!(sink_id, "cell_sink_registered");
    }

    /// v2 `unregisterCellSink`: never calls the sink's `on_overflow`.
    pub fn unregister_cell_sink(&mut self, sink_id: &str) {
        if !self.sinks.contains(sink_id) {
            return;
        }
        self.unregister_sink(sink_id);
        tracing::info!(sink_id, "cell_sink_unregistered");
    }

    /// v2 `suspendCellSink`: transport known down. No repair is latched and no
    /// full is forced, so a dead coordinator cannot restart anyone's baseline.
    pub fn suspend_cell_sink(&mut self, sink_id: &str) {
        self.suspend_sink(sink_id);
    }

    /// v2 `resumeCellSink`: a suspended sink owes a full on every watched
    /// channel; an already-active one only resumes its parked parts.
    pub fn resume_cell_sink(&mut self, sink_id: &str) {
        if !self.sinks.contains(sink_id) {
            return;
        }
        if self.sinks.is_active(sink_id) {
            self.resume_terminal_snapshots();
            return;
        }
        self.resume_sink(sink_id);
        self.request_baselines_for_watched_streams();
    }

    /// v2 `resumeTerminalSnapshots`: drain every parked cursor; a channel with
    /// none parked and a latched repair owes its forced full now.
    pub fn resume_terminal_snapshots(&mut self) {
        let mut channels: Vec<ChannelId> = self
            .streams
            .iter()
            .filter(|(_, stream)| stream.enabled && stream.core_valid)
            .map(|(channel_id, _)| *channel_id)
            .collect();
        channels.sort_unstable();
        for channel_id in channels {
            match self.drain_snapshot(channel_id) {
                SnapshotDrain::NoCursor => {
                    if self.take_repair_latch(channel_id) {
                        self.request_terminal_baseline(channel_id);
                    }
                }
                SnapshotDrain::BaselineComplete { work_owed } => {
                    self.note_stream_baseline_complete(channel_id);
                    if work_owed {
                        self.schedule_leading_emission(channel_id);
                    }
                }
                SnapshotDrain::Blocked => {}
            }
        }
    }

    /// v2 `completeStreamBaseline`'s attribution half: the baseline gate is over
    /// unless a synchronized-output hold still owns the channel.
    pub(crate) fn note_stream_baseline_complete(&mut self, channel_id: ChannelId) {
        if !self.sync_output_held(channel_id) {
            self.gates.remove(&channel_id);
        }
    }

    /// Queue a leading emit for a channel whose baseline just completed with
    /// dirty work behind it (v2 `completeStreamBaseline` → `_scheduleCellEmit`).
    fn schedule_leading_emission(&mut self, channel_id: ChannelId) {
        let Some(stream_id) = self
            .streams
            .get(&channel_id)
            .filter(|stream| stream.enabled && stream.core_valid)
            .map(|stream| stream.stream_id.clone())
        else {
            return;
        };
        self.schedules
            .entry(channel_id)
            .or_insert(CellEmissionSchedule {
                stream_id,
                cooldown_until: None,
            });
        self.wake_cadence();
    }

    /// v2 `forceBaselineForSink`'s build: every watched channel owes a forced
    /// full, which the cadence builds with the record in hand.
    fn request_baselines_for_watched_streams(&mut self) {
        let watched: Vec<ChannelId> = self
            .streams
            .iter()
            .filter(|(_, stream)| stream.enabled && stream.core_valid)
            .map(|(channel_id, _)| *channel_id)
            .collect();
        for channel_id in watched {
            self.request_terminal_baseline(channel_id);
        }
    }
}
