//! The parked full: one immutable snapshot, per-sink cursors over it, and the
//! sender lifecycle that invalidates them. `session::emit` installs a baseline
//! here; [`super::snapshot_cursor_drain`] walks the parked parts out of it.
//! `door` and the coordinator link register through these methods. Depends on
//! `roost_protocol` for the chunk plan and `super::cell_sink` for the delivery
//! records.
//!
//! WHY A CURSOR. A full is chunked when it does not fit one part, and a part a
//! sink refuses is retried while its siblings have moved on. So the full is
//! parked, immutable and shared as one `Arc`; only each sink's POSITION in it is
//! private. A sink that has just joined is given a STREAM-WIDE full, because a
//! second frame builder over one core would steal the rows the first claimed.
//!
//! COMPLETION IS STREAM-WIDE: a repair or a gate release that fired while one
//! sink still owed a baseline would let a delta reach a sink that cannot
//! reproduce the screen.

use std::sync::Arc;

use roost_protocol::cell::frame_chunk_validation::assert_cell_grid_snapshot;
use roost_protocol::cell::frame_chunks::{
    CellGridSnapshotPart, chunk_cell_grid_frame, encoded_cell_grid_frame_size,
};
use roost_protocol::cell::{CELL_GRID_PART_MAX_BYTES, CellGridFrame, cell_frame_to_proto};
use roost_protocol::wire::brand::ChannelId;
use roost_term::{CellEmitState, scrollback_origin};
use tracing::warn;

use super::cell_sink::{
    CellSink, FrameTimings, StreamDeliveryAggregate, aggregate_stream_delivery,
};
use super::emit::CellEmitter;
use super::ids::{MintError, mint_uuid};

/// One part of a parked full, in the shape the sink method that takes it wants.
///
/// A full that fits one part is handed WHOLE to [`CellSink::send_frame`], as
/// v2's `sendCellPartToSink` hands a `frame` part to `sendFrame`
/// (`apps/worker/src/session/session-cell-sinks.ts:220-222`); only a chunk of a
/// full too large for one part reaches [`CellSink::send_snapshot_part`].
#[derive(Debug, Clone, PartialEq)]
pub enum ParkedPart {
    Whole(CellGridFrame),
    /// Always the protocol's `Chunk` arm. It is kept in the enum the sink takes
    /// so a send borrows it instead of rebuilding it for every sink.
    Chunk(CellGridSnapshotPart),
}

/// One parked immutable full, drained part by part for one sink alone.
///
/// The parts are an `Arc`: validation and chunking run ONCE per full and every
/// sink walks the same immutable plan, so only each sink's position in it is
/// private. A deep copy per sink would be the whole frame, per browser.
#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotCursor {
    pub stream_id: String,
    pub snapshot_id: String,
    /// The full's sequence number, for a receiver that logs which baseline it
    /// is installing.
    pub seq: u64,
    pub parts: Arc<Vec<ParkedPart>>,
    pub next_part: usize,
    /// The two clocks the FULL was measured with. Every part of it carries the
    /// same pair, and they are kept with the cursor rather than recomputed: a
    /// part sent minutes later must not claim it was prepared minutes later.
    pub timings: FrameTimings,
}

