//! The cell-frame producer's state, including per-channel output teardown.
//! PTY bytes enter through
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
use super::sync_output::SyncOutputState;
use super::terminal_metadata::TerminalMetadataStage;

/// A burst of chunks collapses into one trailing frame.
pub const CELL_EMIT_COALESCE_MS: i64 = 16;

/// One channel's delivery stream. The coordinator owns `enabled` and the
/// generation; the record owns the core.
#[derive(Debug, Default)]
pub(crate) struct StreamOutput {
    pub(crate) stream_id: String,
    pub(crate) enabled: bool,
    pub(crate) core_valid: bool,
    pub(crate) deliveries: HashMap<String, StreamDelivery>,
    /// A receiver can no longer reproduce the screen.
    pub(crate) pending_repair: bool,
    /// A full came due inside a synchronized frame.
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
    pub(crate) metadata: TerminalMetadataStage,
    /// Channels with output not yet framed.
    pub(crate) dirty: HashSet<ChannelId>,
    /// The instant each channel's echo window
    /// closes (`cell_scheduler::INPUT_ECHO_WINDOW_MS`).
    pub(crate) input_echo: HashMap<ChannelId, i64>,
    /// Gate attribution only.
    pub(crate) gates: HashMap<ChannelId, GateSuppression>,
    /// The resize hold.
    pub(crate) emission_gates: HashSet<ChannelId>,
    /// Queued emissions per channel.
    pub(crate) schedules: HashMap<ChannelId, CellEmissionSchedule>,
    /// Forced fulls a caller without a record owes (register, resume, install).
    pub(crate) baselines_owed: BTreeSet<ChannelId>,
    pub(crate) sync: SyncOutputState,
    pub(crate) query_replies: QueryReplyLane,
    /// Where an OSC 7 folder change becomes a `cwd` session event.
    pub(crate) cwd_events: super::cwd_events::CwdEventLane,
    /// The terminal incident recorder's data-path tap (detached by default).
    pub(crate) capture: crate::capture::CaptureTap,
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

    /// The terminal incident recorder's tap.
    pub fn attach_capture(&mut self, tap: crate::capture::CaptureTap) {
        self.capture = tap;
        tracing::info!("the cell emitter's terminal capture tap was attached");
    }

    /// The tap the resize boundary notes its install and result through.
    pub fn capture_tap(&self) -> crate::capture::CaptureTap {
        self.capture.clone()
    }

    /// Forward probe replies on the same ordered lane (the resize boundary
    /// answers its captured bytes' probes after the resize).
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

    /// Whether a full is owed because it came due inside a synchronized frame.
    pub(crate) fn sync_snapshot_owed(&self, channel_id: ChannelId) -> bool {
        self.streams
            .get(&channel_id)
            .is_some_and(|stream| stream.pending_sync_snapshot)
    }

    /// Drop all per-channel output state and the queued emission: nothing keyed on a gone
    /// channel may outlive it.
    pub fn cancel(&mut self, channel_id: ChannelId) {
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
