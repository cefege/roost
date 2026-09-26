//! Who receives this session's cell frames, and what each receiver still owes.
//! `session::emit` builds a frame and hands it here; the coordinator link and
//! every local terminal socket implement [`CellSink`]. Depends on
//! `roost_protocol` for the frame, part and channel shapes — and on nothing
//! that calls back into emission.
//!
//! THE OVERFLOW RULE, which is the whole reason the registry is its own file.
//! A sink that answers [`CellSinkResult::Overflow`] is DROPPED and told once
//! through [`CellSink::on_overflow`], so its owner can close the transport.
//! Latching a repair for a queue that cannot drain is how a local delivery
//! queue grows without bound; the registry would be holding a reference to a
//! transport nobody is reading. The other sinks keep running: one wedged
//! browser must not stop the coordinator from painting.
//!
//! A sink that merely answers [`CellSinkResult::Dropped`] keeps its
//! registration and owes a fresh baseline — it still has a transport, and a
//! receiver that missed a delta can no longer reproduce the shipped screen.

use std::collections::HashMap;
use std::sync::Arc;

use roost_protocol::cell::CellGridFrame;
use roost_protocol::cell::frame_chunks::CellGridSnapshotPart;
use roost_protocol::wire::brand::ChannelId;

/// The coordinator link's sink id. Byte-compatible with v2's, because it is
/// the same word in both workers' logs and a diagnostic that has to be read
/// against a v2 line says `local:7` and `coord`.
pub const COORD_CELL_SINK_ID: &str = "coord";

/// The prefix every local terminal socket's sink id carries.
pub const LOCAL_CELL_SINK_PREFIX: &str = "local:";

/// The sink id one local socket registers under.
///
/// `socket_id` is the door's own connection id, so a reconnect is a NEW sink
/// and the previous one is unregistered rather than silently reused — a reused
/// id would leave the old cursor's baseline flag claiming a screen the new
/// socket has never seen.
pub fn local_cell_sink_id(socket_id: &str) -> String {
    format!("{LOCAL_CELL_SINK_PREFIX}{socket_id}")
}

/// A sink's answer for one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellSinkResult {
    /// The frame is on this sink's transport.
    Sent,
    /// This sink cannot take it right now and owes a fresh baseline. The sink
    /// stays registered.
    Dropped,
    /// This sink's queue cannot drain. Terminal for the sink: the registry
    /// drops it and calls [`CellSink::on_overflow`] exactly once.
    Overflow,
}

/// The two clocks every frame carries, measured by the producer.
///
/// They are the worker's own leg of the delivery path: `pty_out_ms` is when the
/// OLDEST unshipped PTY byte arrived at this worker, and `worker_emit_ms` is
/// when the frame was finished. The coordinator stamps its own two beside them,
/// so one frame says how long the keeper, this worker, and the coordinator each
/// held it — a sink cannot measure either of the first two, so it is handed
/// them rather than asked to guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameTimings {
    pub pty_out_ms: i64,
    pub worker_emit_ms: i64,
}

/// One receiver of a session's cell frames.
///
/// `&self` throughout for the same reason [`super::sinks::ChannelBinding`] is:
/// the emitter holds the record and calls into a transport it does not own, and
/// a send that could block would hold the session lock across a socket write.
pub trait CellSink: Send + Sync {
    /// This sink's registry id. Unique across the worker; see
    /// [`COORD_CELL_SINK_ID`] and [`local_cell_sink_id`].
    fn id(&self) -> &str;

    /// One complete grid, or a delta, on this sink's transport.
    fn send_frame(
        &self,
        channel_id: ChannelId,
        frame: &CellGridFrame,
        timings: FrameTimings,
    ) -> CellSinkResult;

    /// One part of a parked full. Only ever called for a frame too large to
    /// send whole, and always in part order. The part is the protocol's own
    /// enum, so "one chunk" and "the whole frame" stay the two things the chunk
    /// plan already distinguishes.
    fn send_snapshot_part(
        &self,
        channel_id: ChannelId,
        part: &CellGridSnapshotPart,
        timings: FrameTimings,
    ) -> CellSinkResult;

    /// Called ONCE, and only when the registry drops this sink for an overflow.
    /// Its owner closes the transport here. Never called for an ordinary
    /// unregister, and never called twice for one sink.
    fn on_overflow(&self) {}
}

