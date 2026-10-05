//! One cell frame: v2 `apps/worker/src/session/session-emit.ts` `emitCellFrame`
//! and `installTerminalBaseline` — the gate checks, the build (a delta too big
//! for one part escalates to a full), the commit that installs a baseline or
//! fans a delta out, and the stream-wide repair a dropped delta owes. Split from
//! `session::emit`; called by `session::{cell_scheduler, emit_ingest,
//! sync_output}` and `runtime::channel_delivery`.

use std::time::Instant;

use roost_observability::clock::{EventClock, SystemClock};
use roost_proto::PbCellGridFrame;
use roost_protocol::cell::frame_chunks::encoded_cell_grid_frame_size;
use roost_protocol::cell::{CELL_GRID_PART_MAX_BYTES, CellGridFrame, cell_frame_to_proto};
use roost_protocol::terminal_capture::bundle::TerminalCoverageReason;
use roost_protocol::wire::brand::ChannelId;
use roost_term::next_cell_frame;
use tracing::warn;

use super::cell_gates::CellGate;
use super::cell_sink::FrameTimings;
use super::emit::{CellEmitter, FrameOutcome, Withheld};
use super::snapshot_cursor::prepare_cell_renewal_epoch;
use super::sync_output::SyncOutputAction;
use super::types::SessionRecord;

impl CellEmitter {
    /// v2 `installTerminalBaseline`: `emitCellFrame(force = true)`.
    pub fn install_terminal_baseline(
        &mut self,
        record: &mut SessionRecord,
        now_ms: i64,
    ) -> FrameOutcome {
        self.install_terminal_baseline_at(record, now_ms, Instant::now())
    }

    pub fn install_terminal_baseline_at(
        &mut self,
        record: &mut SessionRecord,
        now_ms: i64,
        now: Instant,
    ) -> FrameOutcome {
        self.baselines_owed.remove(&record.channel_id());
        self.emit_cell_frame_at(record, true, now_ms, now)
    }

    /// Emit one frame for this channel (v2 `emitCellFrame`).
    pub fn emit_cell_frame(
        &mut self,
        record: &mut SessionRecord,
        force: bool,
        now_ms: i64,
    ) -> FrameOutcome {
        self.emit_cell_frame_at(record, force, now_ms, Instant::now())
    }

    /// Deltas flow only once EVERY active sink holds a baseline; ONE frame is
    /// built per tick and fanned out (a second builder over one core would steal
    /// the dirty rows this one claimed).
    pub fn emit_cell_frame_at(
        &mut self,
        record: &mut SessionRecord,
        force: bool,
        now_ms: i64,
        now: Instant,
    ) -> FrameOutcome {
        let channel_id = record.channel_id();
        let Some(stream) = self
            .streams
            .get(&channel_id)
            .filter(|s| s.enabled && s.core_valid)
        else {
            return FrameOutcome::Withheld(Withheld::NoStream);
        };
        let sync_owed = stream.pending_sync_snapshot;
        let delivery = self.delivery_aggregate(channel_id);
        if delivery.active_sinks == 0 {
            return FrameOutcome::Withheld(Withheld::NoSink);
        }
        if self.gate_held(channel_id) {
            self.mark_stream_delivery_dirty(channel_id);
            self.note_dirty(channel_id);
            self.note_gate_suppression(channel_id, CellGate::ResizeCapture, now_ms);
            return FrameOutcome::Withheld(Withheld::Gate);
        }
        if delivery.snapshot_pending {
            if !force {
                self.mark_stream_delivery_dirty(channel_id);
            }
            return FrameOutcome::Withheld(Withheld::SnapshotPending);
        }
        if !force && !delivery.baseline_ready {
            self.mark_stream_delivery_dirty(channel_id);
            self.note_dirty(channel_id);
            self.note_gate_suppression(channel_id, CellGate::Baseline, now_ms);
            return FrameOutcome::Withheld(Withheld::Baseline);
        }
        let full_owed = force || !record.cell_emit.sent_full || sync_owed;
        let action = self.sync_output_action(record, now_ms, now);
        if action == SyncOutputAction::Hold {
            if full_owed {
                self.set_pending_sync_snapshot(channel_id, true);
            }
            self.mark_stream_delivery_dirty(channel_id);
            self.note_dirty(channel_id);
            self.note_gate_suppression(channel_id, CellGate::SyncOutput, now_ms);
            return FrameOutcome::Withheld(Withheld::SyncOutput);
        }
        self.cancel_cell_emission(channel_id);
        if full_owed && record.cell_emit.sent_full && record.cell_emit.seq == 0 {
            prepare_cell_renewal_epoch(record.terminal_core.as_ref(), &mut record.cell_emit);
        }
        let built = match self.build_frame(record, full_owed, now_ms) {
            Ok(built) => built,
            Err(reason) => {
                warn!(%channel_id, %reason, "the emitter could not build a cell frame");
                return FrameOutcome::Withheld(Withheld::Unbuildable(reason));
            }
        };
        if built.escalated {
            tracing::info!(%channel_id, seq = built.frame.seq, "a live delta was escalated to a full frame");
        }
        super::unhandled_seq::note_unhandled_sequences(record, SystemClock.mono_ns() / 1_000_000);
        self.commit_frame(record, channel_id, built, now_ms, now)
    }

