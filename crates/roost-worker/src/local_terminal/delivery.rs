//! LocalTerminal protobuf delivery shared by loopback and peer ports: encoding
//! one whole server frame, the input-result frames, and the view transport the
//! terminal view owner paints a local socket through. `super::sockets` chooses
//! the lane and the close policy; carrier queue ownership counts as a cell
//! delivery ACK. Ports `apps/worker/src/local-door/local-terminal-socket-delivery.ts`
//! and the `transport()` half of `local-terminal-socket.ts`.

use std::sync::{Arc, Weak};

use roost_proto::__buffa::oneof::local_terminal_server_frame::Frame as ServerFrame;
use roost_proto::buffa::Message;
use roost_proto::{
    InputAccepted, InputAmbiguous, InputCommand, InputRejected, LocalTerminalServerFrame,
    PbCellGridFrame, TerminalViewStateFrame,
};
use roost_protocol::cell::CellGridFrame;
use roost_protocol::cell::frame_chunks::CellGridSnapshotPart;
use roost_protocol::terminal_peer::peer::TerminalPeerPacketLane;
use roost_protocol::wire::brand::ChannelId;

use super::authority::PortSession;
use super::port::{PacketSendResult, TerminalPacketPort};
use super::sockets::LocalTerminalSockets;
use crate::session::cell_sink::{CellSinkResult, FrameTimings};
use crate::session::emit_frame::measured_at;
use crate::session::input_write::WorkerInputResult;
use crate::terminal_view::LocalViewTransport;

/// Encode one server frame whole and hand it to the carrier.
pub(super) fn send_local_terminal_frame(
    port: &dyn TerminalPacketPort,
    frame: ServerFrame,
    lane: TerminalPeerPacketLane,
) -> PacketSendResult {
    let envelope = LocalTerminalServerFrame {
        frame: Some(frame),
        ..Default::default()
    };
    port.send(envelope.encode_to_vec(), lane)
}

/// v2 `sendLocalTerminalInputResult`: the one frame a direct input is answered
/// with, stamped with the socket generation.
pub(super) fn input_result_frame(
    generation: u64,
    command: &InputCommand,
    result: &WorkerInputResult,
) -> ServerFrame {
    let session_id = command.session_id.clone();
    let input_seq = command.input_seq;
    match result {
        WorkerInputResult::Accepted { written_bytes } => ServerFrame::from(InputAccepted {
            session_id,
            input_seq,
            domain_generation: generation,
            written_bytes: *written_bytes,
            ..Default::default()
        }),
        WorkerInputResult::Rejected { reason } => ServerFrame::from(InputRejected {
            session_id,
            input_seq,
            domain_generation: generation,
            reason: reason.clone(),
            ..Default::default()
        }),
        WorkerInputResult::Ambiguous {
            written_bytes,
            reason,
        } => ServerFrame::from(InputAmbiguous {
            session_id,
            input_seq,
            domain_generation: generation,
            written_bytes: *written_bytes,
            reason: reason.clone(),
            ..Default::default()
        }),
    }
}

/// v2 `localTerminalCellDelivery`: a carrier that owns the frame (now or
/// queued) is a delivery; one that refuses it cannot drain, so the socket is
/// closed and the sink registry drops the sink.
pub(super) fn local_terminal_cell_delivery(
    result: PacketSendResult,
    close: impl FnOnce(),
) -> CellSinkResult {
    match result {
        PacketSendResult::Accepted | PacketSendResult::Backpressured => CellSinkResult::Sent,
        PacketSendResult::Refused => {
            close();
            CellSinkResult::Overflow
        }
    }
}

fn stamp(frame: &mut PbCellGridFrame, session_id: &str, timings: FrameTimings) {
    session_id.clone_into(&mut frame.session_id);
    frame.pty_out_ms = measured_at(timings.pty_out_ms);
    frame.worker_emit_ms = measured_at(timings.worker_emit_ms);
}

/// The view transport of one authenticated port (v2 `transport(session)`).
/// Called with a view-owner or emitter lock held, so every close it causes is
/// deferred: the port is fenced at once, the teardown runs on its own task.
#[derive(Debug)]
pub(super) struct PortViewTransport {
    pub(super) sockets: Weak<LocalTerminalSockets>,
    pub(super) session: Arc<PortSession>,
}

impl PortViewTransport {
    fn close_later(&self, reason: &'static str) {
        if let Some(sockets) = self.sockets.upgrade() {
            sockets.close_deferred(&self.session, reason);
        }
    }

    /// The session a channel carries, which every direct cell frame names
    /// because the carrier has no channel id.
    fn session_of(&self, channel_id: ChannelId) -> Option<String> {
        let sockets = self.sockets.upgrade()?;
        sockets.session_of_channel(channel_id)
    }

    fn send_cells(&self, frame: ServerFrame) -> CellSinkResult {
        let result =
            send_local_terminal_frame(self.session.port(), frame, TerminalPeerPacketLane::Terminal);
        local_terminal_cell_delivery(result, || self.close_later("local delivery overflow"))
    }
}

impl LocalViewTransport for PortViewTransport {
    fn send_view_state(&self, frame: TerminalViewStateFrame) {
        let result = send_local_terminal_frame(
            self.session.port(),
            ServerFrame::from(frame),
            TerminalPeerPacketLane::Terminal,
        );
        if result == PacketSendResult::Refused {
            self.close_later("local control delivery refused");
        }
    }

    fn send_cell_frame(
        &self,
        channel_id: ChannelId,
        _frame: &CellGridFrame,
        wire: &PbCellGridFrame,
    ) -> CellSinkResult {
        let Some(session_id) = self.session_of(channel_id) else {
            return CellSinkResult::Sent;
        };
        let mut proto = wire.clone();
        proto.session_id = session_id;
        self.send_cells(ServerFrame::from(proto))
    }

    fn send_snapshot_part(
        &self,
        channel_id: ChannelId,
        part: &CellGridSnapshotPart,
        timings: FrameTimings,
    ) -> CellSinkResult {
        let Some(session_id) = self.session_of(channel_id) else {
            return CellSinkResult::Sent;
        };
        let frame = match part {
            CellGridSnapshotPart::Frame(proto) => {
                let mut proto = proto.clone();
                stamp(&mut proto, &session_id, timings);
                ServerFrame::from(proto)
            }
            CellGridSnapshotPart::Chunk(chunk) => {
                let mut chunk = chunk.clone();
                if let Some(part) = chunk.part.as_option_mut() {
                    stamp(part, &session_id, timings);
                }
                ServerFrame::from(chunk)
            }
        };
        self.send_cells(frame)
    }

    fn on_overflow(&self) {
        self.close_later("local delivery overflow");
    }

    fn on_view_expired(&self) {
        self.close_later("terminal view lease expired");
    }
}
