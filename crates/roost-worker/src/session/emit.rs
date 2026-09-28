//! The cell-frame producer: PTY bytes in, ONE frame per tick out, fanned to
//! every registered sink. `runtime` calls it from the keeper's output binding
//! and the cadence. Depends on `roost_term` and `roost_protocol` only.
//!
//! THE LIVE PATH FEEDS THE EXISTING CORE. A chunk goes to
//! `record.terminal_core.write` and to the retained ring, and nothing here ever
//! rebuilds the emulator from those bytes (`docs/FAILURE-INDEX.md`, "Scrollback
//! mangles or drifts with no user action"). Rebuilding from the ring is process
//! ADOPTION, and it belongs to `adopt_retained_history`.
//!
//! ONE FRAME PER TICK: a second `CellEmitState` over one core would steal the
//! dirty rows the first frame claimed. So this builds one frame and then fans
//! it — a delta straight out, a full installed as a per-sink cursor, a repair
//! stream-wide. The cadence, the raw lane and the cursor are the v2 modules'
//! own concerns, in [`super::cell_scheduler`], [`super::raw_metadata`] and
//! [`super::snapshot_cursor`]. The per-channel delivery registry that admits,
//! invalidates and forgets a stream is [`super::emit_streams`].
use std::collections::{HashMap, HashSet};

use roost_protocol::cell::frame_chunks::encoded_cell_grid_frame_size;
use roost_protocol::cell::{CELL_GRID_PART_MAX_BYTES, CellGridFrame, cell_frame_to_proto};
use roost_protocol::wire::brand::ChannelId;
use roost_term::next_cell_frame;
use tracing::warn;

use super::cell_scheduler::GateSuppression;
use super::cell_sink::{CellDeltaFanout, CellSinkRegistry, FrameTimings, StreamDelivery};
use super::raw_metadata::RawMetadataStage;
use super::types::SessionRecord;

/// The coalesce window a burst of PTY chunks collapses into: ~one frame at
/// 60fps, and a hard bound on the frame rate under a flood.
pub const CELL_EMIT_COALESCE_MS: i64 = 16;

/// One channel's delivery stream: who owes a baseline. The coordinator owns
/// `enabled` and the stream generation; the record owns the core.
#[derive(Debug, Default)]
pub(crate) struct StreamOutput {
    pub(crate) stream_id: String,
    pub(crate) enabled: bool,
    pub(crate) core_valid: bool,
    pub(crate) deliveries: HashMap<String, StreamDelivery>,
    /// A repair full is owed because at least one receiver can no longer
    /// reproduce the shipped screen.
    pub(crate) pending_repair: bool,
    /// A synchronized-output hold was open when a full came due, so that full is
    /// still owed.
    pub(crate) pending_sync_snapshot: bool,
}

/// What one ingest did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestOutcome {
    /// Retained, parsed, and this channel is due an emit.
    Accepted { end_seq: u64, input_echo: bool },
    /// Retained, but nothing is watching. The bytes stay in the ring: a later
    /// stream install ships them as history.
    RetainedOnly { end_seq: u64 },
}

/// Why no frame was produced. Every variant is a condition the caller can act
/// on rather than a failure it has to log and retry blind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Withheld {
    /// No stream, the channel is disabled, or a resize invalidated the core.
    NoStream,
    /// No active sink. A suspended transport must not latch a repair.
    NoSink,
    Gate,
    /// A full is parked as a cursor for at least one sink.
    SnapshotPending,
    /// The emitter could not build a frame from this core.
    Unbuildable(String),
}

/// What one emit did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameOutcome {
    /// A full was built. `installed` is false when it could not become parts,
    /// which retires delivery until a new core is adopted.
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

/// The producer, and every delivery decision it owns. The cadence fields are
/// declared here and driven in [`super::cell_scheduler`]; the sender lifecycle
/// and the parked-full cursor in [`super::snapshot_cursor`].
#[derive(Default)]
pub struct CellEmitter {
    pub(crate) sinks: CellSinkRegistry,
    pub(crate) streams: HashMap<ChannelId, StreamOutput>,
    pub(crate) raw: RawMetadataStage,
    /// Channels with work the cadence has not run yet.
    pub(crate) dirty: HashSet<ChannelId>,
    /// Queued input-echo promotions, bounded per channel.
    pub(crate) input_echo: HashMap<ChannelId, u8>,
    /// The named hold on each gated channel.
    pub(crate) gates: HashMap<ChannelId, GateSuppression>,
}

impl std::fmt::Debug for CellEmitter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CellEmitter")
            .field("sinks", &self.sinks)
            .field("streams", &self.streams.len())
            .field("raw", &self.raw)
            .finish()
    }
}