/// What one fanout of a built frame did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CellDeltaFanout {
    /// Active sinks that took the frame.
    pub accepted: usize,
    /// Active sinks that refused it and now owe a fresh baseline.
    pub dropped: usize,
    /// Sinks dropped for an overflow during this fanout. They are no longer
    /// registered, so the caller owes them nothing and must forget their
    /// delivery records.
    pub overflowed: Vec<String>,
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
    pub parts: Arc<Vec<CellGridSnapshotPart>>,
    pub next_part: usize,
    /// The two clocks the FULL was measured with. Every part of it carries the
    /// same pair, and they are kept with the cursor rather than recomputed: a
    /// part sent minutes later must not claim it was prepared minutes later.
    pub timings: FrameTimings,
}

/// One registered sink's independent progress on one stream.
///
/// Each sink owns its own baseline: a coordinator that cannot accept frames
/// never stalls or re-baselines a local socket that can paint.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamDelivery {
    /// The parked full, while this sink is still receiving its parts.
    pub cursor: Option<SnapshotCursor>,
    /// False from stream install until this sink has received the LAST part of
    /// a full, and false again for a sink that has just joined or was told to
    /// re-baseline.
    pub baseline_ready: bool,
    /// Dirty work arrived while this sink's full was blocked. Deltas may not
    /// flow until the debt is paid, and the debt is stream-wide because
    /// emission builds ONE frame per tick over one core.
    pub baseline_dirty: bool,
}

/// Every whole-stream delivery question, answered once over the ACTIVE sinks.
/// Suspended and unregistered sinks contribute nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreamDeliveryAggregate {
    pub active_sinks: usize,
    /// Every active sink holds a complete baseline: deltas may flow.
    pub baseline_ready: bool,
    /// An active sink is mid-snapshot, so the shared builder must not advance.
    pub snapshot_pending: bool,
    /// Work arrived while an active sink's baseline was blocked.
    pub baseline_dirty: bool,
    /// Parts still owed across active sinks.
    pub remaining_snapshot_parts: usize,
    /// Parts in the snapshots currently in flight across active sinks.
    pub snapshot_part_count: usize,
}

#[derive(Clone)]
struct SinkEntry {
    sink: Arc<dyn CellSink>,
    active: bool,
}

/// The registered sinks, and the one aggregation every delivery question goes
/// through.
///
/// The registry does NOT own the per-stream delivery records: they are keyed by
/// channel as well as sink id, and they live with the emitter that advances
/// them. Every path that removes a sink returns what the caller must forget, so
/// the two cannot drift.
#[derive(Default)]
pub struct CellSinkRegistry {
    sinks: HashMap<String, SinkEntry>,
}

impl std::fmt::Debug for CellSinkRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CellSinkRegistry")
            .field("sinks", &self.sinks.keys().collect::<Vec<_>>())
            .field(
                "active",
                &self.sinks.iter().filter(|(_, entry)| entry.active).count(),
            )
            .finish()
    }
}