    fn commit_frame(
        &mut self,
        record: &mut SessionRecord,
        channel_id: ChannelId,
        built: Built,
        now_ms: i64,
        now: Instant,
    ) -> FrameOutcome {
        let Built {
            frame,
            wire,
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
            let evidence = self.capture.wants_emissions().then(|| frame.clone());
            if let Err(reason) = self.install_baseline(channel_id, frame, wire, timings) {
                // A core that produced something the wire refuses cannot be
                // trusted either (v2 `installStreamBaseline` → coreValid=false).
                warn!(%channel_id, %reason, "a full cell frame could not be installed as a baseline");
                self.capture
                    .rejected_emission(record, TerminalCoverageReason::BaselineInvalidated);
                self.retire_stream_delivery(channel_id);
                self.set_core_valid(channel_id, false);
                return FrameOutcome::Full {
                    seq,
                    installed: false,
                };
            }
            self.capture.accepted_emission(record, evidence);
            record.last_pty_out_ms = 0;
            return FrameOutcome::Full {
                seq,
                installed: true,
            };
        }
        let fanout = self.sinks.send_frame_to_active(channel_id, &frame, &wire);
        for sink_id in &fanout.overflowed {
            self.forget_sink_records(sink_id);
        }
        if fanout.accepted == 0 {
            // Nobody took this seq, so the repair full re-uses it and the
            // receivers' sequence space stays contiguous.
            self.capture
                .rejected_emission(record, TerminalCoverageReason::BaselineInvalidated);
            self.repair_stream(record, now_ms, now);
            return FrameOutcome::Delta { seq, fanout };
        }
        record.cell_emit = next_state;
        record.terminal_core.clear_dirty();
        self.clear_dirty(channel_id);
        self.capture.accepted_emission(record, Some(frame));
        record.last_pty_out_ms = 0;
        if fanout.dropped > 0 {
            // One core, one frame per tick: a sink that dropped what its
            // siblings took costs ONE stream-wide full.
            self.capture
                .rejected_emission(record, TerminalCoverageReason::BaselineInvalidated);
            self.repair_stream(record, now_ms, now);
        }
        FrameOutcome::Delta { seq, fanout }
    }

    /// Latch the repair, invalidate every active sink's baseline, and build the
    /// forced full now (v2 `pendingCellRepairs.add` + `installTerminalBaseline`).
    fn repair_stream(&mut self, record: &mut SessionRecord, now_ms: i64, now: Instant) {
        let channel_id = record.channel_id();
        let active: Vec<String> = self
            .sinks
            .active_sinks()
            .iter()
            .map(|sink| sink.id().to_owned())
            .collect();
        if let Some(stream) = self.streams.get_mut(&channel_id) {
            stream.pending_repair = true;
            for sink_id in active {
                stream.deliveries.entry(sink_id).or_default().baseline_ready = false;
            }
        }
        tracing::warn!(%channel_id, "a sink dropped a delta; one stream-wide repair full is owed");
        self.install_terminal_baseline_at(record, now_ms, now);
    }

