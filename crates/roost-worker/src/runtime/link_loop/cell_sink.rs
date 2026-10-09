//! The coordinator link's [`CellSink`]: v2 `main.ts:211-217`'s `"coord"` sink
//! over `transport/coord-link-outbox.ts` `sendCellGrid`/`sendCellGridChunk`.
//! `runtime::cell_cadence` registers it with the emitter and suspends/resumes it
//! across the link's lifecycle; the link drain moves its queue onto the Terminal
//! lane and consumes [`CoordinatorCellSink::take_writable_owed`] (v2
//! `maybeNotifyWritable`). Depends on the link codec and `crate::outbox` only.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use roost_proto::buffa::MessageField;
use roost_proto::{PbCellGridFrame, WCellGrid, WCellGridChunk};
use roost_protocol::cell::CellGridFrame;
use roost_protocol::cell::frame_chunks::CellGridSnapshotPart;
use roost_protocol::wire::brand::ChannelId;
use roost_protocol::wire::coord_worker::CoordWorkerUpstream;
use tokio::sync::Notify;

use crate::outbox::{Lane, Outbox, PENDING_BYTES_CAP, PENDING_CAP};
use crate::runtime::link_wire::LinkWire;
use crate::session::cell_sink::{COORD_CELL_SINK_ID, CellSink, CellSinkResult, FrameTimings};
use crate::session::emit_frame::measured_at;

use super::LinkLoop;

/// The coordinator link, as a receiver of cell frames.
///
/// It holds its own bounded queue because [`CellSink::send_frame`] is `&self`
/// and must not block (the emitter holds a record lock across the fan-out),
/// while the link's outbox is behind `&mut LinkLoop`. Its answers are v2's
/// `TerminalCellSendResult`: `Sent` or `Dropped`, never `Overflow` — the
/// coordinator's transport is not a local socket the registry may close, and
/// dropping this sink would darken every session on the machine.
pub struct CoordinatorCellSink {
    /// The link's OWN codec: a sink encoding with different bytes than the
    /// drain writes would be a private wire definition.
    wire: Arc<dyn LinkWire>,
    queue: Mutex<Outbox>,
    /// v2 `linkReady && nativeWriter.isAttached()`: false from a socket's open
    /// until its hello-ack, and after it detaches.
    attached: AtomicBool,
    /// v2 `writableNotificationPending`: a frame was refused, so the session
    /// layer is owed an `on_writable` once the link has room again.
    writable_owed: AtomicBool,
    /// The link's wake, installed with this sink. A queued frame that waited
    /// for the drain's tick instead reached the browser up to a tick late,
    /// behind the input acknowledgements that are written at once — which is
    /// what made predictive echo contradict its own correct guesses.
    link_wake: Mutex<Option<Arc<Notify>>>,
}

impl std::fmt::Debug for CoordinatorCellSink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CoordinatorCellSink")
            .field("frames", &self.frame_count())
            .field("bytes", &self.byte_count())
            .field("attached", &self.is_attached())
            .field("writable_owed", &self.writable_owed.load(Ordering::Acquire))
            .finish()
    }
}

impl CoordinatorCellSink {
    /// A sink that is detached until the lifecycle attaches it at hello-ack.
    pub fn new(wire: Arc<dyn LinkWire>) -> Self {
        Self {
            wire,
            queue: Mutex::new(Outbox::new(PENDING_CAP, PENDING_BYTES_CAP)),
            attached: AtomicBool::new(false),
            writable_owed: AtomicBool::new(false),
            link_wake: Mutex::new(None),
        }
    }

    /// Wake `link` whenever a frame is queued for it (v2 `sendCellGrid`
    /// writes through at once).
    pub fn wake_link_with(&self, link: Arc<Notify>) {
        if let Ok(mut held) = self.link_wake.lock() {
            *held = Some(link);
        }
    }

    pub fn is_attached(&self) -> bool {
        self.attached.load(Ordering::Acquire)
    }

