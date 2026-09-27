//! The coordinator link's [`CellSink`]: the one receiver of cell frames that is
//! not a local browser socket. Owned by [`super::LinkLoop`], which drains it.
//! Depends on `roost_protocol`'s cell value model, on [`crate::outbox::Outbox`]
//! and on the link's own codec — and on nothing that calls back into emission.
//!
//! WHY THIS SINK HOLDS ITS OWN OUTBOX. [`CellSink::send_frame`] takes `&self`
//! and must not block, because the emitter holds a session's lock across the
//! fan-out and a blocking send would hold a PTY's lock across a socket write.
//! The link's own outbox is behind `&mut LinkLoop`, which the emitter cannot
//! reach. So the sink admits into a bounded outbox of its own and the link's
//! drain moves what is there onto [`crate::outbox::Lane::Terminal`] — bounded
//! twice, once per owner, and both bounds are the outbox's.
//!
//! The three answers are the trait's, and each is a fact rather than a policy. A
//! frame this link cannot take right now is [`CellSinkResult::Dropped`] and owes
//! a fresh baseline, because a receiver that missed a delta cannot reproduce the
//! screen. A frame that alone exceeds the byte cap is
//! [`CellSinkResult::Overflow`], because a queue holding it can never drain. A
//! queue that is merely full is neither: the link drains it, so the sink keeps
//! its registration and the frame is refused for this tick.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use roost_proto::buffa::MessageField;
use roost_proto::{WCellGrid, WCellGridChunk};
use roost_protocol::cell::CellGridFrame;
use roost_protocol::cell::frame_chunks::CellGridSnapshotPart;
use roost_protocol::cell::proto::cell_frame_to_proto;
use roost_protocol::wire::brand::ChannelId;
use roost_protocol::wire::coord_worker::CoordWorkerUpstream;

use crate::outbox::{AdmitError, Lane, Outbox, PENDING_BYTES_CAP, PENDING_CAP};
use crate::runtime::link_wire::LinkWire;
use crate::session::cell_sink::{COORD_CELL_SINK_ID, CellSink, CellSinkResult, FrameTimings};

use super::LinkLoop;

/// One clock, as the wire spells it.
///
/// The producer's clock is signed — an unset arrival is `0` and a clock that went
/// back is negative — and the wire's is not. A negative reading is not a time
/// before the epoch, it is a clock that could not be read, and `0` is what the
/// coordinator already reads as "not measured". Saturating is therefore the
/// truthful mapping rather than a clamp that invents a number.
fn measured_at(clock_ms: i64) -> u64 {
    u64::try_from(clock_ms).unwrap_or(0)
}

/// The session id a worker stamps into a cell frame it builds.
///
/// Empty on purpose, and not a gap: the coordinator fills it in from its own
/// channel-to-session map and explicitly adopts an empty one
/// (`apps/coord/src/terminal/screen/byte-hub.ts:195` — a NON-empty value that
/// disagrees is refused). A worker cannot know the coordinator's session id for
/// a channel, and inventing one would be a claim about a mapping the
/// coordinator owns.
pub const NO_SESSION_ID: &str = "";

/// The coordinator link, as a receiver of cell frames.
pub struct CoordinatorCellSink {
    /// The link's OWN codec, not a second one: a sink that encoded with
    /// different bytes than the drain writes would be a private wire definition,
    /// and every frame it produced would be refused by the coordinator.
    wire: Arc<dyn LinkWire>,
    queue: Mutex<Outbox>,
    /// Whether the link is attached and draining. A detached link is the
    /// `Dropped` case: the transport still exists, so the sink keeps its
    /// registration and still owes the coordinator a baseline.
    attached: Mutex<bool>,
}

impl std::fmt::Debug for CoordinatorCellSink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CoordinatorCellSink")
            .field("frames", &self.frame_count())
            .field("bytes", &self.byte_count())
            .field("attached", &self.is_attached())
            .finish()
    }
}

impl CoordinatorCellSink {
    pub fn new(wire: Arc<dyn LinkWire>) -> Self {
        Self {
            wire,
            queue: Mutex::new(Outbox::new(PENDING_CAP, PENDING_BYTES_CAP)),
            attached: Mutex::new(true),
        }
    }

    pub fn is_attached(&self) -> bool {
        self.attached.lock().map(|held| *held).unwrap_or(false)
    }

    /// Whether the coordinator link is attached enough to take cells.
    pub fn set_attached(&self, attached: bool) {
        if let Ok(mut held) = self.attached.lock() {
            *held = attached;
        }
    }

    pub fn frame_count(&self) -> usize {
        self.queue
            .lock()
            .map(|held| held.frame_count())
            .unwrap_or(0)
    }

    pub fn byte_count(&self) -> usize {
        self.queue.lock().map(|held| held.byte_count()).unwrap_or(0)
    }

    /// Move everything this sink holds onto the link's terminal lane, and return
    /// how many frames moved.
    ///
    /// Called from the link's drain, the only thing that writes to the socket.
    /// The count is returned so a drain log can say what it took and not only
    /// what it wrote.
    pub fn drain_into(&self, outbox: &mut Outbox, now: Instant) -> usize {
        let mut held = match self.queue.lock() {
            Ok(held) => held,
            // A poisoned queue is frames nobody can describe any more. The count
            // of zero against a link that is now behind is what the next full
            // repairs, so this is reported and not worked around.
            Err(_) => {
                tracing::error!("the coordinator cell queue is unusable; cells are being refused");
                return 0;
            }
        };
        let mut moved = 0usize;
        for frame in held.drain_all(now) {
            match outbox.admit(Lane::Terminal, frame.bytes, frame.label, now) {
                Ok(_) => moved += 1,
                Err(error) => {
                    // The link's lane is full, so the frames stay here and the
                    // sink keeps its registration: a full link reconnects, and
                    // dropping this sink would apply a local browser socket's
                    // answer to the transport every session on this machine
                    // depends on.
                    tracing::warn!(%error, "the coordinator link's terminal lane is full");
                    break;
                }
            }
        }
        moved
    }

