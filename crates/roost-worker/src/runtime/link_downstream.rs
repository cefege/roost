//! The downstream side of the coordinator link: decode one socket frame, hand
//! it to [`super::downstream::Dispatcher`], and answer as its [`DownstreamLink`]
//! (replies, hello/event acknowledgements into the pump, browser commands,
//! pipeline state). Called by [`super::link_serve`] for every frame it reads.
//! Split from `runtime::link_drain`; the link side of the helloAck, browserCommand
//! and eventAck arms of v2 `apps/worker/src/transport/coord-link-downstream.ts`.

use std::sync::Arc;
use std::time::Instant;

use roost_protocol::wire::coord_worker::CoordWorkerUpstream;
use tokio_tungstenite::tungstenite::Message;

use crate::browser_commands::Command;
use crate::link_barrier::{Action, Barrier};
use crate::link_ports::LinkPipelineState;
use crate::outbox::Lane;
use crate::uplink::LinkFence;

use super::downstream::DownstreamLink;
use super::link_drain::{apply_to, push_upstream};
use super::link_loop::LinkLoop;
use super::link_wire::WireError;
use super::stop::LinkEnd;

/// v2's answer to a browser command whose frame does not parse.
pub const INVALID_BROWSER_COMMAND: &str = "invalid browser command";

/// Decode one downstream frame and dispatch it. Returns an end only for a stop.
pub(super) fn on_frame(loop_state: &mut LinkLoop, message: Message) -> Option<LinkEnd> {
    let Message::Binary(bytes) = message else {
        // This link carries binary protobuf. A text frame is something else
        // answering, and answering it is how a worker ends up speaking a second
        // protocol on the socket that carries its events.
        tracing::warn!("a non-binary frame arrived on the coordinator link; ignoring it");
        return None;
    };
    let frame = match loop_state.wire.decode_downstream(&bytes) {
        Ok(frame) => frame,
        // Refused on the ENVELOPE's correlation, so the coordinator's pending
        // entry fails now instead of timing out.
        Err(WireError::InvalidBrowserCommand { request_id, reason }) => {
            tracing::warn!(request_id, reason, "a browser command did not parse");
            let refusal = CoordWorkerUpstream::RpcError {
                request_id,
                message: INVALID_BROWSER_COMMAND.to_owned(),
                trace_id: None,
            };
            push_upstream(loop_state, &refusal, "rpc-error");
            return None;
        }
        Err(error) => {
            tracing::warn!(%error, "a downstream frame did not decode");
            return None;
        }
    };
    let dispatcher = Arc::clone(&loop_state.dispatcher);
    dispatcher.dispatch(frame, Instant::now(), loop_state);
    None
}

impl DownstreamLink for LinkLoop {
    fn reply(&mut self, frame: CoordWorkerUpstream) {
        push_upstream(self, &frame, frame.kind());
    }

    fn hello_acknowledged(&mut self, terminal_metadata_negotiated: bool) {
        self.terminal_metadata_negotiated = terminal_metadata_negotiated;
        if terminal_metadata_negotiated {
            let dropped = self.outbox.discard(Lane::RawMetadata);
            if dropped > 0 {
                tracing::debug!(
                    dropped,
                    "raw metadata dropped: compact metadata was negotiated"
                );
            }
        }
        let action = self.pump.on_hello_ack();
        apply_to(self, action);
    }

    // ONE SEQUENCE SPACE. The coordinator acknowledges the snapshot on the same
    // numbering a durable event uses; the pump knows which one it is waiting on.
    // The ROW is deleted in `drain`: this handler is synchronous and a SQLite
    // delete cannot be, so here only records WHICH sequence was answered.
    fn event_acknowledged(&mut self, client_seq: u64) -> bool {
        let was_live = self.pump.barrier().allows_live_traffic();
        let action = match self.pump.on_event_ack(client_seq) {
            Action::IgnoredAck { .. } => self.pump.on_snapshot_ack(client_seq),
            durable => durable,
        };
        self.note_durable_ack(client_seq);
        apply_to(self, action);
        !was_live && self.pump.barrier().allows_live_traffic()
    }

    fn browser_command(&mut self, command: Command, fence: LinkFence) {
        if let Err(command) = self.browser.offer(command, fence) {
            // The command came BACK, so the refusal is correlated on the
            // envelope the coordinator is waiting on.
            let request_id = command.request_id;
            tracing::warn!(
                request_id,
                "a browser command arrived with no command pump to run it"
            );
            let refusal = CoordWorkerUpstream::RpcError {
                request_id,
                message: super::link_loop::NO_SESSION_LAYER_REFUSAL.to_owned(),
                trace_id: None,
            };
            push_upstream(self, &refusal, "browser-command-refusal");
        }
    }

    fn pipeline_state(&self) -> LinkPipelineState {
        LinkPipelineState {
            queue_frames: u64::try_from(self.outbox.frame_count()).unwrap_or(u64::MAX),
            queue_bytes: u64::try_from(self.outbox.byte_count()).unwrap_or(u64::MAX),
            native_buffered_bytes: 0,
            attached: self.pump.barrier() != Barrier::Idle,
        }
    }
}