    /// Whether the coordinator link can take cells for its current generation.
    pub fn set_attached(&self, attached: bool) {
        if self.attached.swap(attached, Ordering::AcqRel) != attached {
            tracing::info!(attached, "the coordinator cell sink's attachment changed");
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

    /// Whether a refused frame still owes the session layer an `on_writable`
    /// (v2 `writableNotificationPending`); the link drain holds its controls
    /// behind it.
    pub fn writable_owed(&self) -> bool {
        self.writable_owed.load(Ordering::Acquire)
    }

    /// Consume the owed writable notification. The link drain calls it once
    /// the link is live and its terminal lane is empty, so the flag is only
    /// taken when the notification can actually be acted on.
    pub fn take_writable_owed(&self) -> bool {
        self.writable_owed.swap(false, Ordering::AcqRel)
    }

    /// Drop every queued frame: they describe a socket generation that is gone,
    /// and v2 never queued a cell across a detach. Returns how many were dropped.
    pub fn discard_queued(&self) -> usize {
        let Ok(mut held) = self.queue.lock() else {
            return 0;
        };
        let dropped = held.discard(Lane::Terminal);
        if dropped > 0 {
            tracing::info!(dropped, "stale coordinator cell frames were discarded");
        }
        dropped
    }

    /// Move everything this sink holds onto the link's terminal lane, and return
    /// how many frames moved. Called from the link's drain only.
    pub fn drain_into(&self, outbox: &mut Outbox, now: Instant) -> usize {
        let Ok(mut held) = self.queue.lock() else {
            tracing::error!("the coordinator cell queue is unusable; cells are being refused");
            return 0;
        };
        let mut moved = 0usize;
        while let Some(frame) = held.drain_one(now) {
            let label = frame.label.clone();
            match outbox.admit(Lane::Terminal, frame.bytes, label, now) {
                Ok(_) => moved += 1,
                Err(error) => {
                    // The link's lane is full; the refused frame is lost, which
                    // is a dropped cell and owes the sink's streams a repair.
                    tracing::warn!(%error, "the coordinator link's terminal lane is full");
                    self.writable_owed.store(true, Ordering::Release);
                    break;
                }
            }
        }
        moved
    }

    /// A refused frame: v2 sets `writableNotificationPending` on every drop.
    fn refuse(&self) -> CellSinkResult {
        self.writable_owed.store(true, Ordering::Release);
        CellSinkResult::Dropped
    }

    /// Encode with the link's codec and admit to this sink's own queue.
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
                return self.refuse();
            }
        };
        let Ok(mut held) = self.queue.lock() else {
            return self.refuse();
        };
        match held.admit(Lane::Terminal, bytes, label, Instant::now()) {
            Ok(_) => {
                drop(held);
                if let Ok(link) = self.link_wake.lock()
                    && let Some(link) = link.as_ref()
                {
                    link.notify_one();
                }
                CellSinkResult::Sent
            }
            Err(error) => {
                tracing::debug!(channel = %channel, %error, "the coordinator cell queue refused a frame");
                drop(held);
                self.refuse()
            }
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
        _frame: &CellGridFrame,
        wire: &PbCellGridFrame,
    ) -> CellSinkResult {
        if !self.is_attached() {
            return self.refuse();
        }
        // The session id stays empty, so the coordinator adopts its own.
        let proto = wire.clone();
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
            return self.refuse();
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
}

impl LinkLoop {
    /// Install the coordinator's cell sink. The SAME `Arc` is registered with
    /// the emitter by `runtime::cell_cadence::CellCadence::spawn`.
    pub fn attach_cell_sink(&mut self, sink: Arc<CoordinatorCellSink>) {
        sink.wake_link_with(Arc::clone(&self.wake));
        self.cell_sink = Some(sink);
    }

    /// The coordinator's cell sink, if one is installed.
    pub fn cell_sink(&self) -> Option<&Arc<CoordinatorCellSink>> {
        self.cell_sink.as_ref()
    }

    /// Move the cell sink's held frames onto this link's terminal lane, once
    /// every frame already waiting in the uplink is admitted: the biased select
    /// serves the cell wake first, and a baseline written ahead of the view
    /// decision announcing its stream is one the coordinator drops.
    pub fn move_cell_frames_into(&mut self) -> usize {
        while let Some(frame) = self.uplink.try_recv() {
            crate::runtime::link_drain::admit_uplink(self, frame);
        }
        let Some(sink) = self.cell_sink.clone() else {
            return 0;
        };
        sink.drain_into(&mut self.outbox, Instant::now())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::Arc;

    use futures_util::FutureExt as _;
    use roost_protocol::cell::types::{CellGridFrame, CellRow, MouseTracking};
    use roost_protocol::wire::brand::ChannelId;
    use tokio::sync::Notify;

    use super::CoordinatorCellSink;
    use crate::runtime::link_wire::ProtoLinkWire;
    use crate::session::cell_sink::{CellSink, CellSinkResult, FrameTimings};
    use crate::session::emit_frame::frame_wire;

    fn full_frame() -> CellGridFrame {
        CellGridFrame {
            stream_id: "00000000-0000-4000-8000-0000000000a1".to_owned(),
            grid_epoch: "epoch-1".to_owned(),
            cols: 80,
            rows: 2,
            cursor_row: 0,
            cursor_col: 0,
            cursor_visible: true,
            alt_screen: false,
            cursor_keys_app: false,
            bracketed_paste: false,
            mouse_tracking: MouseTracking::None,
            mouse_sgr: false,
            focus_events: false,
            kitty_keyboard_flags: 0,
            full: true,
            viewport_rows: (0..2)
                .map(|index| CellRow {
                    index,
                    mark: 0,
                    spans: Arc::from(Vec::new()),
                })
                .collect(),
            scrollback_rows: Vec::new(),
            scrollback_append: Vec::new(),
            scrollback_total: 0,
            sb_base: 0,
            base_seq: 0,
            seq: 1,
            image_placements: Some(roost_protocol::cell::no_image_placements()),
        }
    }

    /// The drain's tick is a backstop, not the cadence: a keystroke's echo
    /// frame that waited for it reached the browser behind later input
    /// acknowledgements.
    #[test]
    fn a_queued_cell_frame_wakes_its_link_and_a_refused_one_does_not() {
        let sink = CoordinatorCellSink::new(Arc::new(ProtoLinkWire));
        let link = Arc::new(Notify::new());
        sink.wake_link_with(Arc::clone(&link));
        let channel = ChannelId::try_from(4_i64).unwrap();
        let timings = FrameTimings {
            pty_out_ms: 1,
            worker_emit_ms: 2,
        };
        let frame = full_frame();
        let wire = frame_wire(&frame, timings).unwrap();

        assert_eq!(
            sink.send_frame(channel, &frame, &wire),
            CellSinkResult::Dropped
        );
        assert!(
            link.notified().now_or_never().is_none(),
            "a refused frame has nothing for the link to drain"
        );

        sink.set_attached(true);
        assert_eq!(
            sink.send_frame(channel, &frame, &wire),
            CellSinkResult::Sent
        );
        assert!(
            link.notified().now_or_never().is_some(),
            "a queued frame wakes the link at once"
        );
    }
}
