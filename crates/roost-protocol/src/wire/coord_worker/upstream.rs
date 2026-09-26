//! The worker-to-coordinator direction of the worker link, one variant per arm
//! of the protobuf `CoordWorkerUp` oneof. Called by the link loop for every
//! frame the worker writes, and by `CoordWorkerUpstream::parse` for the JSON
//! form. Depends on `wire::event` for the durable event union and on
//! `coord_worker::payloads` for the frame records.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::kind_of;
use super::payloads::{
    AgentStatusFrame, Binary, InputResult, RefreshJwt, TerminalMetadata, TerminalStreamResult,
    UpdateProgress, WAttachmentDirectStatus, WCellGrid, WCellGridChunk, WLocalAttachmentPeerAnswer,
    WLocalAttachmentPeerError, WLocalTerminalPeerAnswer, WLocalTerminalPeerError,
    WTerminalInputRouteResult, WTerminalPipelineSnapshot, WTerminalTransportProbeResult,
    WTerminalViewProjection, WTerminalViewState,
};
use crate::validate::nonnegative;
use crate::wire::brand::{TraceId, WorkerFp};
use crate::wire::event::SessionEvent;
use crate::{ProtocolError, ProtocolResult};

/// A frame travelling from the worker to the coordinator.
///
/// `kind` is the discriminant, and every arm the protobuf oneof declares is
/// present here exactly once so a decode is total: a frame this build cannot
/// name is refused by the codec rather than skipped, because a skipped arm on
/// the terminal path is a dropped frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum CoordWorkerUpstream {
    /// The first frame after the socket opens, answered with `hello-ack`.
    /// `capabilities` is what the coordinator negotiates view ownership from,
    /// and `process_epoch` is what fences a superseded connection generation.
    #[serde(rename = "hello")]
    Hello {
        worker_fp: WorkerFp,
        version: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        capabilities: Vec<String>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        process_epoch: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    /// The keepalive reply to a downstream `ping`; the coordinator reads the
    /// round trip as liveness.
    #[serde(rename = "pong")]
    Pong {
        ts: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    /// One record of the session event log, in order, stamped with the
    /// worker's monotonic `client_seq`. The coordinator appends it in the same
    /// transaction its projections use, so an event is never visible to a
    /// reader before the rows it implies, and acks that exact sequence.
    #[serde(rename = "event")]
    Event {
        event: SessionEvent,
        #[serde(default)]
        client_seq: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    /// The reply to a downstream `browser-command`, correlated by the
    /// coordinator's own `request_id` so the reply reaches the browser that
    /// asked rather than whichever browser asked next.
    #[serde(rename = "rpc-ok")]
    RpcOk {
        request_id: String,
        data: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    #[serde(rename = "rpc-error")]
    RpcError {
        request_id: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    /// PTY bytes as the worker saw them, on their own channel id.
    #[serde(rename = "binary")]
    Binary(Binary),
    /// An in-band replacement JWT, sent before the current one expires.
    #[serde(rename = "refresh-jwt")]
    RefreshJwt(RefreshJwt),
    /// One whole cell grid, or one bounded part of an oversized one. Carried
    /// as the generated message because the value model has no serde form and
    /// `cell::proto` already owns the mapping in both directions.
    #[serde(rename = "cell-grid")]
    CellGrid(WCellGrid),
    #[serde(rename = "cell-grid-chunk")]
    CellGridChunk(WCellGridChunk),
    /// The truthful outcome of a terminal-control request. Only a
    /// `PreWrite` rejection is retry-safe, so the phase travels with it.
    #[serde(rename = "input-result")]
    InputResult(InputResult),
    /// The outcome of one terminal-stream state request, carrying the
    /// coordinator's already-aggregated geometry and the generation addressed.
    #[serde(rename = "terminal-stream-result")]
    TerminalStreamResult(TerminalStreamResult),
    /// A volatile agent-status observation. Never journal-backed: the next one
    /// replaces it, and a stale one is fenced by the status epoch.
    #[serde(rename = "agent-status")]
    AgentStatus(AgentStatusFrame),
    /// Journal-backed progress of one update job on this host, replayed after
    /// a reconnect so an upgrade in flight resumes instead of restarting.
    #[serde(rename = "update-progress")]
    UpdateProgress(UpdateProgress),
    /// Compact semantic terminal metadata: no PTY bytes, so a title or
    /// activity change costs a fixed frame however long the line is.
    #[serde(rename = "terminal-metadata")]
    TerminalMetadata(TerminalMetadata),
    /// One worker-owned view decision, addressed back to the browser socket the
    /// coordinator relayed the command from.
    #[serde(rename = "terminal-view-state")]
    TerminalViewState(WTerminalViewState),
    /// A session's whole viewer membership as the worker aggregated it, so the
    /// coordinator can answer presence and diagnostics without owning it.
    #[serde(rename = "terminal-view-projection")]
    TerminalViewProjection(WTerminalViewProjection),
    #[serde(rename = "local-terminal-peer-answer")]
    LocalTerminalPeerAnswer(WLocalTerminalPeerAnswer),
    #[serde(rename = "local-terminal-peer-error")]
    LocalTerminalPeerError(WLocalTerminalPeerError),
    #[serde(rename = "local-attachment-peer-answer")]
    LocalAttachmentPeerAnswer(WLocalAttachmentPeerAnswer),
    #[serde(rename = "local-attachment-peer-error")]
    LocalAttachmentPeerError(WLocalAttachmentPeerError),
    #[serde(rename = "attachment-direct-status")]
    AttachmentDirectStatus(WAttachmentDirectStatus),
    #[serde(rename = "terminal-input-route-result")]
    TerminalInputRouteResult(WTerminalInputRouteResult),
    #[serde(rename = "terminal-transport-probe-result")]
    TerminalTransportProbeResult(WTerminalTransportProbeResult),
    /// Content-free evidence for one bounded terminal-pipeline sample: the
    /// worker reports only source-owned ids, counters and ages.
    #[serde(rename = "terminal-pipeline-snapshot")]
    TerminalPipelineSnapshot(WTerminalPipelineSnapshot),
}

impl CoordWorkerUpstream {
    /// The wire spelling of this frame's discriminant.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Hello { .. } => "hello",
            Self::Pong { .. } => "pong",
            Self::Event { .. } => "event",
            Self::RpcOk { .. } => "rpc-ok",
            Self::RpcError { .. } => "rpc-error",
            Self::Binary(_) => "binary",
            Self::RefreshJwt(_) => "refresh-jwt",
            Self::CellGrid(_) => "cell-grid",
            Self::CellGridChunk(_) => "cell-grid-chunk",
            Self::InputResult(_) => "input-result",
            Self::TerminalStreamResult(_) => "terminal-stream-result",
            Self::AgentStatus(_) => "agent-status",
            Self::UpdateProgress(_) => "update-progress",
            Self::TerminalMetadata(_) => "terminal-metadata",
            Self::TerminalViewState(_) => "terminal-view-state",
            Self::TerminalViewProjection(_) => "terminal-view-projection",
            Self::LocalTerminalPeerAnswer(_) => "local-terminal-peer-answer",
            Self::LocalTerminalPeerError(_) => "local-terminal-peer-error",
            Self::LocalAttachmentPeerAnswer(_) => "local-attachment-peer-answer",
            Self::LocalAttachmentPeerError(_) => "local-attachment-peer-error",
            Self::AttachmentDirectStatus(_) => "attachment-direct-status",
            Self::TerminalInputRouteResult(_) => "terminal-input-route-result",
            Self::TerminalTransportProbeResult(_) => "terminal-transport-probe-result",
            Self::TerminalPipelineSnapshot(_) => "terminal-pipeline-snapshot",
        }
    }

    /// Decode and check one upstream frame. `value` is the already-decoded
    /// JSON.
    pub fn parse(value: Value) -> ProtocolResult<Self> {
        // The event goes through its own admission path first: a frame is not a
        // way to append a record the durable log would have refused.
        if let (Some("event"), Some(event)) = (kind_of(&value), value.get("event")) {
            SessionEvent::parse(event.clone())?;
        }
        let frame: Self = serde_json::from_value(value)
            .map_err(|error| ProtocolError::new("coord_worker_upstream", error.to_string()))?;
        frame.check()?;
        Ok(frame)
    }

    /// The rules a constructed value can be held to, beyond the shapes their
    /// own types already enforce.
    pub fn check(&self) -> ProtocolResult<()> {
        match self {
            Self::Pong { ts, .. } => nonnegative("pong.ts", *ts),
            Self::Binary(binary) => binary.check(),
            _ => Ok(()),
        }
    }
}
