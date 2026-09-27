//! The terminal-control outcome tables: the domain enums for a write phase, an
//! input status, a stream status and a stream failure kind, against the
//! protobuf enums `worker_transport.proto` numbers them. Called by
//! `coord_worker_proto::upstream` for the `input-result` and
//! `terminal-stream-result` arms.
//!
//! The phase is not decoration on either result frame. Only a phase strictly
//! before the keeper write can promise no mutation occurred, so it is the sole
//! basis on which the coordinator rolls provisional state back and the browser
//! retries without risking a duplicate write. `UNSPECIFIED` is therefore
//! refused on decode rather than mapped to a value that would look provable.

use roost_proto::{
    TerminalInputStatus as PbTerminalInputStatus,
    TerminalStreamFailureKind as PbTerminalStreamFailureKind,
    TerminalStreamStatus as PbTerminalStreamStatus, TerminalWritePhase as PbTerminalWritePhase,
    WInputResult, WTerminalStreamResult,
};

use crate::wire::brand::SessionId;
use crate::wire::coord_worker::{
    InputResult, TerminalInputStatus, TerminalStreamFailureKind, TerminalStreamResult,
    TerminalStreamStatus, TerminalWritePhase,
};
use crate::{ProtocolError, ProtocolResult};

/// The generated proto enums for the terminal-control outcomes. The two
/// tables below are lookups over `worker_transport.proto` and nothing else:
/// the domain enums name what each value MEANS, the proto enums number them.
fn write_phase_to_proto(phase: TerminalWritePhase) -> PbTerminalWritePhase {
    match phase {
        TerminalWritePhase::PreWrite => PbTerminalWritePhase::PreWrite,
        TerminalWritePhase::Written => PbTerminalWritePhase::Written,
        TerminalWritePhase::Unknown => PbTerminalWritePhase::Unknown,
    }
}

fn write_phase_from_proto(field: &str, value: i32) -> ProtocolResult<TerminalWritePhase> {
    match value {
        1 => Ok(TerminalWritePhase::PreWrite),
        2 => Ok(TerminalWritePhase::Written),
        3 => Ok(TerminalWritePhase::Unknown),
        0 => Err(ProtocolError::new(
            field,
            "the unspecified phase proves nothing",
        )),
        other => Err(ProtocolError::new(
            field,
            format!("{other} is not a terminal write phase this build names"),
        )),
    }
}

fn input_status_to_proto(status: TerminalInputStatus) -> PbTerminalInputStatus {
    match status {
        TerminalInputStatus::Accepted => PbTerminalInputStatus::Accepted,
        TerminalInputStatus::Rejected => PbTerminalInputStatus::Rejected,
        TerminalInputStatus::Ambiguous => PbTerminalInputStatus::Ambiguous,
    }
}

fn input_status_from_proto(field: &str, value: i32) -> ProtocolResult<TerminalInputStatus> {
    match value {
        1 => Ok(TerminalInputStatus::Accepted),
        2 => Ok(TerminalInputStatus::Rejected),
        3 => Ok(TerminalInputStatus::Ambiguous),
        0 => Err(ProtocolError::new(
            field,
            "the unspecified status says nothing",
        )),
        other => Err(ProtocolError::new(
            field,
            format!("{other} is not a terminal input status this build names"),
        )),
    }
}

fn stream_status_to_proto(status: TerminalStreamStatus) -> PbTerminalStreamStatus {
    match status {
        TerminalStreamStatus::Committed => PbTerminalStreamStatus::Committed,
        TerminalStreamStatus::Rejected => PbTerminalStreamStatus::Rejected,
        TerminalStreamStatus::Ambiguous => PbTerminalStreamStatus::Ambiguous,
    }
}

fn stream_status_from_proto(field: &str, value: i32) -> ProtocolResult<TerminalStreamStatus> {
    match value {
        1 => Ok(TerminalStreamStatus::Committed),
        2 => Ok(TerminalStreamStatus::Rejected),
        3 => Ok(TerminalStreamStatus::Ambiguous),
        0 => Err(ProtocolError::new(
            field,
            "the unspecified status says nothing",
        )),
        other => Err(ProtocolError::new(
            field,
            format!("{other} is not a terminal stream status this build names"),
        )),
    }
}

