//! The coordinator-to-worker direction of the link codec: one mapping per arm
//! of `CoordWorkerDown`, in both directions. Called by `coord_worker_proto`.
//!
//! The arm numbers are not written here: each arm is the corresponding variant
//! of the generated oneof, so the field number is the one
//! `protocol/proto/roost/v1/worker_transport.proto` compiled into it. A number
//! this build does not know arrives as an unknown field on the wrapper and is
//! refused with that number in the message.

use roost_proto::coord_worker_down::Frame;
use roost_proto::{
    CoordWorkerDown, DBinary, DBrowserCommand, DEventAck, DHelloAck, DPing,
    DTerminalSnapshotRequest,
};

use super::records::{channel_id, data_from_json, narrow_i64};
use super::{DOWNSTREAM, empty_arm, unknown_arm};
use crate::wire::brand::SessionId;
use crate::wire::control::ClientControlFrame;
use crate::wire::coord_worker::CoordWorkerDownstream;
use crate::wire::coord_worker::{Binary, EventAck, TerminalSnapshotRequest};
use crate::{ProtocolError, ProtocolResult};

/// Map one union frame onto the generated oneof.
pub(super) fn to_proto(frame: &CoordWorkerDownstream) -> ProtocolResult<CoordWorkerDown> {
    let arm = match frame {
        CoordWorkerDownstream::HelloAck { capabilities, .. } => {
            Frame::HelloAck(Box::new(DHelloAck {
                capabilities: capabilities.clone(),
                ..Default::default()
            }))
        }
        CoordWorkerDownstream::Ping { ts, .. } => Frame::Ping(Box::new(DPing {
            ts: super::records::widen_u64("ping.ts", *ts)?,
            ..Default::default()
        })),
        CoordWorkerDownstream::BrowserCommand {
            browser_id,
            viewer_id,
            request_id,
            frame,
            ..
        } => Frame::BrowserCommand(Box::new(DBrowserCommand {
            browser_id: browser_id.clone(),
            viewer_id: viewer_id.clone(),
            request_id: request_id.clone(),
            frame_json: serde_json::to_string(frame)
                .map_err(|error| ProtocolError::new("browser-command.frame", error.to_string()))?,
            ..Default::default()
        })),
        CoordWorkerDownstream::EventAck(ack) => Frame::EventAck(Box::new(DEventAck {
            client_seq: ack.client_seq,
            ..Default::default()
        })),
        CoordWorkerDownstream::Binary(binary) => Frame::Binary(Box::new(DBinary {
            channel_id: binary.channel_id.as_u32(),
            direction: u32::from(binary.direction),
            data: binary.data.clone(),
            ..Default::default()
        })),
        CoordWorkerDownstream::InputRequest(request) => {
            Frame::InputRequest(Box::new((*request).clone()))
        }
        CoordWorkerDownstream::TerminalStreamState(state) => {
            Frame::TerminalStreamState(Box::new((*state).clone()))
        }
        CoordWorkerDownstream::TerminalSnapshotRequest(request) => {
            Frame::TerminalSnapshotRequest(Box::new(DTerminalSnapshotRequest {
                session_id: request.session_id.as_str().to_owned(),
                stream_id: request.stream_id.clone(),
                ..Default::default()
            }))
        }
        CoordWorkerDownstream::TerminalPipelineSnapshot(request) => {
            Frame::TerminalPipelineSnapshot(Box::new((*request).clone()))
        }
        CoordWorkerDownstream::TerminalViewRelay(relay) => {
            Frame::TerminalViewRelay(Box::new((*relay).clone()))
        }
        CoordWorkerDownstream::TerminalViewSocketClosed(closed) => {
            Frame::TerminalViewSocketClosed(Box::new((*closed).clone()))
        }
        CoordWorkerDownstream::LocalTerminalGrant(grant) => {
            Frame::LocalTerminalGrant(Box::new((*grant).clone()))
        }
        CoordWorkerDownstream::LocalTerminalGrantRevoke(revoke) => {
            Frame::LocalTerminalGrantRevoke(Box::new((*revoke).clone()))
        }
        CoordWorkerDownstream::LocalTerminalPeerOffer(offer) => {
            Frame::LocalTerminalPeerOffer(Box::new((*offer).clone()))
        }
        CoordWorkerDownstream::LocalTerminalPeerCancel(cancel) => {
            Frame::LocalTerminalPeerCancel(Box::new((*cancel).clone()))
        }
        CoordWorkerDownstream::LocalAttachmentPeerOffer(offer) => {
            Frame::LocalAttachmentPeerOffer(Box::new((*offer).clone()))
        }
        CoordWorkerDownstream::LocalAttachmentPeerCancel(cancel) => {
            Frame::LocalAttachmentPeerCancel(Box::new((*cancel).clone()))
        }
        CoordWorkerDownstream::LocalAttachmentGrant(grant) => {
            Frame::LocalAttachmentGrant(Box::new((*grant).clone()))
        }
        CoordWorkerDownstream::LocalAttachmentGrantRevoke(revoke) => {
            Frame::LocalAttachmentGrantRevoke(Box::new((*revoke).clone()))
        }
        CoordWorkerDownstream::AttachmentDirectStatusRequest(request) => {
            Frame::AttachmentDirectStatusRequest(Box::new((*request).clone()))
        }
        CoordWorkerDownstream::AttachmentChunk(chunk) => {
            Frame::AttachmentChunk(Box::new((*chunk).clone()))
        }
        CoordWorkerDownstream::TerminalInputRouteClaim(claim) => {
            Frame::TerminalInputRouteClaim(Box::new((*claim).clone()))
        }
        CoordWorkerDownstream::TerminalTransportProbe(probe) => {
            Frame::TerminalTransportProbe(Box::new((*probe).clone()))
        }
        CoordWorkerDownstream::TerminalDirectRetire(retire) => {
            Frame::TerminalDirectRetire(Box::new((*retire).clone()))
        }
        CoordWorkerDownstream::CoordMovePrepare(prepare) => {
            Frame::CoordMovePrepare(Box::new((*prepare).clone()))
        }
        CoordWorkerDownstream::CoordMoveSnapshotStart(start) => {
            Frame::CoordMoveSnapshotStart(Box::new((*start).clone()))
        }
        CoordWorkerDownstream::CoordMoveSnapshotChunk(chunk) => {
            Frame::CoordMoveSnapshotChunk(Box::new((*chunk).clone()))
        }
        CoordWorkerDownstream::CoordRelocate(relocate) => {
            Frame::CoordRelocate(Box::new((*relocate).clone()))
        }
        CoordWorkerDownstream::UpdateBroker(broker) => {
            Frame::UpdateBroker(Box::new((*broker).clone()))
        }
        CoordWorkerDownstream::KeeperUpdatePrepare(prepare) => {
            Frame::KeeperUpdatePrepare(Box::new((*prepare).clone()))
        }
        CoordWorkerDownstream::AgentPrompt(prompt) => {
            Frame::AgentPrompt(Box::new((*prompt).clone()))
        }
    };
    Ok(CoordWorkerDown {
        frame: Some(arm),
        ..Default::default()
    })
}