    /// Encode with the link's codec and admit to this sink's own queue.
    ///
    /// Every refusal here is answered, never swallowed. An unencodable frame is
    /// a frame the coordinator never sees and the registry has to re-baseline a
    /// whole stream around; a frame left unaccounted for is a terminal that
    /// stops painting with no error anywhere.
    fn enqueue(
        &self,
        channel: ChannelId,
        frame: CoordWorkerUpstream,
        label: &str,
    ) -> CellSinkResult {
        let bytes = match self.wire.encode_upstream(&frame) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::error!(
                    channel = %channel,
                    kind = frame.kind(),
                    reason = %error,
                    "a cell frame did not encode, so the stream owes a fresh baseline"
                );
                return CellSinkResult::Dropped;
            }
        };
        let mut held = match self.queue.lock() {
            Ok(held) => held,
            Err(_) => return CellSinkResult::Overflow,
        };
        match held.admit(Lane::Terminal, bytes, label, Instant::now()) {
            Ok(_) => CellSinkResult::Sent,
            // A frame that alone exceeds the byte cap can never fit, so a queue
            // holding it can never drain. That IS the overflow case.
            Err(AdmitError::FrameTooLarge { .. }) => CellSinkResult::Overflow,
            // The queue is over its bounds but the link drains it, so the sink
            // stays registered and the frame is refused for this tick.
            Err(_) => CellSinkResult::Dropped,
        }
    }
}

impl CellSink for CoordinatorCellSink {
    fn id(&self) -> &str {
        COORD_CELL_SINK_ID
    }

    fn send_frame(
        &self,
        channel_id: ChannelId,
        frame: &CellGridFrame,
        timings: FrameTimings,
    ) -> CellSinkResult {
        if !self.is_attached() {
            return CellSinkResult::Dropped;
        }
        let mut proto = match cell_frame_to_proto(frame, NO_SESSION_ID) {
            Ok(proto) => proto,
            Err(error) => {
                // A frame that cannot be built is not this sink's to drop
                // outright: it is a stream that owes a baseline, and the registry
                // learns that from `Dropped`.
                tracing::error!(
                    channel = %channel_id,
                    reason = %error,
                    "a cell frame did not build, so the stream owes a fresh baseline"
                );
                return CellSinkResult::Dropped;
            }
        };
        // The producer measured these; a sink cannot, so they are stamped here
        // rather than left at the mapping's zero. The coordinator reads them to
        // attribute per-hop latency, and a zero reads as "this hop was free".
        proto.pty_out_ms = measured_at(timings.pty_out_ms);
        proto.worker_emit_ms = measured_at(timings.worker_emit_ms);
        self.enqueue(
            channel_id,
            CoordWorkerUpstream::CellGrid(WCellGrid {
                channel_id: channel_id.as_u32(),
                frame: MessageField::some(proto),
                ..Default::default()
            }),
            "cell-grid",
        )
    }

    fn send_snapshot_part(
        &self,
        channel_id: ChannelId,
        part: &CellGridSnapshotPart,
        timings: FrameTimings,
    ) -> CellSinkResult {
        if !self.is_attached() {
            return CellSinkResult::Dropped;
        }
        let frame = match part {
            CellGridSnapshotPart::Frame(proto) => {
                let mut proto = proto.clone();
                proto.pty_out_ms = measured_at(timings.pty_out_ms);
                proto.worker_emit_ms = measured_at(timings.worker_emit_ms);
                CoordWorkerUpstream::CellGrid(WCellGrid {
                    channel_id: channel_id.as_u32(),
                    frame: MessageField::some(proto),
                    ..Default::default()
                })
            }
            CellGridSnapshotPart::Chunk(chunk) => {
                CoordWorkerUpstream::CellGridChunk(WCellGridChunk {
                    channel_id: channel_id.as_u32(),
                    chunk: MessageField::some(chunk.clone()),
                    ..Default::default()
                })
            }
        };
        self.enqueue(channel_id, frame, "cell-grid-chunk")
    }

    fn on_overflow(&self) {
        // The queue cannot drain, so this sink's transport is the coordinator
        // link and the honest thing is to mark it detached: every later frame
        // answers `Dropped`, and a link that comes back is re-attached by the
        // drain rather than by a latch nobody is watching.
        self.set_attached(false);
        tracing::error!(
            "the coordinator cell queue cannot drain; cells are refused until the link drains it"
        );
    }
}

impl LinkLoop {
    /// Install the coordinator's cell sink, and return the one it replaced.
    pub fn attach_cell_sink(&mut self, sink: Arc<super::cell_sink::CoordinatorCellSink>) {
        self.cell_sink = Some(sink);
    }

    /// The coordinator's cell sink, if one is installed.
    pub fn cell_sink(&self) -> Option<&Arc<super::cell_sink::CoordinatorCellSink>> {
        self.cell_sink.as_ref()
    }

    /// Move the cell sink's held frames onto this link's terminal lane.
    ///
    /// Synchronous and called from the drain, because the sink is `&self` by the
    /// `CellSink` trait's own signature and the drain is the only place on this
    /// link that is already holding the tick.
    pub fn move_cell_frames_into(&mut self) -> usize {
        let Some(sink) = self.cell_sink.clone() else {
            return 0;
        };
        sink.drain_into(&mut self.outbox, Instant::now())
    }
}
