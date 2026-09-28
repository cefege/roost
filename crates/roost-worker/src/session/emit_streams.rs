//! The emitter's per-channel delivery registry: which channels have a stream,
//! whether the coordinator still wants their cells, and whether the core behind
//! one is intact. `session::emit` asks it on every ingest and every tick,
//! `session::lifecycle` drives it as a channel is adopted and forgotten, and
//! `runtime::channel_delivery` mints and traps generations for the stream
//! transaction. Ports the stream-state halves of
//! `apps/worker/src/session/session-terminal-control.ts` and
//! `session-resize-capture.ts`. Depends on `roost_protocol` and `super::types`.

use roost_protocol::wire::brand::ChannelId;

use super::emit::{CellEmitter, StreamOutput};
use super::types::SessionRecord;

impl CellEmitter {
    /// Adopt a channel's delivery stream under a coordinator-minted generation.
    ///
    /// The record's emit state is re-addressed HERE rather than at the first
    /// emit: a frame naming the placeholder the record was born with would be
    /// addressed to a generation the coordinator never minted. A full is owed
    /// immediately: no sink holds a baseline for a fresh stream, so the next
    /// emit builds one whether or not the caller forces it.
    pub fn install_stream(&mut self, record: &mut SessionRecord, stream_id: &str) {
        let channel_id = record.channel_id();
        record.cell_emit.stream_id = stream_id.to_owned();
        let stream = self.streams.entry(channel_id).or_default();
        stream.stream_id = stream_id.to_owned();
        stream.enabled = true;
        stream.core_valid = true;
        self.note_dirty(channel_id);
        tracing::info!(%channel_id, stream_id, "a terminal delivery stream was installed");
    }

    /// The coordinator's own claim on whether this channel produces cells.
    pub fn set_stream_enabled(&mut self, channel_id: ChannelId, enabled: bool) {
        if let Some(stream) = self.streams.get_mut(&channel_id) {
            stream.enabled = enabled;
        }
    }

    /// A resize that trapped the core clears this; every later PTY byte takes
    /// the recovery lane until a new core is adopted.
    pub fn set_core_valid(&mut self, channel_id: ChannelId, valid: bool) {
        if let Some(stream) = self.streams.get_mut(&channel_id) {
            stream.core_valid = valid;
        }
    }

    pub fn has_enabled_stream(&self, channel_id: ChannelId) -> bool {
        self.streams
            .get(&channel_id)
            .is_some_and(|stream| stream.enabled && stream.core_valid)
    }

    /// Forget the stream, the staged raw bytes, the dirty mark and any hold.
    ///
    /// ONE CALL RATHER THAN THREE AT EACH CALLER, because a replaced generation
    /// must leave nothing describing a grid that no longer exists, and the three
    /// things a stale entry would keep alive — the stream, the staged raw
    /// bytes, and the dirty mark and hold keyed on it — have no other owner.
    pub fn forget_channel(&mut self, channel_id: ChannelId) {
        self.streams.remove(&channel_id);
        self.raw.forget_channel(channel_id);
        self.cancel(channel_id);
    }

    /// Mint the coordinator's next generation over this channel (v2
    /// `applyTerminalStreamState`, `session-terminal-control.ts:266-338`).
    ///
    /// The old generation's queued work and every sink's baseline go at once,
    /// and the new one starts with NO delivery records: every sink owes a
    /// baseline before it can take a delta. Core validity CROSSES generations —
    /// a frozen core never parsed what arrived since its trap, so only a
    /// re-proof clears it (`docs/FAILURE-INDEX.md`, "A terminal never repaints
    /// again"). The emit state keeps its grid identity and history counters and
    /// restarts its sequence; only a real geometry change moves the epoch.
    pub fn mint_stream(
        &mut self,
        record: &mut SessionRecord,
        stream_id: &str,
        enabled: bool,
        geometry_changed: bool,
    ) {
        let channel_id = record.channel_id();
        self.cancel_cell_emission(channel_id);
        self.clear_dirty(channel_id);
        self.clear_stream_delivery_dirty(channel_id);
        self.retire_stream_delivery(channel_id);
        let stream = self
            .streams
            .entry(channel_id)
            .or_insert_with(|| StreamOutput {
                core_valid: true,
                ..StreamOutput::default()
            });
        stream.stream_id = stream_id.to_owned();
        stream.enabled = enabled;
        stream.deliveries.clear();
        let core_valid = stream.core_valid;
        record.cell_emit.stream_id = stream_id.to_owned();
        record.cell_emit.seq = 0;
        if geometry_changed {
            record.cell_emit.grid_epoch_revision += 1;
        }
        tracing::info!(
            %channel_id,
            stream_id,
            enabled,
            core_valid,
            geometry_changed,
            "a terminal stream generation was minted"
        );
    }

    /// Whether this channel's core may be parsed and emitted from; false when
    /// no generation has been minted over it.
    pub fn stream_core_valid(&self, channel_id: ChannelId) -> bool {
        self.streams
            .get(&channel_id)
            .is_some_and(|stream| stream.core_valid)
    }

    /// Whether a minted generation exists and its core was latched invalid:
    /// the one case v2 routes a chunk to the retain-only lane.
    pub fn stream_core_trapped(&self, channel_id: ChannelId) -> bool {
        self.streams
            .get(&channel_id)
            .is_some_and(|stream| !stream.core_valid)
    }

    /// Latch a core fail-closed (v2 `failCore`): no sink may build on a frame
    /// it produced, and nothing queued for it may run.
    pub fn trap_stream_core(&mut self, channel_id: ChannelId) {
        self.retire_stream_delivery(channel_id);
        self.set_core_valid(channel_id, false);
        self.cancel_cell_emission(channel_id);
        tracing::warn!(%channel_id, "a terminal core was latched fail-closed");
    }

    /// The core was resized at a proven boundary (v2 `resetEmissionEpoch`):
    /// every sink's baseline describes a grid that no longer exists.
    pub fn reset_stream_delivery(&mut self, channel_id: ChannelId) {
        self.retire_stream_delivery(channel_id);
        self.mark_stream_delivery_dirty(channel_id);
    }
}