/// Map a decoded oneof back onto the union, refusing a frame whose arm this
/// build cannot name and re-running the union's own rules on what arrived.
pub(super) fn from_proto(message: &CoordWorkerDown) -> ProtocolResult<CoordWorkerDownstream> {
    let Some(arm) = &message.frame else {
        return Err(first_unknown(message).unwrap_or_else(|| empty_arm(DOWNSTREAM)));
    };
    let frame = match arm {
        Frame::HelloAck(ack) => CoordWorkerDownstream::HelloAck {
            capabilities: ack.capabilities.clone(),
            trace_id: None,
        },
        Frame::Ping(ping) => CoordWorkerDownstream::Ping {
            ts: narrow_i64("ping.ts", ping.ts)?,
            trace_id: None,
        },
        Frame::BrowserCommand(command) => CoordWorkerDownstream::BrowserCommand {
            browser_id: command.browser_id.clone(),
            viewer_id: command.viewer_id.clone(),
            request_id: command.request_id.clone(),
            // The relayed frame goes through its own admission path: relaying a
            // browser command is not permission to relay a frame the browser
            // could not have sent the worker directly.
            frame: ClientControlFrame::parse(data_from_json(
                "browser-command.frame",
                &command.frame_json,
            )?)?,
            trace_id: None,
        },
        Frame::EventAck(ack) => CoordWorkerDownstream::EventAck(EventAck {
            client_seq: ack.client_seq,
        }),
        Frame::Binary(binary) => CoordWorkerDownstream::Binary(Binary {
            channel_id: channel_id("binary.channel_id", binary.channel_id)?,
            direction: u8::try_from(binary.direction)
                .map_err(|_| ProtocolError::new("binary.direction", "must fit in 8 bits"))?,
            data: binary.data.clone(),
            seq: 0,
        }),
        Frame::InputRequest(request) => CoordWorkerDownstream::InputRequest((**request).clone()),
        Frame::TerminalStreamState(state) => {
            CoordWorkerDownstream::TerminalStreamState((**state).clone())
        }
        Frame::TerminalSnapshotRequest(request) => {
            CoordWorkerDownstream::TerminalSnapshotRequest(TerminalSnapshotRequest {
                session_id: SessionId::try_from(request.session_id.as_str())
                    .map_err(|error| error.within("terminal-snapshot-request.session_id"))?,
                stream_id: request.stream_id.clone(),
            })
        }
        Frame::TerminalPipelineSnapshot(request) => {
            CoordWorkerDownstream::TerminalPipelineSnapshot((**request).clone())
        }
        Frame::TerminalViewRelay(relay) => {
            CoordWorkerDownstream::TerminalViewRelay((**relay).clone())
        }
        Frame::TerminalViewSocketClosed(closed) => {
            CoordWorkerDownstream::TerminalViewSocketClosed((**closed).clone())
        }
        Frame::LocalTerminalGrant(grant) => {
            CoordWorkerDownstream::LocalTerminalGrant((**grant).clone())
        }
        Frame::LocalTerminalGrantRevoke(revoke) => {
            CoordWorkerDownstream::LocalTerminalGrantRevoke((**revoke).clone())
        }
        Frame::LocalTerminalPeerOffer(offer) => {
            CoordWorkerDownstream::LocalTerminalPeerOffer((**offer).clone())
        }
        Frame::LocalTerminalPeerCancel(cancel) => {
            CoordWorkerDownstream::LocalTerminalPeerCancel((**cancel).clone())
        }
        Frame::LocalAttachmentPeerOffer(offer) => {
            CoordWorkerDownstream::LocalAttachmentPeerOffer((**offer).clone())
        }
        Frame::LocalAttachmentPeerCancel(cancel) => {
            CoordWorkerDownstream::LocalAttachmentPeerCancel((**cancel).clone())
        }
        Frame::LocalAttachmentGrant(grant) => {
            CoordWorkerDownstream::LocalAttachmentGrant((**grant).clone())
        }
        Frame::LocalAttachmentGrantRevoke(revoke) => {
            CoordWorkerDownstream::LocalAttachmentGrantRevoke((**revoke).clone())
        }
        Frame::AttachmentDirectStatusRequest(request) => {
            CoordWorkerDownstream::AttachmentDirectStatusRequest((**request).clone())
        }
        Frame::AttachmentChunk(chunk) => CoordWorkerDownstream::AttachmentChunk((**chunk).clone()),
        Frame::TerminalInputRouteClaim(claim) => {
            CoordWorkerDownstream::TerminalInputRouteClaim((**claim).clone())
        }
        Frame::TerminalTransportProbe(probe) => {
            CoordWorkerDownstream::TerminalTransportProbe((**probe).clone())
        }
        Frame::TerminalDirectRetire(retire) => {
            CoordWorkerDownstream::TerminalDirectRetire((**retire).clone())
        }
        Frame::CoordMovePrepare(prepare) => {
            CoordWorkerDownstream::CoordMovePrepare((**prepare).clone())
        }
        Frame::CoordMoveSnapshotStart(start) => {
            CoordWorkerDownstream::CoordMoveSnapshotStart((**start).clone())
        }
        Frame::CoordMoveSnapshotChunk(chunk) => {
            CoordWorkerDownstream::CoordMoveSnapshotChunk((**chunk).clone())
        }
        Frame::CoordRelocate(relocate) => {
            CoordWorkerDownstream::CoordRelocate((**relocate).clone())
        }
        Frame::UpdateBroker(broker) => CoordWorkerDownstream::UpdateBroker((**broker).clone()),
        Frame::KeeperUpdatePrepare(prepare) => {
            CoordWorkerDownstream::KeeperUpdatePrepare((**prepare).clone())
        }
        Frame::AgentPrompt(prompt) => CoordWorkerDownstream::AgentPrompt((**prompt).clone()),
    };
    frame.check()?;
    Ok(frame)
}

/// The first field number the wrapper could not place, refused by number
/// rather than skipped.
fn first_unknown(message: &CoordWorkerDown) -> Option<ProtocolError> {
    message
        .__buffa_unknown_fields
        .iter()
        .next()
        .map(|field| unknown_arm(DOWNSTREAM, field.number))
}
