//! The cell-frame producer's state: v2 `apps/worker/src/session/session-emit.ts`
//! (the emitter and `_disposeOutputState`). PTY bytes enter through
//! `session::emit_ingest` from the keeper binding (`runtime::channel_delivery`);
//! ONE frame per tick leaves through `session::emit_frame`, fanned to every
//! registered sink. The cadence (`runtime::cell_cadence`) runs the timers; the
//! gates live in `super::{cell_gates, cell_scheduler, sync_output}`.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use roost_protocol::wire::brand::ChannelId;

use super::cell_gates::GateSuppression;
use super::cell_scheduler::CellEmissionSchedule;
use super::cell_sink::{CellDeltaFanout, CellSinkRegistry, StreamDelivery};
use super::query_reply::QueryReplyLane;
use super::raw_metadata::RawMetadataStage;
use super::sync_output::SyncOutputState;
use super::terminal_metadata::TerminalMetadataStage;

/// v2 `CELL_EMIT_COALESCE_MS`: a burst of chunks collapses into one trailing frame.
pub const CELL_EMIT_COALESCE_MS: i64 = 16;

/// One channel's delivery stream. The coordinator owns `enabled` and the
/// generation; the record owns the core.
#[derive(Debug, Default)]
pub(crate) struct StreamOutput {
    pub(crate) stream_id: String,
    pub(crate) enabled: bool,
    pub(crate) core_valid: bool,
    pub(crate) deliveries: HashMap<String, StreamDelivery>,
    /// v2 `pendingCellRepairs`: a receiver can no longer reproduce the screen.
    pub(crate) pending_repair: bool,
    /// v2 `pendingSyncCellSnapshots`: a full came due inside a synchronized frame.
    pub(crate) pending_sync_snapshot: bool,
}

/// What one ingest did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestOutcome {
    /// Retained, and the enabled stream took the chunk into its cadence.
    Accepted { end_seq: u64, input_echo: bool },
    /// Retained, but no enabled stream is watching.
    RetainedOnly { end_seq: u64 },
}

/// Why no frame was produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Withheld {
    /// No stream, the channel is disabled, or a resize invalidated the core.
    NoStream,
    /// No active sink. A suspended transport must not latch a repair.
    NoSink,
    /// The resize emission gate holds the channel.
    Gate,
    /// An active sink has no baseline and this emit was not forced.
    Baseline,
    /// An open synchronized-output frame withholds it.
    SyncOutput,
    /// A full is parked as a cursor for at least one sink.
    SnapshotPending,
    /// The emitter could not build a frame from this core.
    Unbuildable(String),
}

/// What one emit did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameOutcome {
    /// A full was built; `installed` is false when it could not become parts.
    Full {
        seq: u64,
        installed: bool,
    },
    /// A delta was built and fanned out.
    Delta {
        seq: u64,
        fanout: CellDeltaFanout,
    },
    Withheld(Withheld),
}

/// The producer and every delivery decision it owns.
#[derive(Default)]
pub struct CellEmitter {
    pub(crate) sinks: CellSinkRegistry,
    pub(crate) streams: HashMap<ChannelId, StreamOutput>,
    pub(crate) raw: RawMetadataStage,
    pub(crate) metadata: TerminalMetadataStage,
    /// v2 `cellDirty`.
    pub(crate) dirty: HashSet<ChannelId>,
    /// v2 `inputSensitiveChannels`.
    pub(crate) input_echo: HashMap<ChannelId, u8>,
    /// v2 `cellGateSuppression`: attribution only.
    pub(crate) gates: HashMap<ChannelId, GateSuppression>,
    /// v2 `cellEmissionGates`: the resize hold.
    pub(crate) emission_gates: HashSet<ChannelId>,
    /// v2 `cellEmitSchedules`.
    pub(crate) schedules: HashMap<ChannelId, CellEmissionSchedule>,
    /// Forced fulls a caller without a record owes (register, resume, install).
    pub(crate) baselines_owed: BTreeSet<ChannelId>,
    pub(crate) sync: SyncOutputState,
    pub(crate) query_replies: QueryReplyLane,
    /// Where an OSC 7 folder change becomes a `cwd` session event.
    pub(crate) cwd_events: super::cwd_events::CwdEventLane,
    /// Woken whenever the cadence owes work; `runtime::cell_cadence` waits on it.
    cadence: Arc<tokio::sync::Notify>,
}

impl std::fmt::Debug for CellEmitter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CellEmitter")
            .field("sinks", &self.sinks)
            .field("streams", &self.streams.len())
            .field("schedules", &self.schedules.len())
            .field("raw", &self.raw)
            .finish_non_exhaustive()
    }
}

impl CellEmitter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn sinks(&self) -> &CellSinkRegistry {
        &self.sinks
    }

    pub fn raw_metadata(&self) -> &RawMetadataStage {
        &self.raw
    }

    pub fn terminal_metadata(&self) -> &TerminalMetadataStage {
        &self.metadata
    }

    /// The query-reply lane every live core write answers into (WQuery's writer).
    pub fn attach_query_replies(&mut self, lane: QueryReplyLane) {
        self.query_replies = lane;
        tracing::info!("the cell emitter's query-reply lane was attached");
    }

    /// The lane an OSC 7 folder change is published through (`runtime::owners`).
    pub fn attach_cwd_events(&mut self, lane: super::cwd_events::CwdEventLane) {
        self.cwd_events = lane;
        tracing::info!("the cell emitter's cwd event lane was attached");
    }

    /// Forward probe replies on the same ordered lane (the resize boundary
    /// answers its captured bytes' probes after the resize, v2 `forwardReplies`).
    pub fn send_query_replies(
        &self,
        session_id: &roost_protocol::wire::brand::SessionId,
        bytes: Vec<u8>,
    ) -> bool {
        self.query_replies.send(session_id, bytes)
    }

    /// The notification the cadence waits on.
    pub fn cadence_wake(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.cadence)
    }

    pub(crate) fn wake_cadence(&self) {
        self.cadence.notify_one();
    }

    /// v2 `pendingSyncCellSnapshots.has`.
    pub(crate) fn sync_snapshot_owed(&self, channel_id: ChannelId) -> bool {
        self.streams
            .get(&channel_id)
            .is_some_and(|stream| stream.pending_sync_snapshot)
    }

    /// v2 `_disposeOutputState` + `cancelCellEmission`: nothing keyed on a gone
    /// channel may outlive it.
    pub fn cancel(&mut self, channel_id: ChannelId) {
        self.raw.forget_channel(channel_id);
        self.metadata.forget_channel(channel_id);
        self.input_echo.remove(&channel_id);
        self.dirty.remove(&channel_id);
        self.gates.remove(&channel_id);
        self.emission_gates.remove(&channel_id);
        self.schedules.remove(&channel_id);
        self.baselines_owed.remove(&channel_id);
        self.forget_sync_output(channel_id);
        if let Some(stream) = self.streams.get_mut(&channel_id) {
            stream.pending_repair = false;
            stream.pending_sync_snapshot = false;
        }
    }
}

/// The most history rows one live delta may append before it becomes a full.
pub use roost_term::LIVE_DELTA_SCROLLBACK_ROWS_CAP;
