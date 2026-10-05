//! The recording coordinator sink the terminal stream harness registers: every
//! full, delta and wire conversion it was handed, plus a switch that refuses
//! parked parts after the first. Split from the harness so the harness stays one
//! concept; registered by `super::Harness`.

use std::sync::Mutex;

use roost_proto::PbCellGridFrame;
use roost_protocol::cell::CellGridFrame;
use roost_protocol::cell::frame_chunks::CellGridSnapshotPart;
use roost_protocol::wire::brand::ChannelId;
use roost_worker::session::cell_sink::{CellSink, CellSinkResult, FrameTimings};

use super::held;

/// The coordinator sink, recording every full and delta it was offered.
#[derive(Default)]
pub struct RecordingSink {
    pub frames: Mutex<Vec<CellGridFrame>>,
    /// The wire conversion each frame arrived with, in the same order.
    pub wires: Mutex<Vec<PbCellGridFrame>>,
    pub parts: Mutex<Vec<CellGridSnapshotPart>>,
    pub refuse_parts_after_first: Mutex<bool>,
}

impl CellSink for RecordingSink {
    fn id(&self) -> &str {
        "coord"
    }
    fn send_frame(
        &self,
        _channel_id: ChannelId,
        frame: &CellGridFrame,
        wire: &PbCellGridFrame,
    ) -> CellSinkResult {
        held(&self.frames).push(frame.clone());
        held(&self.wires).push(wire.clone());
        CellSinkResult::Sent
    }
    fn send_snapshot_part(
        &self,
        _channel_id: ChannelId,
        part: &CellGridSnapshotPart,
        _timings: FrameTimings,
    ) -> CellSinkResult {
        let refuse = *held(&self.refuse_parts_after_first) && !held(&self.parts).is_empty();
        held(&self.parts).push(part.clone());
        if refuse {
            CellSinkResult::Dropped
        } else {
            CellSinkResult::Sent
        }
    }
}
