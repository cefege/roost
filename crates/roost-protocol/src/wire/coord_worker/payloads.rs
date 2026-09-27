//! The payload records the two worker-link frame unions name, plus the
//! generated messages the remaining arms carry verbatim. Read by
//! `coord_worker::upstream` and `coord_worker::downstream`; mapped to and from
//! protobuf by `proto_adapters::coord_worker_proto`.
//!
//! An arm carries a record from this file when the crate already has an owner
//! for that shape — `AgentStatus` — or when the shape is a handful of scalars
//! a caller in this workspace has to build without naming a protobuf type.
//! Every other arm carries the generated message, re-exported here so a crate
//! that may not depend on `roost-proto` (the worker) can still construct it.
//! That is deliberately not a second value model: the generated message is the
//! only definition of a shape no domain module owns yet, and the track that
//! gives it an owner replaces the re-export rather than adding a parallel
//! struct.

use serde::{Deserialize, Serialize};

use crate::wire::agent_status::AgentStatus;
use crate::wire::brand::{ChannelId, SessionId};
use crate::wire::coord_worker::{DIR_FROM_PTY, DIR_TO_PTY};
use crate::{ProtocolError, ProtocolResult};

pub use roost_proto::{
    // Downstream arms with no domain owner yet.
    DAgentPrompt,
    DAttachmentChunk,
    DAttachmentDirectStatusRequest,
    DCoordMovePrepare,
    DCoordMoveSnapshotChunk,
    DCoordMoveSnapshotStart,
    DCoordRelocate,
    DInputRequest,
    DKeeperUpdatePrepare,
    DLocalAttachmentGrant,
    DLocalAttachmentGrantRevoke,
    DLocalAttachmentPeerCancel,
    DLocalAttachmentPeerOffer,
    DLocalTerminalGrant,
    DLocalTerminalGrantRevoke,
    DLocalTerminalPeerCancel,
    DLocalTerminalPeerOffer,
    DTerminalDirectRetire,
    DTerminalInputRouteClaim,
    DTerminalPipelineSnapshotRequest,
    DTerminalStreamState,
    DTerminalTransportProbe,
    DTerminalViewRelay,
    DTerminalViewSocketClosed,
    DUpdateBroker,
    // Upstream arms with no domain owner yet.
    WAttachmentDirectStatus,
    WCellGrid,
    WCellGridChunk,
    WLocalAttachmentPeerAnswer,
    WLocalAttachmentPeerError,
    WLocalTerminalPeerAnswer,
    WLocalTerminalPeerError,
    WTerminalInputRouteResult,
    WTerminalPipelineSnapshot,
    WTerminalTransportProbeResult,
    WTerminalViewProjection,
    WTerminalViewState,
};

/// PTY bytes in flight between the two ends of the link. `seq` is the keeper
/// ring's end sequence and is upstream-only: the coordinator stamps a
/// direction on what it relays and has no ring to read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Binary {
    pub channel_id: ChannelId,
    pub direction: u8,
    pub data: Vec<u8>,
    #[serde(default, skip_serializing_if = "is_zero_seq")]
    pub seq: u64,
}

fn is_zero_seq(seq: &u64) -> bool {
    *seq == 0
}

impl Binary {
    /// The two direction bytes, checked together so a frame naming neither is
    /// refused at the boundary rather than at the keeper write.
    pub fn check(&self) -> ProtocolResult<()> {
        if self.direction == DIR_FROM_PTY || self.direction == DIR_TO_PTY {
            Ok(())
        } else {
            Err(ProtocolError::new(
                "binary.direction",
                format!(
                    "must be {DIR_FROM_PTY} or {DIR_TO_PTY}, got {}",
                    self.direction
                ),
            ))
        }
    }
}

/// The in-band replacement token a worker sends before its current JWT
/// expires. The stream stays open across it, so a bad one closes the socket
/// rather than any frame being dropped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RefreshJwt {
    pub jwt: String,
}

