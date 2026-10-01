//! What the diagnostic snapshot says about one live session. Owned by the
//! worker.
//!
//! THE SHAPE IS THE CONTRACT, NOT A CONVENIENCE. `diag-snapshot` crosses a
//! process boundary into the coordinator's `diag.snapshot` fan-out and out to
//! every layered terminal probe, and those readers key on these names: a
//! `sessions` map keyed by session id, and inside it `raw.head_seq`,
//! `cell.seq`, `cell.grid_epoch`, `channel_binding` and `sync_output`. A
//! report without them is not a smaller report — it is a report the readers
//! resolve to `null` and then treat as a stalled stream.
//!
//! THREE RANGES, DELIBERATELY. What the byte ring retains, what the core
//! retains in lines, and what the last emitted frame told the browser are three
//! different facts about three different structures, and the only useful
//! diagnostic is the disagreement between them. `cell.sb_dropped` is frozen at
//! the last SUCCESSFUL emit, so every gate that withholds a frame leaves it
//! stale by whatever the ring evicted since; comparing it against `cell.core`
//! is the whole point of carrying both.
//!
//! NOTHING HERE MAY CARRY TERMINAL TEXT. Every value is an id, a bound, a count
//! or a duration, because this report is read by parsers and shipped to
//! operators. Ports v2 `apps/worker/src/session/session-diag-snapshot.ts`.

use serde_json::{Value, json};

use roost_protocol::wire::brand::ChannelId;
use roost_term::scrollback_origin;

use super::cell_gates::CELL_GATE_BUDGET_MS;
use super::emit::CellEmitter;
use super::lifecycle::SessionManager;
use super::types::SessionRecord;

/// The emitter-owned half of one session's diagnostic facts.
///
/// Split from the record's own facts because the emitter is behind a trait and
/// the record is behind the session table, and a fold that needed both would
/// have to hold two locks at once to read one channel.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChannelDiagnostics {
    pub dirty: bool,
    pub gate_active: bool,
    pub gate: Option<&'static str>,
    pub gate_since_ms: Option<i64>,
    pub gate_suppressed: u64,
    pub gate_over_budget: bool,
    pub pending_repair: bool,
    pub stream: Option<StreamDiagnostics>,
    pub sync_output: Option<SyncOutputDiagnostics>,
    pub raw_frames: usize,
    pub raw_bytes: usize,
}

/// One channel's delivery stream, as the report names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamDiagnostics {
    pub stream_id: String,
    pub enabled: bool,
    pub core_valid: bool,
    /// The coordinator sink's own delivery record; the per-sink list below is
    /// the truth for every sink, including a local terminal socket.
    pub baseline_ready: bool,
    pub baseline_dirty: bool,
    pub deliveries: Vec<SinkDiagnostics>,
}

/// One sink's place in a stream's snapshot cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SinkDiagnostics {
    pub sink_id: String,
    pub active: bool,
    pub baseline_ready: bool,
    pub baseline_dirty: bool,
    pub snapshot_id: Option<String>,
    pub snapshot_next_part: Option<usize>,
    pub snapshot_part_count: Option<usize>,
}

/// An open DEC 2026 frame and the ceilings it is being held to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncOutputDiagnostics {
    pub generation: u64,
    pub sb_total_at_open: u64,
    pub tripped: bool,
}

impl CellEmitter {
    /// One channel's emitter-owned facts, read once.
    pub fn channel_diagnostics(&self, channel_id: ChannelId) -> ChannelDiagnostics {
        let suppression = self.gate_suppression(channel_id);
        let (raw_frames, raw_bytes) = self.raw.channel_backlog(channel_id);
        ChannelDiagnostics {
            dirty: self.dirty.contains(&channel_id),
            gate_active: self.gate_held(channel_id) || self.sync_output_held(channel_id),
            gate: suppression.map(|held| held.gate.as_str()),
            gate_since_ms: suppression.map(|held| held.since_ms),
            gate_suppressed: suppression.map_or(0, |held| held.suppressed),
            gate_over_budget: suppression.is_some_and(|held| held.over_budget),
            pending_repair: self.repair_latched(channel_id),
            stream: self
                .streams
                .get(&channel_id)
                .map(|stream| StreamDiagnostics {
                    stream_id: stream.stream_id.clone(),
                    enabled: stream.enabled,
                    core_valid: stream.core_valid,
                    baseline_ready: stream
                        .deliveries
                        .get(crate::session::cell_sink::COORD_CELL_SINK_ID)
                        .is_some_and(|delivery| delivery.baseline_ready),
                    baseline_dirty: stream
                        .deliveries
                        .get(crate::session::cell_sink::COORD_CELL_SINK_ID)
                        .is_some_and(|delivery| delivery.baseline_dirty),
                    deliveries: stream
                        .deliveries
                        .iter()
                        .map(|(sink_id, delivery)| SinkDiagnostics {
                            sink_id: sink_id.clone(),
                            active: self.sinks.is_active(sink_id),
                            baseline_ready: delivery.baseline_ready,
                            baseline_dirty: delivery.baseline_dirty,
                            snapshot_id: delivery
                                .cursor
                                .as_ref()
                                .map(|cursor| cursor.snapshot_id.clone()),
                            snapshot_next_part: delivery
                                .cursor
                                .as_ref()
                                .map(|cursor| cursor.next_part),
                            snapshot_part_count: delivery
                                .cursor
                                .as_ref()
                                .map(|cursor| cursor.parts.len()),
                        })
                        .collect(),
                }),
            sync_output: self.sync_output_diagnostics(channel_id),
            raw_frames,
            raw_bytes,
        }
    }
}

