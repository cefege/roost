//! The worker-to-coordinator direction of the link codec: one mapping per arm
//! of `CoordWorkerUp`, in both directions. Called by `coord_worker_proto`.
//!
//! The arm numbers are not written here: each arm is the corresponding variant
//! of the generated oneof, so the field number is the one
//! `protocol/proto/roost/v1/worker_transport.proto` compiled into it. A number
//! this build does not know arrives as an unknown field on the wrapper and is
//! refused with that number in the message.

use roost_proto::buffa::MessageField;
use roost_proto::coord_worker_up::Frame;
use roost_proto::{
    CoordWorkerUp, WBinary, WHello, WPong, WRefreshJwt, WRpcError, WRpcOk, WSessionEvent,
};

use super::control_outcomes::{
    input_result_from_proto, input_result_to_proto, stream_result_from_proto,
    stream_result_to_proto,
};
use super::records::{
    agent_status_from_proto, agent_status_to_proto, channel_id, data_from_json, data_to_json,
    metadata_from_proto, metadata_to_proto, narrow_i64, update_progress_from_proto,
    update_progress_to_proto, widen_u64,
};
use super::{UPSTREAM, empty_arm, unknown_arm};
use crate::wire::brand::WorkerFp;
use crate::wire::coord_worker::CoordWorkerUpstream;
use crate::wire::coord_worker::{Binary, RefreshJwt};
use crate::wire::event_proto::{event_to_proto, proto_to_event};
use crate::{ProtocolError, ProtocolResult};

