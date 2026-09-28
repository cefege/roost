//! The validated capture command and the result a DiagSnapshot answers with:
//! the `TerminalCaptureRequest` wire validation v2 runs before any cache
//! lookup, lease allocation or worker dispatch, and the `terminal_capture`
//! member of the response. Ports `TerminalCaptureActionName`,
//! `TerminalCaptureCommand`, `TerminalCaptureResult` and
//! `validateTerminalCaptureRequest` of `packages/protocol/src/terminal-capture.ts`.
//! Consumed by the coordinator's capture bridge (`roost_coord::terminal_capture`).

use roost_proto::{TerminalCaptureAction, TerminalCaptureRequest};
use serde::Serialize;

use super::bundle::TerminalCaptureReason;
use super::{
    TerminalCaptureErrorCode, TerminalCaptureFileRef, TerminalCaptureStatus,
    has_at_most_browser_evidence_bytes,
};
use crate::viewport::is_terminal_uuid;

/// One step of an opt-in recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalCaptureActionName {
    /// Arm or renew a lease.
    Start,
    /// Freeze an incident bundle.
    Capture,
    /// Release a lease.
    Stop,
}

impl TerminalCaptureActionName {
    /// The spelling the worker frame and the result carry.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Capture => "capture",
            Self::Stop => "stop",
        }
    }
}

/// A request that passed every wire bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalCaptureCommand {
    pub action: TerminalCaptureActionName,
    pub session_id: String,
    pub recording_id: String,
    pub capture_id: String,
    pub reason: TerminalCaptureReason,
    /// Frozen browser evidence; empty for START/STOP.
    pub browser_evidence_json: String,
}

/// The fixed refusal a validation produces: a code and the field it names.
pub type CaptureRefusal = (TerminalCaptureErrorCode, &'static str);

/// Validate the wire request. UNSPECIFIED is invalid whenever the message is
/// present, and START/STOP carry no evidence and only the manual reason, so a
/// control call can never smuggle a payload past the CAPTURE size gate.
///
/// # Errors
/// The code and the first field that failed its bound.
pub fn validate_terminal_capture_request(
    request: &TerminalCaptureRequest,
) -> Result<TerminalCaptureCommand, CaptureRefusal> {
    use TerminalCaptureErrorCode::{EvidenceTooLarge, InvalidArgument};
    let action = match request.action.as_known() {
        Some(TerminalCaptureAction::Start) => TerminalCaptureActionName::Start,
        Some(TerminalCaptureAction::Capture) => TerminalCaptureActionName::Capture,
        Some(TerminalCaptureAction::Stop) => TerminalCaptureActionName::Stop,
        _ => return Err((InvalidArgument, "action")),
    };
    for (value, field) in [
        (&request.session_id, "session_id"),
        (&request.recording_id, "recording_id"),
        (&request.capture_id, "capture_id"),
    ] {
        if !is_terminal_uuid(value) {
            return Err((InvalidArgument, field));
        }
    }
    let Some(reason) = TerminalCaptureReason::parse(&request.reason) else {
        return Err((InvalidArgument, "reason"));
    };
    if action != TerminalCaptureActionName::Capture {
        if !request.browser_evidence_json.is_empty() {
            return Err((InvalidArgument, "browser_evidence_json"));
        }
        if reason != TerminalCaptureReason::Manual {
            return Err((InvalidArgument, "reason"));
        }
    } else if !has_at_most_browser_evidence_bytes(&request.browser_evidence_json) {
        return Err((EvidenceTooLarge, "browser_evidence_json"));
    }
    Ok(TerminalCaptureCommand {
        action,
        session_id: request.session_id.clone(),
        recording_id: request.recording_id.clone(),
        capture_id: request.capture_id.clone(),
        reason,
        browser_evidence_json: request.browser_evidence_json.clone(),
    })
}

/// `terminal_capture` of a DiagSnapshot response: ids, counts, bounds and
/// status only, never terminal content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TerminalCaptureResult {
    pub capture_id: String,
    pub recording_id: String,
    pub session_id: String,
    pub action: TerminalCaptureActionName,
    pub status: TerminalCaptureStatus,
    pub expires_at_ms: Option<u64>,
    pub worker_fp: Option<String>,
    pub path: Option<String>,
    pub byte_length: Option<u64>,
    pub error: Option<TerminalCaptureErrorCode>,
    /// The last worker-local frozen incident, so a worker-triggered capture is
    /// downloadable though the browser never asked for it.
    pub recent_worker_capture: Option<TerminalCaptureFileRef>,
}
