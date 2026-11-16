//! The always-on anomaly signals one worker link raises: protocol violations,
//! queue overflows, an exceeded event rate and a failed durable append. Called
//! by `handshake`, `link_session`, `result_lane` and `frame_dispatch`, each of
//! which already logs the same event for context; this module only adds the
//! doctor-readable record, so every site stays one line.

use std::fmt::Display;

use roost_observability::{LogFields, SignalKind, signal};
use serde::Serialize;

/// The worker sent a frame this link refuses: `reason` names which rule.
pub(super) fn protocol_violation(worker_fp: &str, reason: &'static str) {
    signal::emit(
        SignalKind::WorkerProtocolViolation,
        LogFields::new()
            .set("worker_fp", worker_fp)
            .set("reason", reason)
            .set("cooldownKey", worker_fp),
    );
}

/// A bounded worker-frame queue or socket budget rejected a frame.
pub(super) fn queue_overflow(worker_fp: &str, frames: impl Serialize, bytes: impl Serialize) {
    signal::emit(
        SignalKind::WorkerQueueOverflow,
        LogFields::new()
            .set("worker_fp", worker_fp)
            .set("frames", frames)
            .set("bytes", bytes)
            .set("cooldownKey", worker_fp),
    );
}

/// The worker's durable-event window was exceeded and the socket closes.
pub(super) fn event_rate_exceeded(
    worker_fp: impl Display,
    limit: impl Serialize,
    window_ms: impl Serialize,
) {
    let worker_fp = worker_fp.to_string();
    signal::emit(
        SignalKind::WorkerEventRateExceeded,
        LogFields::new()
            .set("worker_fp", &worker_fp)
            .set("limit", limit)
            .set("window_ms", window_ms)
            .set("cooldownKey", &worker_fp),
    );
}

/// A worker's durable event could not be appended to the log.
pub(super) fn event_append_failed(worker_fp: &str, client_seq: u64, error: &dyn Display) {
    signal::emit(
        SignalKind::EventAppendFailed,
        LogFields::new()
            .set("worker_fp", worker_fp)
            .set("client_seq", client_seq)
            .set("error", error.to_string()),
    );
}