/// One session's whole diagnostic record, as the report names it.
///
/// `worker_fp` and the wall clock arrive as arguments because the session
/// record does not own them: the fingerprint is the worker's identity and the
/// reading is the report's, taken once for every channel it covers.
pub fn session_value(
    record: &SessionRecord,
    worker_fp: &str,
    channel: &ChannelDiagnostics,
    unhandled: Option<Value>,
    now_mono_ms: u64,
) -> Value {
    let core = record.terminal_core.as_ref();
    let live_dropped = scrollback_origin(core, record.cell_emit.scrollback_origin).ok();
    let live_retained = core.scrollback_count() as u64;
    let core_facts = live_dropped.map(|dropped| {
        json!({
            "discarded": dropped.saturating_sub(record.cell_emit.scrollback_origin),
            "dropped": dropped,
            "retained_lines": live_retained,
            "total": dropped + live_retained,
        })
    });
    let retained_bytes = record.scrollback.len() as u64;
    json!({
        "session_trace_id": record.identity.session_trace_id.as_str(),
        "cwd": record.identity.cwd,
        "channel_binding": {
            "worker_fp": worker_fp,
            "channel_id": record.channel_id().as_u32(),
        },
        "raw": {
            "head_seq": record.head_seq,
            "tail_seq": record.head_seq.saturating_sub(retained_bytes),
            "retained_bytes": retained_bytes,
            "cap_bytes": record.scrollback.capacity() as u64,
            "evicting": record.scrollback.evicting(),
        },
        "cell": {
            "grid_epoch": record.cell_emit.grid_epoch(),
            "seq": record.cell_emit.seq,
            "dirty": channel.dirty,
            "sb_dropped": record.cell_emit.sb_dropped,
            "sb_origin": record.cell_emit.scrollback_origin,
            "last_sb_total": record.cell_emit.last_scrollback_total,
            "core": core_facts,
            "origin_pin": record.sb_origin_pin.as_ref().map(|pin| json!({
                "at_mono_ms": pin.at_mono_ms,
                "age_ms": now_mono_ms.saturating_sub(pin.at_mono_ms),
                "cols": pin.cols,
                "rows": pin.rows,
                "replayed_ring": pin.replayed_ring,
                "ring_evicted": pin.ring_evicted,
                "clamped": pin.clamped,
                "replay_lost_rows": pin.replay_lost_rows,
                "replay_floor": pin.replay_floor,
                "sb_origin": pin.sb_origin,
                "sb_dropped": pin.sb_dropped,
            })),
        },
        "gate": {
            "active": channel.gate_active,
            "gate": channel.gate,
            "age_ms": channel.gate_since_ms,
            "suppressed_frames": channel.gate_suppressed,
            "over_budget": channel.gate_over_budget,
            "budget_ms": CELL_GATE_BUDGET_MS,
        },
        "sync_output": channel.sync_output.map(|hold| json!({
            "generation": hold.generation,
            "sb_total_at_open": hold.sb_total_at_open,
            "tripped": hold.tripped,
        })),
        "pending_repair": channel.pending_repair,
        "terminal_stream": channel.stream.as_ref().map(|stream| json!({
            "stream_id": stream.stream_id,
            "enabled": stream.enabled,
            "core_valid": stream.core_valid,
            "baseline_ready": stream.baseline_ready,
            "baseline_dirty": stream.baseline_dirty,
            "deliveries": stream.deliveries.iter().map(|delivery| json!({
                "sink_id": delivery.sink_id,
                "active": delivery.active,
                "baseline_ready": delivery.baseline_ready,
                "baseline_dirty": delivery.baseline_dirty,
                "snapshot_id": delivery.snapshot_id,
                "snapshot_next_part": delivery.snapshot_next_part,
                "snapshot_part_count": delivery.snapshot_part_count,
            })).collect::<Vec<Value>>(),
        })),
        "terminal_control": {
            "raw_metadata_queue": {
                "pending_frames": channel.raw_frames,
                "pending_bytes": channel.raw_bytes,
            },
        },
        "terminal": {
            "alt_mode": record.alt_mode,
            "cols": core.cols(),
            "rows": core.rows(),
            "input_modes": {
                "mouse_sgr": core.mouse_sgr(),
                "focus_events": core.focus_events(),
            },
            "unhandled_sequences": unhandled,
        },
    })
}

impl SessionManager {
    /// This worker's own fingerprint, for a report that names which machine a
    /// session belongs to.
    pub fn worker_fingerprint(&self) -> &str {
        self.worker_fp.as_str()
    }

    /// The emitter seam the diagnostic snapshot reads its delivery facts
    /// through, so the snapshot never reaches past the trait into the emitter.
    pub fn cells(&self) -> &std::sync::Mutex<dyn super::binding::CellDelivery> {
        &self.cells
    }
}