impl CellEmitter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn sinks(&self) -> &CellSinkRegistry {
        &self.sinks
    }

    /// Ingest one PTY chunk, synchronously, from the keeper's output binding.
    ///
    /// Ring first, then core: a crash between the two leaves history ahead of
    /// the screen rather than a screen ahead of its own history.
    pub fn ingest_pty_chunk(
        &mut self,
        record: &mut SessionRecord,
        chunk: &[u8],
        now_ms: i64,
    ) -> IngestOutcome {
        let channel_id = record.channel_id();
        if record.last_pty_out_ms == 0 {
            // The arrival of the OLDEST byte not yet shipped. The emitter stamps
            // it into the frame, so the coordinator's own leg is measured
            // separately from the worker's preparation.
            record.last_pty_out_ms = now_ms;
        }
        let end_seq = record.append_retained(chunk);
        record.terminal_core.write(chunk);
        let input_echo = self.consume_input_echo_promotion(channel_id);
        if self.sinks.is_empty() && self.raw.semantic_metadata_negotiated() {
            warn!(%channel_id, len = chunk.len(), "PTY output arrived with no cell sink registered");
        }
        if !self.has_enabled_stream(channel_id) {
            return IngestOutcome::RetainedOnly { end_seq };
        }
        self.note_dirty(channel_id);
        self.raw.stage(channel_id, end_seq, chunk);
        tracing::trace!(%channel_id, len = chunk.len(), end_seq, "a PTY chunk was ingested");
        IngestOutcome::Accepted {
            end_seq,
            input_echo,
        }
    }

    ///
    /// The lane a resize transaction's capture uses: a byte parsed at stale
    /// geometry is the height-step defect in `docs/FAILURE-INDEX.md`.
    pub fn retain_without_parsing(&mut self, record: &mut SessionRecord, chunk: &[u8]) -> u64 {
        record.append_retained(chunk)
    }

    /// Emit one frame for this channel. `force` is the caller's claim that
    /// every receiver must be given a complete grid. Deltas flow only after
    /// EVERY active sink holds a baseline; until then this emit builds the full
    /// that is owed, forced or not.
    pub fn emit_cell_frame(
        &mut self,
        record: &mut SessionRecord,
        force: bool,
        now_ms: i64,
    ) -> FrameOutcome {
        let channel_id = record.channel_id();
        let Some(stream) = self.streams.get(&channel_id) else {
            return FrameOutcome::Withheld(Withheld::NoStream);
        };
        if !stream.enabled || !stream.core_valid {
            return FrameOutcome::Withheld(Withheld::NoStream);
        }
        let aggregate = self.delivery_aggregate(channel_id);
        if aggregate.active_sinks == 0 {
            // A suspended transport must not latch repairs or force baselines.
            // Its resume owes one full, so nothing needs recording here.
            return FrameOutcome::Withheld(Withheld::NoSink);
        }
        if self.gate_held(channel_id) {
            self.note_dirty(channel_id);
            self.note_gate_suppression(channel_id, now_ms);
            return FrameOutcome::Withheld(Withheld::Gate);
        }
        if aggregate.snapshot_pending {
            if !force {
                self.mark_stream_delivery_dirty(channel_id);
                self.note_dirty(channel_id);
            }
            return FrameOutcome::Withheld(Withheld::SnapshotPending);
        }
        // An active sink with no baseline is owed a full, and this emit builds
        // it. v2 builds that full inside the register or resume that created
        // the debt (`session-cell-sinks.ts:90-100,258-269`); this emitter holds
        // no record there, so the debt waits for the next emit, and withholding
        // here would leave it with nothing to pay it — every delta blocked
        // behind one sink that was never sent a baseline.
        let full_owed = force
            || !aggregate.baseline_ready
            || !record.cell_emit.sent_full
            || stream.pending_sync_snapshot;
        let built = match self.build_frame(record, full_owed, now_ms) {
            Ok(built) => built,
            Err(reason) => {
                warn!(%channel_id, %reason, "the emitter could not build a cell frame");
                return FrameOutcome::Withheld(Withheld::Unbuildable(reason));
            }
        };
        if built.escalated {
            // The delta did not fit one part. It became a full instead, and the
            // reason is a log line rather than a silent shape change: a client
            // that was promised deltas gets one without them, and only the log
            // says why.
            tracing::info!(
                %channel_id,
                seq = built.frame.seq,
                "a live delta was escalated to a full frame"
            );
        }
        self.commit_frame(record, channel_id, built)
    }

    fn commit_frame(
        &mut self,
        record: &mut SessionRecord,
        channel_id: ChannelId,
        built: Built,
    ) -> FrameOutcome {
        let Built {
            frame,
            next_state,
            timings,
            escalated: _,
        } = built;
        let seq = frame.seq;
        if frame.full {
            record.cell_emit = next_state;
            record.terminal_core.clear_dirty();
            self.clear_dirty(channel_id);
            self.clear_stream_delivery_dirty(channel_id);
            return match self.install_baseline(channel_id, frame, timings) {
                Ok(()) => FrameOutcome::Full {
                    seq,
                    installed: true,
                },
                Err(reason) => {
                    warn!(%channel_id, %reason, "a full cell frame could not be installed as a baseline");
                    // The frame was built from a core that produced something
                    // the wire refuses, so its rows cannot be trusted either.
                    self.retire_stream_delivery(channel_id);
                    self.set_core_valid(channel_id, false);
                    FrameOutcome::Full {
                        seq,
                        installed: false,
                    }
                }
            };
        }
        let fanout = self.sinks.send_frame_to_active(channel_id, &frame, timings);
        for sink_id in &fanout.overflowed {
            // The registry already dropped it; a record left behind would keep a
            // gone sink's cursor parked in every later drain.
            self.forget_sink_records(sink_id);
        }
        if fanout.accepted == 0 {
            // Nobody took this sequence, so the repair full re-uses it and the
            // receiver's sequence space stays contiguous. The emit state must
            // NOT advance: the same seq is still owed as a full.
            self.note_repair(channel_id);
            return FrameOutcome::Delta { seq, fanout };
        }
        record.cell_emit = next_state;
        record.terminal_core.clear_dirty();
        self.clear_dirty(channel_id);
        // The oldest unshipped byte has now shipped, so the next frame measures
        // its own preparation segment rather than this one's whole wait.
        record.last_pty_out_ms = 0;
        if fanout.dropped > 0 {
            // A sink that dropped the delta its siblings took owes a fresh
            // baseline, and the worker fold can no longer reproduce what every
            // receiver holds. The repair is stream-wide: one core, one frame.
            self.note_repair(channel_id);
        }
        FrameOutcome::Delta { seq, fanout }
    }

    /// Build the next frame, and the state that produced it. A delta that does
    /// not fit one part is rebuilt as a full rather than chunked: a delta has no
    /// snapshot id, so a receiver assembling one from parts could not know it
    /// had lost a part.
    fn build_frame(
        &self,
        record: &SessionRecord,
        full: bool,
        now_ms: i64,
    ) -> Result<Built, String> {
        let core = record.terminal_core.as_ref();
        let (mut frame, state) = next_cell_frame(core, &record.cell_emit, full, Some(0))
            .map_err(|error| error.to_string())?;
        // The part ceiling is a CANONICAL PROTOBUF BYTE COUNT, so the frame is
        // materialised once here to be measured. The sink materialises it again
        // for the wire: this crate does not depend on `roost-proto`, and a
        // second value model to avoid that copy would be a worse trade.
        let wire = cell_frame_to_proto(&frame, record.session_id().as_str())
            .map_err(|error| error.to_string())?;
        if !wire.full && encoded_cell_grid_frame_size(&wire) > CELL_GRID_PART_MAX_BYTES {
            let (escalated, state) = next_cell_frame(core, &record.cell_emit, true, Some(0))
                .map_err(|error| error.to_string())?;
            frame = escalated;
            return Ok(Built {
                frame,
                next_state: state,
                timings: frame_timings(record.last_pty_out_ms, now_ms),
                escalated: true,
            });
        }
        Ok(Built {
            frame,
            next_state: state,
            timings: frame_timings(record.last_pty_out_ms, now_ms),
            escalated: false,
        })
    }
}

/// One built frame, the state that produced it, and its two clocks.
struct Built {
    frame: CellGridFrame,
    next_state: roost_term::CellEmitState,
    timings: FrameTimings,
    /// The delta did not fit one part and was rebuilt as a full.
    escalated: bool,
}

/// The two clocks a frame carries: when the oldest unshipped PTY byte arrived,
/// and when this worker finished preparing it. With no unshipped byte yet, the
/// arrival is this frame's own emit time — the byte is in this very frame.
fn frame_timings(last_pty_out_ms: i64, worker_emit_ms: i64) -> FrameTimings {
    FrameTimings {
        pty_out_ms: if last_pty_out_ms > 0 {
            last_pty_out_ms
        } else {
            worker_emit_ms
        },
        worker_emit_ms,
    }
}

/// Re-exported for a caller that wants the same bound the emitter holds itself
/// to: the most history rows one live delta may append before it becomes a
/// full frame rather than a frame with a silent hole.
pub use roost_term::LIVE_DELTA_SCROLLBACK_ROWS_CAP;