/// How one drain of a parked full ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotDrain {
    /// This channel has no parked cursor.
    NoCursor,
    /// Every active sink holds a baseline. `work_owed` says whether dirty work
    /// arrived while it was blocked, which is the caller's cue to emit.
    BaselineComplete { work_owed: bool },
    /// A sink refused a part, or the cursor is incomplete. It stays parked.
    Blocked,
}

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("the built frame is not a legal cell grid frame: {0}")]
    Contract(#[from] roost_protocol::cell::CellGridChunkError),
    #[error("the built frame could not be turned into its wire form: {0}")]
    Wire(#[from] roost_protocol::ProtocolError),
    #[error("a snapshot identity could not be minted: {0}")]
    Mint(#[from] MintError),
}

impl CellEmitter {
    /// Register a sink and force one full for every channel it watches: a
    /// receiver that has just gained delivery holds no baseline.
    pub fn register_sink(&mut self, sink: Arc<dyn CellSink>) {
        let sink_id = sink.id().to_owned();
        self.sinks.register(sink);
        self.force_baseline_for_sink(&sink_id);
    }

    /// Remove a sink and every delivery record it held.
    pub fn unregister_sink(&mut self, sink_id: &str) {
        if self.sinks.unregister(sink_id).is_some() {
            self.forget_sink_records(sink_id);
        }
    }

    /// The transport is known down. Cursors are abandoned rather than resumed:
    /// a sink that was gone missed the parts, and handing it the last part of a
    /// snapshot it never started would declare a baseline it lacks.
    pub fn suspend_sink(&mut self, sink_id: &str) {
        if !self.sinks.suspend(sink_id) {
            return;
        }
        for stream in self.streams.values_mut() {
            if let Some(delivery) = stream.deliveries.get_mut(sink_id) {
                delivery.cursor = None;
                delivery.baseline_ready = false;
                delivery.baseline_dirty = false;
            }
        }
    }

    /// The transport is usable again. Only a sink this call RE-activated owes a
    /// full.
    pub fn resume_sink(&mut self, sink_id: &str) {
        if self.sinks.resume(sink_id) {
            self.force_baseline_for_sink(sink_id);
        }
    }

    /// One channel's delivery state, answered over the ACTIVE sinks.
    pub fn delivery_aggregate(&self, channel_id: ChannelId) -> StreamDeliveryAggregate {
        aggregate_stream_delivery(
            &self.sinks,
            self.streams.get(&channel_id).map(|s| &s.deliveries),
        )
    }

    /// Park ONE built full as a cursor for every active sink, then drain them.
    /// Validation and chunking run once.
    pub fn install_baseline(
        &mut self,
        channel_id: ChannelId,
        frame: CellGridFrame,
        timings: FrameTimings,
    ) -> Result<(), SnapshotError> {
        let snapshot_id = mint_uuid()?;
        // The canonical size IS a protobuf byte count, so the frame is
        // materialised once here to be validated and measured. A full that fits
        // one part is parked as the value model and the sink materialises it
        // again for the wire: this crate may not depend on `roost-proto`, and a
        // second value model to avoid the copy would be a worse trade.
        let wire = cell_frame_to_proto(&frame, "")?;
        assert_cell_grid_snapshot(&wire)?;
        let seq = frame.seq;
        let parts = if encoded_cell_grid_frame_size(&wire) <= CELL_GRID_PART_MAX_BYTES {
            vec![ParkedPart::Whole(frame)]
        } else {
            chunk_cell_grid_frame(&wire, &snapshot_id)?
                .into_iter()
                .map(|chunk| ParkedPart::Chunk(CellGridSnapshotPart::Chunk(chunk)))
                .collect()
        };
        let parts = Arc::new(parts);
        let Some(stream) = self.streams.get_mut(&channel_id) else {
            return Ok(());
        };
        let stream_id = stream.stream_id.clone();
        let sink_ids: Vec<String> = self
            .sinks
            .active_sinks()
            .iter()
            .map(|s| s.id().to_owned())
            .collect();
        // Every cursor is installed BEFORE the first part ships, so an early
        // completion cannot report the stream baselined while a sibling still
        // owes a full.
        for sink_id in sink_ids {
            let delivery = stream.deliveries.entry(sink_id).or_default();
            delivery.cursor = Some(SnapshotCursor {
                stream_id: stream_id.clone(),
                snapshot_id: snapshot_id.clone(),
                seq,
                parts: Arc::clone(&parts),
                next_part: 0,
                timings,
            });
            delivery.baseline_ready = false;
        }
        self.drain_snapshot(channel_id);
        Ok(())
    }

    /// Channels holding at least one parked cursor, for a resume sweep.
    pub fn channels_with_parked_snapshots(&self) -> Vec<ChannelId> {
        let mut channels: Vec<ChannelId> = self
            .streams
            .iter()
            .filter(|(_, stream)| {
                stream
                    .deliveries
                    .values()
                    .any(|delivery| delivery.cursor.is_some())
            })
            .map(|(channel_id, _)| *channel_id)
            .collect();
        channels.sort_unstable();
        channels
    }

    /// trapped resize, or a snapshot request. Every sink's baseline goes at once:
    /// a frame they could not all reproduce is a frame none may build on.
    pub fn retire_stream_delivery(&mut self, channel_id: ChannelId) {
        if let Some(stream) = self.streams.get_mut(&channel_id) {
            for delivery in stream.deliveries.values_mut() {
                delivery.cursor = None;
                delivery.baseline_ready = false;
            }
        }
    }

    /// stream-wide: emission builds ONE frame per tick over one core.
    pub fn mark_stream_delivery_dirty(&mut self, channel_id: ChannelId) {
        if let Some(stream) = self.streams.get_mut(&channel_id) {
            for delivery in stream.deliveries.values_mut() {
                if delivery.cursor.is_some() || !delivery.baseline_ready {
                    delivery.baseline_dirty = true;
                }
            }
        }
    }

    pub fn clear_stream_delivery_dirty(&mut self, channel_id: ChannelId) {
        if let Some(stream) = self.streams.get_mut(&channel_id) {
            for delivery in stream.deliveries.values_mut() {
                delivery.baseline_dirty = false;
            }
        }
    }

    /// Latch a repair full: a receiver can no longer reproduce the screen.
    pub fn note_repair(&mut self, channel_id: ChannelId) {
        if let Some(stream) = self.streams.get_mut(&channel_id) {
            stream.pending_repair = true;
        }
        tracing::warn!(%channel_id, "a cell baseline is invalid and a repair full is owed");
    }

    /// Take the repair latch and the synchronized-output debt. `true` means one
    /// full is owed for this channel.
    pub fn take_pending_repair(&mut self, channel_id: ChannelId) -> bool {
        let Some(stream) = self.streams.get_mut(&channel_id) else {
            return false;
        };
        let owed = stream.pending_repair || stream.pending_sync_snapshot;
        stream.pending_repair = false;
        stream.pending_sync_snapshot = false;
        owed
    }

    /// just closed with a full still owed.
    pub fn set_pending_sync_snapshot(&mut self, channel_id: ChannelId, owed: bool) {
        if let Some(stream) = self.streams.get_mut(&channel_id) {
            stream.pending_sync_snapshot = owed;
        }
    }

    /// One owed full per watched channel for a sink that just gained delivery,
    /// as v2's `forceBaselineForSink` (`session-cell-sinks.ts:258-269`).
    ///
    /// Every parked cursor is dropped, not waited on: the new full supersedes
    /// it, and a forced build withheld behind one leaves every sink owing a
    /// baseline with nothing left to force it. The emitter holds no record, so
    /// the debt is recorded here and the next emit on the channel builds it.
    fn force_baseline_for_sink(&mut self, sink_id: &str) {
        let watching: Vec<ChannelId> = self
            .streams
            .iter()
            .filter(|(_, stream)| stream.enabled && stream.core_valid)
            .map(|(channel_id, _)| *channel_id)
            .collect();
        for channel_id in watching {
            if let Some(stream) = self.streams.get_mut(&channel_id) {
                for parked in stream.deliveries.values_mut() {
                    parked.cursor = None;
                }
                let delivery = stream.deliveries.entry(sink_id.to_owned()).or_default();
                delivery.baseline_ready = false;
            }
            self.mark_stream_delivery_dirty(channel_id);
            self.note_dirty(channel_id);
            tracing::info!(%channel_id, sink_id, "a full frame is owed to a sink that just gained delivery");
        }
    }

    pub(crate) fn forget_sink_records(&mut self, sink_id: &str) {
        for stream in self.streams.values_mut() {
            stream.deliveries.remove(sink_id);
        }
    }
}

/// Preserve a same-grid renewal epoch while the new full's absolute history
/// range still overlaps what the last frame addressed. A renewal that keeps the
/// grid keeps the epoch: bumping it would invalidate every row index a client
/// holds for a grid that did not change.
pub fn prepare_cell_renewal_epoch(
    core: &dyn roost_term::TerminalCore,
    emit: &mut CellEmitState,
) -> bool {
    let Ok(sb_dropped) = scrollback_origin(core, emit.scrollback_origin) else {
        return false;
    };
    let scrollback_total = sb_dropped + core.scrollback_count() as u64;
    let compatible = core.cols() == emit.cols
        && core.rows() == emit.rows
        && core.using_alt_screen() == emit.alt
        && sb_dropped <= emit.last_scrollback_total
        && scrollback_total >= emit.last_scrollback_total;
    if !compatible {
        emit.grid_epoch_revision += 1;
        warn!(
            cols = core.cols(),
            rows = core.rows(),
            alt = core.using_alt_screen(),
            "a renewal frame advances the grid epoch because the grid is not the one clients hold"
        );
    }
    compatible
}
