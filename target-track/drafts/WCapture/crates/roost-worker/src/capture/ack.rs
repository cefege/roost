//! The worker's reply to one `diag-terminal-capture` step: the constructors
//! every capture path answers with, and the rule for `recent_worker_capture`.
//! Ports `apps/worker/src/diag/terminal-capture-ack.ts` (the reply type itself
//! is `roost_protocol::terminal_capture::TerminalCaptureWorkerAck`); called by
//! `super::recorder` and `super::write`.

use roost_protocol::terminal_capture::{
    TerminalCaptureErrorCode, TerminalCaptureFileRef, TerminalCaptureStatus,
    TerminalCaptureWorkerAck,
};

/// `recent_worker_capture` names the last incident the WORKER ITSELF froze. The
/// capture being answered is excluded: echoing it there would claim the worker
/// independently detected an incident the operator requested.
pub fn recent_worker_capture_for(
    recent: Option<&TerminalCaptureFileRef>,
    answering_capture_id: &str,
) -> Option<TerminalCaptureFileRef> {
    recent
        .filter(|recent| recent.capture_id != answering_capture_id)
        .cloned()
}

/// v2 `terminalCaptureFailureAck`.
pub fn failure_ack(
    error: TerminalCaptureErrorCode,
    recent: Option<TerminalCaptureFileRef>,
    expires_at_ms: Option<u64>,
) -> TerminalCaptureWorkerAck {
    TerminalCaptureWorkerAck {
        status: TerminalCaptureStatus::Error,
        path: None,
        byte_length: None,
        error: Some(error),
        expires_at_ms,
        recent_worker_capture: recent,
    }
}

/// An armed or renewed lease.
pub fn recording_ack(
    expires_at_ms: u64,
    recent: Option<TerminalCaptureFileRef>,
) -> TerminalCaptureWorkerAck {
    TerminalCaptureWorkerAck {
        status: TerminalCaptureStatus::Recording,
        path: None,
        byte_length: None,
        error: None,
        expires_at_ms: Some(expires_at_ms),
        recent_worker_capture: recent,
    }
}

/// A released lease, or a repeat STOP that found nothing left to free.
pub fn stopped_ack(recent: Option<TerminalCaptureFileRef>) -> TerminalCaptureWorkerAck {
    TerminalCaptureWorkerAck {
        status: TerminalCaptureStatus::Stopped,
        path: None,
        byte_length: None,
        error: None,
        expires_at_ms: None,
        recent_worker_capture: recent,
    }
}