/// Map one union frame onto the generated oneof. The event and the status
/// carry values the domain union holds as `i64` and whole milliseconds, so this
/// is fallible: a value `uint64` cannot carry is refused, never wrapped.
pub(super) fn to_proto(frame: &CoordWorkerUpstream) -> ProtocolResult<CoordWorkerUp> {
    let arm = match frame {
        CoordWorkerUpstream::Hello {
            worker_fp,
            version,
            capabilities,
            process_epoch,
            ..
        } => Frame::Hello(Box::new(WHello {
            worker_fp: worker_fp.as_str().to_owned(),
            version: version.clone(),
            capabilities: capabilities.clone(),
            process_epoch: process_epoch.clone(),
            ..Default::default()
        })),
        CoordWorkerUpstream::Pong { ts, .. } => Frame::Pong(Box::new(WPong {
            ts: widen_u64("pong.ts", *ts)?,
            ..Default::default()
        })),
        CoordWorkerUpstream::Event {
            event, client_seq, ..
        } => Frame::Event(Box::new(WSessionEvent {
            event: MessageField::some(event_to_proto(event, 0)?),
            client_seq: *client_seq,
            ..Default::default()
        })),
        CoordWorkerUpstream::RpcOk {
            request_id, data, ..
        } => Frame::RpcOk(Box::new(WRpcOk {
            request_id: request_id.clone(),
            data_json: data_to_json(data)?,
            ..Default::default()
        })),
        CoordWorkerUpstream::RpcError {
            request_id,
            message,
            ..
        } => Frame::RpcError(Box::new(WRpcError {
            request_id: request_id.clone(),
            message: message.clone(),
            ..Default::default()
        })),
        CoordWorkerUpstream::Binary(binary) => Frame::Binary(Box::new(WBinary {
            channel_id: binary.channel_id.as_u32(),
            direction: u32::from(binary.direction),
            seq: binary.seq,
            data: binary.data.clone(),
            ..Default::default()
        })),
        CoordWorkerUpstream::RefreshJwt(refresh) => Frame::RefreshJwt(Box::new(WRefreshJwt {
            jwt: refresh.jwt.clone(),
            ..Default::default()
        })),
        CoordWorkerUpstream::CellGrid(grid) => Frame::CellGrid(Box::new((*grid).clone())),
        CoordWorkerUpstream::CellGridChunk(chunk) => {
            Frame::CellGridChunk(Box::new((*chunk).clone()))
        }
        CoordWorkerUpstream::InputResult(result) => {
            Frame::InputResult(Box::new(input_result_to_proto(result)?))
        }
        CoordWorkerUpstream::TerminalStreamResult(result) => {
            Frame::TerminalStreamResult(Box::new(stream_result_to_proto(result)?))
        }
        CoordWorkerUpstream::AgentStatus(status) => {
            Frame::AgentStatus(Box::new(agent_status_to_proto(status)?))
        }
        CoordWorkerUpstream::UpdateProgress(progress) => {
            Frame::UpdateProgress(Box::new(update_progress_to_proto(progress)?))
        }
        CoordWorkerUpstream::TerminalMetadata(metadata) => {
            Frame::TerminalMetadata(Box::new(metadata_to_proto(metadata)?))
        }
        CoordWorkerUpstream::TerminalViewState(state) => {
            Frame::TerminalViewState(Box::new((*state).clone()))
        }
        CoordWorkerUpstream::TerminalViewProjection(projection) => {
            Frame::TerminalViewProjection(Box::new((*projection).clone()))
        }
        CoordWorkerUpstream::LocalTerminalPeerAnswer(answer) => {
            Frame::LocalTerminalPeerAnswer(Box::new((*answer).clone()))
        }
        CoordWorkerUpstream::LocalTerminalPeerError(error) => {
            Frame::LocalTerminalPeerError(Box::new((*error).clone()))
        }
        CoordWorkerUpstream::LocalAttachmentPeerAnswer(answer) => {
            Frame::LocalAttachmentPeerAnswer(Box::new((*answer).clone()))
        }
        CoordWorkerUpstream::LocalAttachmentPeerError(error) => {
            Frame::LocalAttachmentPeerError(Box::new((*error).clone()))
        }
        CoordWorkerUpstream::AttachmentDirectStatus(status) => {
            Frame::AttachmentDirectStatus(Box::new((*status).clone()))
        }
        CoordWorkerUpstream::TerminalInputRouteResult(result) => {
            Frame::TerminalInputRouteResult(Box::new((*result).clone()))
        }
        CoordWorkerUpstream::TerminalTransportProbeResult(result) => {
            Frame::TerminalTransportProbeResult(Box::new((*result).clone()))
        }
        CoordWorkerUpstream::TerminalPipelineSnapshot(snapshot) => {
            Frame::TerminalPipelineSnapshot(Box::new((*snapshot).clone()))
        }
    };
    Ok(CoordWorkerUp {
        frame: Some(arm),
        ..Default::default()
    })
}