fn failure_kind_to_proto(kind: TerminalStreamFailureKind) -> PbTerminalStreamFailureKind {
    match kind {
        TerminalStreamFailureKind::RetryablePreWrite => {
            PbTerminalStreamFailureKind::RetryablePreWrite
        }
        TerminalStreamFailureKind::SessionNotLive => PbTerminalStreamFailureKind::SessionNotLive,
        TerminalStreamFailureKind::InvalidRequest => PbTerminalStreamFailureKind::InvalidRequest,
        TerminalStreamFailureKind::CoreFailed => PbTerminalStreamFailureKind::CoreFailed,
        TerminalStreamFailureKind::AmbiguousBoundary => {
            PbTerminalStreamFailureKind::AmbiguousBoundary
        }
    }
}

fn failure_kind_from_proto(field: &str, value: i32) -> ProtocolResult<TerminalStreamFailureKind> {
    match value {
        1 => Ok(TerminalStreamFailureKind::RetryablePreWrite),
        2 => Ok(TerminalStreamFailureKind::SessionNotLive),
        3 => Ok(TerminalStreamFailureKind::InvalidRequest),
        4 => Ok(TerminalStreamFailureKind::CoreFailed),
        5 => Ok(TerminalStreamFailureKind::AmbiguousBoundary),
        0 => Err(ProtocolError::new(
            field,
            "the unspecified failure kind says nothing",
        )),
        other => Err(ProtocolError::new(
            field,
            format!("{other} is not a stream failure kind this build names"),
        )),
    }
}

fn session_id(field: &str, value: &str) -> ProtocolResult<SessionId> {
    SessionId::try_from(value).map_err(|error| error.within(field))
}

/// One terminal-control request's truthful outcome, in both directions. The
/// phase is mandatory on both: the coordinator honours a rejection as definite
/// — unwinding provisional state and freeing the browser to retry — only when
/// the phase proves the keeper never wrote.
pub(super) fn input_result_to_proto(result: &InputResult) -> ProtocolResult<WInputResult> {
    Ok(WInputResult {
        request_id: result.request_id.clone(),
        session_id: result.session_id.as_str().to_owned(),
        input_seq: result.input_seq,
        status: input_status_to_proto(result.status).into(),
        written_bytes: result.written_bytes,
        reason: result.reason.clone(),
        phase: write_phase_to_proto(result.phase).into(),
        ..Default::default()
    })
}

pub(super) fn input_result_from_proto(result: &WInputResult) -> ProtocolResult<InputResult> {
    Ok(InputResult {
        request_id: result.request_id.clone(),
        session_id: session_id("input-result.session_id", &result.session_id)?,
        input_seq: result.input_seq,
        status: input_status_from_proto("input-result.status", result.status.to_i32())?,
        written_bytes: result.written_bytes,
        reason: result.reason.clone(),
        phase: write_phase_from_proto("input-result.phase", result.phase.to_i32())?,
    })
}

/// One terminal-stream request's outcome, in both directions.
pub(super) fn stream_result_to_proto(
    result: &TerminalStreamResult,
) -> ProtocolResult<WTerminalStreamResult> {
    Ok(WTerminalStreamResult {
        request_id: result.request_id.clone(),
        session_id: result.session_id.as_str().to_owned(),
        stream_id: result.stream_id.clone(),
        enabled: result.enabled,
        status: stream_status_to_proto(result.status).into(),
        channel_resize_seq: result.channel_resize_seq,
        effective_cols: result.effective_cols,
        effective_rows: result.effective_rows,
        resized: result.resized,
        reason: result.reason.clone(),
        phase: write_phase_to_proto(result.phase).into(),
        failure_kind: failure_kind_to_proto(result.failure_kind).into(),
        ..Default::default()
    })
}

pub(super) fn stream_result_from_proto(
    result: &WTerminalStreamResult,
) -> ProtocolResult<TerminalStreamResult> {
    Ok(TerminalStreamResult {
        request_id: result.request_id.clone(),
        session_id: session_id("terminal-stream-result.session_id", &result.session_id)?,
        stream_id: result.stream_id.clone(),
        enabled: result.enabled,
        status: stream_status_from_proto("terminal-stream-result.status", result.status.to_i32())?,
        channel_resize_seq: result.channel_resize_seq,
        effective_cols: result.effective_cols,
        effective_rows: result.effective_rows,
        resized: result.resized,
        reason: result.reason.clone(),
        phase: write_phase_from_proto("terminal-stream-result.phase", result.phase.to_i32())?,
        failure_kind: failure_kind_from_proto(
            "terminal-stream-result.failure_kind",
            result.failure_kind.to_i32(),
        )?,
    })
}