    /// v2 `pendingCellRepairs.delete`: take the latch.
    pub(crate) fn take_repair_latch(&mut self, channel_id: ChannelId) -> bool {
        self.streams
            .get_mut(&channel_id)
            .is_some_and(|stream| std::mem::take(&mut stream.pending_repair))
    }

    pub(crate) fn repair_latched(&self, channel_id: ChannelId) -> bool {
        self.streams
            .get(&channel_id)
            .is_some_and(|stream| stream.pending_repair)
    }

    /// Build the next frame. A delta that does not fit one part is rebuilt as
    /// a full rather than chunked: a delta has no snapshot id to reassemble by.
    fn build_frame(
        &self,
        record: &SessionRecord,
        full: bool,
        now_ms: i64,
    ) -> Result<Built, String> {
        let core = record.terminal_core.as_ref();
        let (frame, state) = next_cell_frame(core, &record.cell_emit, full, Some(0))
            .map_err(|error| error.to_string())?;
        let timings = frame_timings(record.last_pty_out_ms, now_ms);
        let wire = frame_wire(&frame, timings).map_err(|error| error.to_string())?;
        // Every receiver names the session, so the part cap is measured with
        // the id it will carry: one tag byte, a length byte, the id.
        let session_bytes =
            u32::try_from(record.session_id().as_str().len() + 2).unwrap_or(u32::MAX);
        let named_size = encoded_cell_grid_frame_size(&wire).saturating_add(session_bytes);
        if !wire.full && named_size > CELL_GRID_PART_MAX_BYTES {
            let (frame, next_state) = next_cell_frame(core, &record.cell_emit, true, Some(0))
                .map_err(|error| error.to_string())?;
            let wire = frame_wire(&frame, timings).map_err(|error| error.to_string())?;
            return Ok(Built {
                frame,
                wire,
                next_state,
                timings,
                escalated: true,
            });
        }
        Ok(Built {
            frame,
            wire,
            next_state: state,
            timings,
            escalated: false,
        })
    }
}

/// One built frame, its single wire conversion, the state that produced it,
/// and its two clocks.
struct Built {
    frame: CellGridFrame,
    wire: PbCellGridFrame,
    next_state: roost_term::CellEmitState,
    timings: FrameTimings,
    escalated: bool,
}

/// The session id a worker stamps into a cell frame it builds.
///
/// Empty on purpose: the coordinator fills it in from its own channel-to-session
/// map and explicitly adopts an empty one
/// (`apps/coord/src/terminal/screen/byte-hub.ts:195`), while a NON-empty value
/// that disagrees is refused. A local sink names the session itself.
pub(crate) const NO_SESSION_ID: &str = "";

/// One clock, as the wire spells it. A negative producer reading is a clock
/// that could not be read, and `0` is what a reader takes as "not measured",
/// so saturating is the truthful mapping.
pub(crate) fn measured_at(clock_ms: i64) -> u64 {
    u64::try_from(clock_ms).unwrap_or(0)
}

/// The frame's ONE protobuf conversion, carrying its two clocks; every sink
/// clones this rather than walking the spans again.
pub fn frame_wire(
    frame: &CellGridFrame,
    timings: FrameTimings,
) -> roost_protocol::ProtocolResult<PbCellGridFrame> {
    let mut wire = cell_frame_to_proto(frame, NO_SESSION_ID)?;
    wire.pty_out_ms = measured_at(timings.pty_out_ms);
    wire.worker_emit_ms = measured_at(timings.worker_emit_ms);
    Ok(wire)
}

/// When the oldest unshipped PTY byte arrived, and when this frame was
/// prepared; with no unshipped byte the arrival is the emit itself.
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
