//! One dedicated, correlated terminal-capture call to the session's own
//! worker: a `diag-terminal-capture` browser command under the coordinator's
//! diagnostic identity, settled by that worker's `rpc-ok` through the shared
//! pending table, with the capture deadline instead of the 2 s diagnostic one.
//! The reply crosses a trust boundary, so the acknowledgement is rebuilt from
//! the fields it recognizes and everything else is a worker failure.
//! Called by `terminal_capture::bridge`.
//! Ports `apps/coord/src/terminal/capture/terminal-capture-worker-call.ts`.

use connectrpc::ErrorCode;
use roost_protocol::terminal_capture::command::TerminalCaptureCommand;
use roost_protocol::terminal_capture::{
    TerminalCaptureErrorCode, TerminalCaptureFileRef, TerminalCaptureStatus,
    TerminalCaptureWorkerAck,
};
use roost_protocol::wire::control::ClientControlFrame;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use roost_protocol::wire::{SessionId, WorkerFp};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use tokio::time::{Duration, Instant};

use crate::terminal_screen::scrollback_relay::ScrollbackRelay;
use crate::workers::diag_send::COORD_DIAG_BROWSER_ID;
use crate::workers::send::{SendOutcome, current_routable_worker, send_frame_through};

/// A worker-chosen path under its own log directory; this only stops an
/// unbounded string reaching the browser.
const WORKER_PATH_MAX_CHARS: usize = 1_024;
const WORKER_CAPTURE_ID_MAX_CHARS: usize = 64;
/// The largest integer a JavaScript peer can send exactly.
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// How a worker call ended: an acknowledgement, or a fixed failure code
/// (`WorkerOffline`, `WorkerTimeout` or `WorkerFailed`).
pub type TerminalCaptureWorkerOutcome = Result<TerminalCaptureWorkerAck, TerminalCaptureErrorCode>;

/// Send one capture step and wait for the worker's acknowledgement.
pub async fn request_terminal_capture(
    relay: &ScrollbackRelay,
    worker_fp: &WorkerFp,
    command: &TerminalCaptureCommand,
    coordinator_evidence_json: &str,
    deadline_ms: u64,
) -> TerminalCaptureWorkerOutcome {
    use TerminalCaptureErrorCode::{WorkerFailed, WorkerOffline, WorkerTimeout};
    let started_at = Instant::now();
    let Some(worker) = current_routable_worker(relay.workers(), worker_fp) else {
        return Err(WorkerOffline);
    };
    let Ok(session_id) = SessionId::try_from(command.session_id.as_str()) else {
        return Err(WorkerFailed);
    };
    let mut pending = match relay
        .pending()
        .create_fresh(Some(worker_fp.as_str()), relay.now_ms())
    {
        Ok(pending) => pending,
        Err(error) => {
            tracing::warn!(%worker_fp, ?error, "terminal capture: no correlation entry");
            return Err(WorkerFailed);
        }
    };
    let request_id = pending.request_id().to_owned();
    let frame = CoordWorkerDownstream::BrowserCommand {
        browser_id: COORD_DIAG_BROWSER_ID.to_owned(),
        viewer_id: COORD_DIAG_BROWSER_ID.to_owned(),
        request_id: request_id.clone(),
        frame: ClientControlFrame::DiagTerminalCapture {
            request_id,
            session_id,
            recording_id: command.recording_id.clone(),
            capture_id: command.capture_id.clone(),
            action: command.action.as_str().to_owned(),
            reason: command.reason.as_str().to_owned(),
            browser_evidence_json: command.browser_evidence_json.clone(),
            coordinator_evidence_json: coordinator_evidence_json.to_owned(),
            trace_id: None,
        },
        trace_id: None,
    };
    // A refused write drops `pending` at return, so a late reply finds nothing.
    if let SendOutcome::Refused(refusal) = send_frame_through(relay.workers(), &worker, frame) {
        tracing::warn!(%worker_fp, %refusal, "terminal capture: the worker transport dropped the request");
        return Err(WorkerOffline);
    }
    let wait_until = started_at + Duration::from_millis(deadline_ms);
    match tokio::time::timeout_at(wait_until, pending.settle()).await {
        Err(_elapsed) => Err(WorkerTimeout),
        Ok(Ok(value)) => narrow_worker_ack(&value).ok_or(WorkerFailed),
        Ok(Err(error)) => Err(match error.code {
            ErrorCode::DeadlineExceeded => WorkerTimeout,
            ErrorCode::Unavailable => WorkerOffline,
            _ => WorkerFailed,
        }),
    }
}

/// Rebuild the acknowledgement from recognized fields only. One unexpected
/// shape is a worker failure, never a partially trusted result: the path and
/// byte length end up in an operator-visible download action.
fn narrow_worker_ack(value: &Value) -> Option<TerminalCaptureWorkerAck> {
    let data = value.as_object()?;
    let status = closed_literal::<TerminalCaptureStatus>(data.get("status")?)?;
    let error = match data.get("error") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) if text.is_empty() => None,
        Some(other) => Some(closed_literal::<TerminalCaptureErrorCode>(other)?),
    };
    Some(TerminalCaptureWorkerAck {
        status,
        path: narrow_path(data.get("path"))?,
        byte_length: narrow_count(data.get("byte_length"))?,
        error,
        expires_at_ms: narrow_count(data.get("expires_at_ms"))?,
        recent_worker_capture: narrow_file_ref(data.get("recent_worker_capture"))?,
    })
}

/// A string that is exactly one member of a closed snake_case vocabulary.
fn closed_literal<T: DeserializeOwned>(value: &Value) -> Option<T> {
    value
        .is_string()
        .then(|| serde_json::from_value(value.clone()).ok())
        .flatten()
}

/// Outer `None` rejects the acknowledgement; inner `None` is an absent field.
fn narrow_path(value: Option<&Value>) -> Option<Option<String>> {
    match value {
        None | Some(Value::Null) => Some(None),
        Some(Value::String(text)) if text.is_empty() => Some(None),
        Some(Value::String(text)) if text.chars().count() <= WORKER_PATH_MAX_CHARS => {
            Some(Some(text.clone()))
        }
        Some(_) => None,
    }
}

fn narrow_count(value: Option<&Value>) -> Option<Option<u64>> {
    match value {
        None | Some(Value::Null) => Some(None),
        Some(number) => number
            .as_u64()
            .filter(|count| *count <= MAX_SAFE_INTEGER)
            .map(Some),
    }
}

fn narrow_file_ref(value: Option<&Value>) -> Option<Option<TerminalCaptureFileRef>> {
    let reference: &Map<String, Value> = match value {
        None | Some(Value::Null) => return Some(None),
        Some(Value::Object(reference)) => reference,
        Some(_) => return None,
    };
    let capture_id = reference.get("capture_id")?.as_str()?;
    if capture_id.is_empty() || capture_id.chars().count() > WORKER_CAPTURE_ID_MAX_CHARS {
        return None;
    }
    Some(Some(TerminalCaptureFileRef {
        capture_id: capture_id.to_owned(),
        path: narrow_path(reference.get("path"))??,
        byte_length: narrow_count(reference.get("byte_length"))??,
        status: closed_literal::<TerminalCaptureStatus>(reference.get("status")?)?,
    }))
}