impl CellSinkRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a sink is registered at all, active or suspended.
    ///
    /// Distinct from [`CellSinkRegistry::is_active`]: a suspended sink is still
    /// registered, and a cursor belonging to one is parked rather than gone.
    pub fn contains(&self, sink_id: &str) -> bool {
        self.sinks.contains_key(sink_id)
    }
    pub fn len(&self) -> usize {
        self.sinks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sinks.is_empty()
    }

    pub fn is_active(&self, sink_id: &str) -> bool {
        self.sinks.get(sink_id).is_some_and(|entry| entry.active)
    }

    /// Add a sink, replacing any registration under the same id.
    ///
    /// A replaced sink is not told: it was replaced, not overflowed, and its
    /// owner is the only thing that can know the two apart. The caller owes a
    /// forced full for every watched channel either way, because a receiver
    /// that has just (re)gained delivery has no baseline.
    pub fn register(&mut self, sink: Arc<dyn CellSink>) {
        let id = sink.id().to_owned();
        tracing::info!(sink_id = %id, "a cell sink registered");
        self.sinks.insert(id, SinkEntry { sink, active: true });
    }

    /// Remove a sink. Returns it, so the caller can forget its delivery
    /// records on every stream.
    pub fn unregister(&mut self, sink_id: &str) -> Option<Arc<dyn CellSink>> {
        let removed = self.sinks.remove(sink_id).map(|entry| entry.sink);
        if removed.is_some() {
            tracing::info!(sink_id, "a cell sink unregistered");
        }
        removed
    }

    /// The transport is known down: stop delivering, without latching a repair
    /// or forcing a full, so a dead coordinator cannot restart baselines while
    /// another sink keeps painting. Its resume owes one full.
    pub fn suspend(&mut self, sink_id: &str) -> bool {
        let Some(entry) = self.sinks.get_mut(sink_id) else {
            return false;
        };
        if !entry.active {
            return false;
        }
        entry.active = false;
        tracing::info!(sink_id, "a cell sink was suspended");
        true
    }

    /// The transport is usable again. Returns whether this call was the one
    /// that re-activated it, because only that owes a forced full.
    pub fn resume(&mut self, sink_id: &str) -> bool {
        let Some(entry) = self.sinks.get_mut(sink_id) else {
            return false;
        };
        if entry.active {
            return false;
        }
        entry.active = true;
        tracing::info!(sink_id, "a cell sink was resumed");
        true
    }

    /// The sinks that must receive this tick's frame.
    ///
    /// Snapshotted because a send may drop a sink, and a later iteration must
    /// not then hand a frame to a sink the registry has already removed.
    pub fn active_sinks(&self) -> Vec<Arc<dyn CellSink>> {
        self.sinks
            .values()
            .filter(|entry| entry.active)
            .map(|entry| entry.sink.clone())
            .collect()
    }

    /// Ship ONE built frame to every active sink.
    ///
    /// A `Dropped` is the caller's repair signal; an `Overflow` drops that sink
    /// alone and is reported in [`CellDeltaFanout::overflowed`] rather than
    /// counted as a drop, because a sink that is gone owes nothing.
    pub fn send_frame_to_active(
        &mut self,
        frame: &CellGridFrame,
        timings: FrameTimings,
    ) -> CellDeltaFanout {
        let mut fanout = CellDeltaFanout::default();
        for sink in self.active_sinks() {
            if !self.is_active(sink.id()) {
                continue;
            }
            match sink.send_frame(channel_id, frame, timings) {
                CellSinkResult::Sent => fanout.accepted += 1,
                CellSinkResult::Dropped => fanout.dropped += 1,
                CellSinkResult::Overflow => self.drop_for_overflow(sink.id(), channel_id),
            }
        }
        fanout
    }

    /// One snapshot part for one sink.
    ///
    /// An overflow already dropped the sink, so the caller sees a
    /// non-advancing answer and abandons that cursor.
    pub fn send_part_to_sink(
        &mut self,
        channel_id: ChannelId,
        sink_id: &str,
        part: &CellGridSnapshotPart,
        timings: FrameTimings,
    ) -> CellSinkResult {
        let Some(entry) = self.sinks.get(sink_id) else {
            return CellSinkResult::Dropped;
        };
        let sink = entry.sink.clone();
        let answer = sink.send_snapshot_part(channel_id, part, timings);
        if answer == CellSinkResult::Overflow {
            self.drop_for_overflow(sink_id, channel_id);
            return CellSinkResult::Dropped;
        }
        answer
    }

    /// Drop one sink for an overflow and tell it exactly once.
    fn drop_for_overflow(&mut self, sink_id: &str, channel_id: ChannelId) {
        let Some(sink) = self.unregister(sink_id) else {
            return;
        };
        tracing::warn!(
            sink_id,
            %channel_id,
            "a cell sink overflowed its delivery queue and was dropped"
        );
        sink.on_overflow();
    }
}

/// Answer every whole-stream delivery question over the ACTIVE sinks.
pub fn aggregate_stream_delivery(
    registry: &CellSinkRegistry,
    deliveries: Option<&HashMap<String, StreamDelivery>>,
) -> StreamDeliveryAggregate {
    let mut aggregate = StreamDeliveryAggregate {
        baseline_ready: true,
        ..StreamDeliveryAggregate::default()
    };
    for entry in registry_active_entries(registry) {
        aggregate.active_sinks += 1;
        let Some(delivery) = deliveries.and_then(|all| all.get(entry.as_str())) else {
            // A sink with no delivery record has never been handed a full.
            aggregate.baseline_ready = false;
            continue;
        };
        if !delivery.baseline_ready {
            aggregate.baseline_ready = false;
        }
        if delivery.baseline_dirty {
            aggregate.baseline_dirty = true;
        }
        let Some(cursor) = delivery.cursor.as_ref() else {
            continue;
        };
        aggregate.snapshot_pending = true;
        aggregate.snapshot_part_count += cursor.parts.len();
        aggregate.remaining_snapshot_parts += cursor.parts.len().saturating_sub(cursor.next_part);
    }
    aggregate
}

/// The ids of the active sinks, in no particular order.
fn registry_active_entries(registry: &CellSinkRegistry) -> Vec<String> {
    registry
        .active_sinks()
        .iter()
        .map(|sink| sink.id().to_owned())
        .collect()
}
