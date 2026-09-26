//! The answer to one browser command, and the trace it carries back. Every
//! handler in this module returns a `Reply`; `mod::dispatch` is the only thing
//! that turns one into a link frame.
//!
//! The trace is read here rather than at each handler because every variant of
//! `ClientControlFrame` carries one and the only way to read it is an
//! exhaustive match. One match, in one place, means a frame kind that arrives
//! without a trace fails this file's build rather than silently answering
//! uncorrelated.

use roost_protocol::wire::brand::TraceId;
use roost_protocol::wire::control::ClientControlFrame;
use roost_protocol::wire::coord_worker::CoordWorkerUpstream;

/// What a command is answered with.
///
/// Always correlated: a `Reply` cannot be built without the request id it
/// answers, because the constructor is the only way to make one. That is what
/// keeps "handled but not answered" off the table — the failure mode where a
/// coordinator's pending-RPC entry waits out its deadline for a reply the
/// worker decided was unnecessary.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    Ok {
        request_id: String,
        data: serde_json::Value,
    },
    Error {
        request_id: String,
        message: String,
    },
}

impl Reply {
    pub fn ok(request_id: &str, data: serde_json::Value) -> Self {
        Self::Ok {
            request_id: request_id.to_owned(),
            data,
        }
    }

    pub fn error(request_id: &str, message: impl Into<String>) -> Self {
        Self::Error {
            request_id: request_id.to_owned(),
            message: message.into(),
        }
    }

    /// The request this answers.
    pub fn request_id(&self) -> &str {
        match self {
            Self::Ok { request_id, .. } | Self::Error { request_id, .. } => request_id,
        }
    }

    /// Whether this answer reports success.
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok { .. })
    }

    /// The payload, for a caller that is asserting on one.
    pub fn data(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Ok { data, .. } => Some(data),
            Self::Error { .. } => None,
        }
    }

    /// The message, for a caller that is asserting on a refusal.
    pub fn message(&self) -> Option<&str> {
        match self {
            Self::Ok { .. } => None,
            Self::Error { message, .. } => Some(message),
        }
    }

    /// The frame to put on the coordinator link, carrying `trace_id` forward.
    pub fn into_upstream(self, trace_id: Option<TraceId>) -> CoordWorkerUpstream {
        match self {
            Self::Ok { request_id, data } => CoordWorkerUpstream::RpcOk {
                request_id,
                data,
                trace_id,
            },
            Self::Error {
                request_id,
                message,
            } => CoordWorkerUpstream::RpcError {
                request_id,
                message,
                trace_id,
            },
        }
    }
}

/// The trace a frame carries, echoed on whatever answers it.
///
/// Exhaustive on purpose: `ClientControlFrame::kind` is exhaustive for the
/// same reason, and a variant added without one of the two is a command whose
/// trace silently stops travelling.
pub fn trace_id(frame: &ClientControlFrame) -> Option<TraceId> {
    match frame {
        ClientControlFrame::Attach { trace_id, .. }
        | ClientControlFrame::Detach { trace_id, .. }
        | ClientControlFrame::SpawnShell { trace_id, .. }
        | ClientControlFrame::Kill { trace_id, .. }
        | ClientControlFrame::ReadFile { trace_id, .. }
        | ClientControlFrame::ReadFileChunk { trace_id, .. }
        | ClientControlFrame::AttachmentProbe { trace_id, .. }
        | ClientControlFrame::ListDir { trace_id, .. }
        | ClientControlFrame::Mkdir { trace_id, .. }
        | ClientControlFrame::ListSkills { trace_id, .. }
        | ClientControlFrame::GitDiff { trace_id, .. }
        | ClientControlFrame::SetTitle { trace_id, .. }
        | ClientControlFrame::CursorPos { trace_id, .. }
        | ClientControlFrame::GetHome { trace_id, .. }
        | ClientControlFrame::GetScrollbackCells { trace_id, .. }
        | ClientControlFrame::SearchScrollback { trace_id, .. }
        | ClientControlFrame::CancelScrollbackSearch { trace_id, .. }
        | ClientControlFrame::SearchScrollbackBatch { trace_id, .. }
        | ClientControlFrame::CancelScrollbackSearchBatch { trace_id, .. }
        | ClientControlFrame::ListAttachments { trace_id, .. }
        | ClientControlFrame::DeleteAttachment { trace_id, .. }
        | ClientControlFrame::DiagTerminalCapture { trace_id, .. }
        | ClientControlFrame::DiagSnapshot { trace_id, .. }
        | ClientControlFrame::RespawnIfMissing { trace_id, .. } => trace_id.clone(),
    }
}
