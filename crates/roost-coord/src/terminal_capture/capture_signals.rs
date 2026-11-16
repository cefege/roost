//! The doctor-readable records of a terminal capture's lifecycle: started,
//! stopped and failed. Called by `terminal_capture::bridge` beside the log line
//! each transition already writes; the cooldown key is the recording or capture
//! id so two distinct captures never coalesce.

use std::fmt::Debug;

use roost_observability::{LogFields, SignalKind, signal};

/// A recording was armed (or its lease renewed) and the worker acknowledged it.
pub(super) fn capture_started(
    session_id: &str,
    recording_id: &str,
    worker_fp: &str,
    expires_at_ms: u64,
    renewed: bool,
) {
    signal::emit(
        SignalKind::TerminalCaptureStarted,
        LogFields::new()
            .set("sid", session_id)
            .set("recording_id", recording_id)
            .set("worker_fp", worker_fp)
            .set("expires_at_ms", expires_at_ms)
            .set("renewed", renewed)
            .set("cooldownKey", recording_id),
    );
}

/// The owner released a recording.
pub(super) fn capture_stopped(session_id: &str, recording_id: &str, lease_ms: u64) {
    signal::emit(
        SignalKind::TerminalCaptureStopped,
        LogFields::new()
            .set("sid", session_id)
            .set("recording_id", recording_id)
            .set("lease_ms", lease_ms)
            .set("cooldownKey", recording_id),
    );
}

/// A start or capture could not complete; `capture_id` names the capture, or
/// the recording id when the failure came before one existed.
pub(super) fn capture_failed(session_id: &str, capture_id: &str, error: &dyn Debug) {
    signal::emit(
        SignalKind::TerminalCaptureFailed,
        LogFields::new()
            .set("sid", session_id)
            .set("capture_id", capture_id)
            .set("error", format!("{error:?}"))
            .set("cooldownKey", capture_id),
    );
}

/// A lease passed its server-time expiry (or was ended for `reason`) without
/// renewal.
pub(super) fn capture_expired(session_id: &str, recording_id: &str, reason: &str) {
    signal::emit(
        SignalKind::TerminalCaptureExpired,
        LogFields::new()
            .set("sid", session_id)
            .set("recording_id", recording_id)
            .set("reason", reason)
            .set("cooldownKey", recording_id),
    );
}