/// Compact semantic terminal metadata: title and activity as state rather than
/// as PTY bytes, so a change costs a fixed frame however long the line is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalMetadata {
    pub channel_id: ChannelId,
    pub title_changed: bool,
    pub title: String,
    pub activity_changed: bool,
    pub activity_ts_ms: u64,
}

/// Journal-backed progress of one update job on the worker host. Replayed
/// after a reconnect; the sequence is monotonic per job, so a worker that
/// reconnects mid-update resumes rather than restarting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateProgress {
    pub request_id: String,
    pub job_id: String,
    pub sequence: u64,
    pub phase: String,
    pub message: String,
    pub terminal: bool,
    pub success: bool,
    pub error: String,
}

/// A volatile agent-status observation the worker publishes for one session.
/// Never placed in the durable outbox: a lost status is replaced by the next
/// one, and a stale one is fenced by the status epoch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentStatusFrame {
    pub status: AgentStatus,
}

/// The exact `client_seq` a durable insert (or unique-index dedup) settled.
/// A stale or duplicate ack cannot release the worker's replay barrier, so
/// the number is the whole content of this frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventAck {
    pub client_seq: u64,
}

/// A fire-and-forget repair request for one session's stream. Repeating it
/// replaces any partial same-stream cursor with a fresh snapshot id and full
/// baseline, so it carries no id of its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSnapshotRequest {
    pub session_id: SessionId,
    pub stream_id: String,
}

/// Where a terminal-control request stopped relative to the keeper write.
/// `status` says what happened; the phase is what makes it provable. Only
/// `PreWrite` can promise no mutation occurred, so it is the sole basis on
/// which the coordinator rolls provisional state back and the browser retries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalWritePhase {
    PreWrite,
    Written,
    Unknown,
}

impl TerminalWritePhase {
    /// The protobuf spelling, which is the enum's own variant name minus the
    /// `TERMINAL_WRITE_PHASE_` prefix the generated code adds.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PreWrite => "pre_write",
            Self::Written => "written",
            Self::Unknown => "unknown",
        }
    }
}

/// The truthful outcome of a terminal-control request. `status` is what the
/// worker is certain of; `phase` is what makes a rejection safely retryable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalInputStatus {
    Accepted,
    Rejected,
    Ambiguous,
}

impl TerminalInputStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Ambiguous => "ambiguous",
        }
    }
}

/// A terminal-control request's outcome, correlated by the coordinator's own
/// `request_id`. Unlike the legacy binary path an accepted result proves the
/// keeper completed the PTY operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputResult {
    pub request_id: String,
    pub session_id: SessionId,
    pub input_seq: u64,
    pub status: TerminalInputStatus,
    pub written_bytes: u32,
    pub reason: String,
    pub phase: TerminalWritePhase,
}

/// How a terminal STREAM (the coordinator's view ownership) was resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalStreamStatus {
    Committed,
    Rejected,
    Ambiguous,
}

impl TerminalStreamStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Committed => "committed",
            Self::Rejected => "rejected",
            Self::Ambiguous => "ambiguous",
        }
    }
}

/// Why a stream request failed, which the status alone cannot say: an ambiguous
/// boundary is retryable and a session-not-live is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalStreamFailureKind {
    RetryablePreWrite,
    SessionNotLive,
    InvalidRequest,
    CoreFailed,
    AmbiguousBoundary,
}

impl TerminalStreamFailureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RetryablePreWrite => "retryable_pre_write",
            Self::SessionNotLive => "session_not_live",
            Self::InvalidRequest => "invalid_request",
            Self::CoreFailed => "core_failed",
            Self::AmbiguousBoundary => "ambiguous_boundary",
        }
    }
}

/// The outcome of one terminal-stream state request, carrying the geometry the
/// coordinator already aggregated and the generation it addressed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalStreamResult {
    pub request_id: String,
    pub session_id: SessionId,
    pub stream_id: String,
    pub enabled: bool,
    pub status: TerminalStreamStatus,
    pub channel_resize_seq: u64,
    pub effective_cols: u32,
    pub effective_rows: u32,
    pub resized: bool,
    pub reason: String,
    pub phase: TerminalWritePhase,
    pub failure_kind: TerminalStreamFailureKind,
}