/// Map a decoded oneof back onto the union, refusing a frame whose arm this
/// build cannot name and re-running the union's own cross-field rules on what
/// the wire actually carried.
pub(super) fn from_proto(message: &CoordWorkerUp) -> ProtocolResult<CoordWorkerUpstream> {
    let Some(arm) = &message.frame else {
        return Err(first_unknown(message).unwrap_or_else(|| empty_arm(UPSTREAM)));
    };
    let frame = match arm {
        Frame::Hello(hello) => CoordWorkerUpstream::Hello {
            worker_fp: WorkerFp::try_from(hello.worker_fp.as_str())
                .map_err(|error| error.within("hello.worker_fp"))?,
            version: hello.version.clone(),
            capabilities: hello.capabilities.clone(),
            process_epoch: hello.process_epoch.clone(),
            trace_id: None,
        },
        Frame::Pong(pong) => CoordWorkerUpstream::Pong {
            ts: narrow_i64("pong.ts", pong.ts)?,
            trace_id: None,
        },
        Frame::Event(event) => {
            let proto = event
                .event
                .as_option()
                .ok_or_else(|| ProtocolError::new("event.event", "carries no event record"))?;
            let decoded = proto_to_event(proto)?.ok_or_else(|| {
                ProtocolError::new("event.event", "carries no event kind this build knows")
            })?;
            CoordWorkerUpstream::Event {
                event: decoded.event,
                client_seq: event.client_seq,
                trace_id: None,
            }
        }
        Frame::RpcOk(rpc) => CoordWorkerUpstream::RpcOk {
            request_id: rpc.request_id.clone(),
            data: data_from_json("rpc-ok.data", &rpc.data_json)?,
            trace_id: None,
        },
        Frame::RpcError(rpc) => CoordWorkerUpstream::RpcError {
            request_id: rpc.request_id.clone(),
            message: rpc.message.clone(),
            trace_id: None,
        },
        Frame::Binary(binary) => CoordWorkerUpstream::Binary(Binary {
            channel_id: channel_id("binary.channel_id", binary.channel_id)?,
            direction: u8::try_from(binary.direction)
                .map_err(|_| ProtocolError::new("binary.direction", "must fit in 8 bits"))?,
            data: binary.data.clone(),
            seq: binary.seq,
        }),
        Frame::RefreshJwt(refresh) => CoordWorkerUpstream::RefreshJwt(RefreshJwt {
            jwt: refresh.jwt.clone(),
        }),
        Frame::CellGrid(grid) => CoordWorkerUpstream::CellGrid((**grid).clone()),
        Frame::CellGridChunk(chunk) => CoordWorkerUpstream::CellGridChunk((**chunk).clone()),
        Frame::InputResult(result) => {
            CoordWorkerUpstream::InputResult(input_result_from_proto(result)?)
        }
        Frame::TerminalStreamResult(result) => {
            CoordWorkerUpstream::TerminalStreamResult(stream_result_from_proto(result)?)
        }
        Frame::AgentStatus(status) => {
            CoordWorkerUpstream::AgentStatus(crate::wire::coord_worker::AgentStatusFrame {
                status: agent_status_from_proto(status)?,
            })
        }
        Frame::UpdateProgress(progress) => {
            CoordWorkerUpstream::UpdateProgress(update_progress_from_proto(progress))
        }
        Frame::TerminalMetadata(metadata) => {
            CoordWorkerUpstream::TerminalMetadata(metadata_from_proto(metadata)?)
        }
        Frame::TerminalViewState(state) => {
            CoordWorkerUpstream::TerminalViewState((**state).clone())
        }
        Frame::TerminalViewProjection(projection) => {
            CoordWorkerUpstream::TerminalViewProjection((**projection).clone())
        }
        Frame::LocalTerminalPeerAnswer(answer) => {
            CoordWorkerUpstream::LocalTerminalPeerAnswer((**answer).clone())
        }
        Frame::LocalTerminalPeerError(error) => {
            CoordWorkerUpstream::LocalTerminalPeerError((**error).clone())
        }
        Frame::LocalAttachmentPeerAnswer(answer) => {
            CoordWorkerUpstream::LocalAttachmentPeerAnswer((**answer).clone())
        }
        Frame::LocalAttachmentPeerError(error) => {
            CoordWorkerUpstream::LocalAttachmentPeerError((**error).clone())
        }
        Frame::AttachmentDirectStatus(status) => {
            CoordWorkerUpstream::AttachmentDirectStatus((**status).clone())
        }
        Frame::TerminalInputRouteResult(result) => {
            CoordWorkerUpstream::TerminalInputRouteResult((**result).clone())
        }
        Frame::TerminalTransportProbeResult(result) => {
            CoordWorkerUpstream::TerminalTransportProbeResult((**result).clone())
        }
        Frame::TerminalPipelineSnapshot(snapshot) => {
            CoordWorkerUpstream::TerminalPipelineSnapshot((**snapshot).clone())
        }
    };
    frame.check()?;
    Ok(frame)
}

/// The first field number the wrapper could not place. On a oneof that is
/// exactly an arm from a build this one does not have, so it is refused by
/// number rather than skipped: a skipped arm here is a dropped terminal frame.
fn first_unknown(message: &CoordWorkerUp) -> Option<ProtocolError> {
    message
        .__buffa_unknown_fields
        .iter()
        .next()
        .map(|field| unknown_arm(UPSTREAM, field.number))
}
