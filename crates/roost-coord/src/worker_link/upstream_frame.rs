//! One message off the worker socket, decoded once and classified: the link's
//! own three arms (hello, pong, credential refresh), or a dispatcher frame with
//! its `FrameClass` and channel.
//!
//! Called by `worker_link::handshake` and `worker_link::connection` for every
//! data message. Decodes through `roost_protocol::proto_adapters::
//! coord_worker_proto::decode_upstream`, the link's one codec, and ports the
//! frame routing of `apps/coord/src/workers/worker-ws-handler.ts` (`message`)
//! with the arm table of `worker-conn.ts` (`handleUpstream`) and
//! `worker-frame-dispatch.ts` (`handleLiveFrame`).

use axum::body::Bytes;
use axum::extract::ws::Message;
use roost_protocol::ProtocolResult;
use roost_protocol::proto_adapters::coord_worker_proto::decode_upstream;
use roost_protocol::wire::coord_worker::CoordWorkerUpstream;
use roost_protocol::wire::{SessionEvent, WorkerFp};

use crate::worker_link::dispatch::{FrameClass, InboundFrame};

/// The negotiated facts a `WHello` carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelloFrame {
    /// The fingerprint the worker claims; admitted only when it is the one the
    /// upgrade authenticated.
    pub worker_fp: WorkerFp,
    /// The worker's process epoch, `None` when the hello carried none.
    pub process_epoch: Option<String>,
    /// Everything the worker advertised; the coordinator acknowledges a subset.
    pub capabilities: Vec<String>,
}

/// One decoded upstream frame, routed.
#[derive(Debug, Clone, PartialEq)]
pub enum LinkFrame {
    /// The forced first frame, and a protocol violation after it.
    Hello(HelloFrame),
    /// The reply to the coordinator's application ping.
    Pong { ts: i64 },
    /// An in-band replacement credential.
    RefreshJwt { jwt: String },
    /// Everything the frame dispatcher owns. Boxed because a decoded frame is
    /// several times the size of the three link arms.
    Dispatch(Box<InboundFrame>),
}

impl LinkFrame {
    /// The wire spelling of this frame's arm, for a log line.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Hello(_) => "hello",
            Self::Pong { .. } => "pong",
            Self::RefreshJwt { .. } => "refresh-jwt",
            Self::Dispatch(frame) => frame.frame.kind(),
        }
    }

    /// Whether this frame may pass before the generation's snapshot barrier.
    ///
    /// v2 admits exactly `hello`, `event`, `pong` and `refreshJwt` early
    /// (`worker-ws-handler.ts:222-234`): liveness and durable replay keep
    /// flowing, and nothing else bypasses the connection-level readiness gate.
    #[must_use]
    pub fn crosses_snapshot_barrier(&self) -> bool {
        match self {
            Self::Hello(_) | Self::Pong { .. } | Self::RefreshJwt { .. } => true,
            Self::Dispatch(frame) => frame.class == FrameClass::Durable,
        }
    }
}

/// The bytes a data message carries, or `None` for a control message.
///
/// A text message is decoded from its UTF-8 bytes, as v2's `TextEncoder`
/// path does; a binary message is used as it arrived, without a copy.
#[must_use]
pub fn message_bytes(message: Message) -> Option<Bytes> {
    match message {
        Message::Binary(bytes) => Some(bytes),
        Message::Text(text) => Some(Bytes::copy_from_slice(text.as_str().as_bytes())),
        Message::Ping(_) | Message::Pong(_) | Message::Close(_) => None,
    }
}

/// Decode one worker-link frame and route it.
pub fn decode_link_frame(bytes: &[u8]) -> ProtocolResult<LinkFrame> {
    decode_upstream(bytes).map(classify)
}

/// Route one decoded frame.
///
/// **Live** is everything the terminal and status hubs consume synchronously;
/// **Rpc** is every reply or progress record settling something the
/// coordinator holds open (v2 routes all of them through the pending tables or
/// their typed sinks); **Durable** is the event log alone.
///
/// The three link arms are copied out of a borrowed frame: a hello and a
/// credential refresh arrive once per connection or token lifetime, so the
/// copy costs nothing next to moving the dispatcher arms, which carry cells.
fn classify(upstream: CoordWorkerUpstream) -> LinkFrame {
    use CoordWorkerUpstream as Up;
    let (class, channel) = match &upstream {
        Up::Hello {
            worker_fp,
            capabilities,
            process_epoch,
            ..
        } => {
            return LinkFrame::Hello(HelloFrame {
                worker_fp: worker_fp.clone(),
                process_epoch: (!process_epoch.is_empty()).then(|| process_epoch.clone()),
                capabilities: capabilities.clone(),
            });
        }
        Up::Pong { ts, .. } => return LinkFrame::Pong { ts: *ts },
        Up::RefreshJwt(refresh) => {
            return LinkFrame::RefreshJwt {
                jwt: refresh.jwt.clone(),
            };
        }
        Up::Event { event, .. } => (FrameClass::Durable, announced_channel(event)),
        Up::CellGrid(grid) => (FrameClass::Live, grid.channel_id),
        Up::CellGridChunk(chunk) => (FrameClass::Live, chunk.channel_id),
        Up::Binary(binary) => (FrameClass::Live, binary.channel_id.as_u32()),
        Up::TerminalMetadata(metadata) => (FrameClass::Live, metadata.channel_id.as_u32()),
        Up::TerminalViewState(_) | Up::TerminalViewProjection(_) | Up::AgentStatus(_) => {
            (FrameClass::Live, 0)
        }
        Up::RpcOk { .. }
        | Up::RpcError { .. }
        | Up::InputResult(_)
        | Up::TerminalStreamResult(_)
        | Up::TerminalPipelineSnapshot(_)
        | Up::UpdateProgress(_)
        | Up::LocalTerminalPeerAnswer(_)
        | Up::LocalTerminalPeerError(_)
        | Up::LocalAttachmentPeerAnswer(_)
        | Up::LocalAttachmentPeerError(_)
        | Up::AttachmentDirectStatus(_)
        | Up::TerminalInputRouteResult(_)
        | Up::TerminalTransportProbeResult(_)
        | Up::AgentToolOutput(_)
        | Up::AgentToolResult(_) => (FrameClass::Rpc, 0),
    };
    LinkFrame::Dispatch(Box::new(InboundFrame {
        class,
        channel,
        frame: upstream,
    }))
}

/// The channel a durable event binds, which is the one its first cells arrive
/// on: `opened`'s channel and `respawned`'s new one (`worker-ws-handler.ts:
/// 281-303`). Every other event names no channel.
fn announced_channel(event: &SessionEvent) -> u32 {
    match event {
        SessionEvent::Opened { channel, .. } => channel.as_u32(),
        SessionEvent::Respawned { new_channel, .. } => new_channel.as_u32(),
        _ => 0,
    }
}
