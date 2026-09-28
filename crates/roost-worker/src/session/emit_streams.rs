//! The emitter's per-channel delivery registry: which channels have a stream,
//! whether the coordinator still wants their cells, and whether the core behind
//! one is intact. `session::emit` asks it on every ingest and every tick, and
//! `session::lifecycle` drives it as a channel is adopted, trapped and
//! forgotten. Depends on `roost_protocol` for the channel id and `super::types`
//! for the record, and on nothing that depends on it back.

use roost_protocol::wire::brand::ChannelId;

use super::emit::CellEmitter;
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
}
