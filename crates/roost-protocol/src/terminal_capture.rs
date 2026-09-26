//! The limits on a terminal capture request, the vocabulary its answer uses,
//! and the shape of that answer.
//!
//! One table, so a limit cannot drift between the layer that produces evidence
//! and the layer that refuses it: the worker's recorder, the coordinator's
//! bridge and the storage writer all read these numbers from here. The bundle
//! shapes and the validation of a bundle's contents live with the recorder and
//! the coordinator; this file is the bounds and the answer they agree on.
//!
//! The answer vocabulary lives here rather than in the worker because the
//! coordinator narrows a worker's reply against the SAME literals: a code only
//! the worker can name is a capture the coordinator treats as a worker failure.

use serde::{Deserialize, Serialize};

/// Byte and entry limits are `usize` because they are compared against a
/// buffer or a collection length; millisecond limits are `u64` because they are
/// compared against a clock delta the caller supplies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalCaptureLimits {
    /// Server-time lease window for one recording.
    pub lease_ms: u64,
    /// Idempotent START renewal cadence while the debugging pane is visible.
    pub renew_interval_ms: u64,
    pub max_recordings_per_process: usize,
    pub max_recordings_per_document: usize,
    /// Per session per layer, across all retained records.
    pub layer_bytes: usize,
    pub layer_entries: usize,
    pub raw_bytes: usize,
    pub cell_bytes: usize,
    pub metadata_bytes: usize,
    pub browser_evidence_bytes: usize,
    pub coordinator_evidence_bytes: usize,
    pub bundle_bytes: usize,
    /// Dedicated capture deadline; the ordinary diag snapshot keeps its 2s.
    pub capture_deadline_ms: u64,
    pub completed_capture_ids: usize,
    pub retention_ms: u64,
    /// Automatic captures per session, regardless of a new epoch.
    pub automatic_cooldown_ms: u64,
    pub manual_cooldown_ms: u64,
    pub core_sample_interval_ms: u64,
    pub core_sample_max_cells: usize,
    pub core_sample_budget_us: u64,
    pub core_sample_suppress_ms: u64,
    pub core_scrollback_tail_rows: usize,
    pub capture_history_rows: usize,
    pub browser_history_tail_rows: usize,
    pub browser_rows_max: usize,
    /// Combined byte cap plus incident retention on one worker.
    pub storage_files: usize,
    pub storage_bytes: usize,
}

pub const TERMINAL_CAPTURE_LIMITS: TerminalCaptureLimits = TerminalCaptureLimits {
    lease_ms: 30 * 60_000,
    renew_interval_ms: 5 * 60_000,
    max_recordings_per_process: 2,
    max_recordings_per_document: 2,
    layer_bytes: 8 * 1024 * 1024,
    layer_entries: 128,
    raw_bytes: 1024 * 1024,
    cell_bytes: 6 * 1024 * 1024,
    metadata_bytes: 1024 * 1024,
    browser_evidence_bytes: 512 * 1024,
    coordinator_evidence_bytes: 512 * 1024,
    bundle_bytes: 32 * 1024 * 1024,
    capture_deadline_ms: 10_000,
    completed_capture_ids: 128,
    retention_ms: 24 * 60 * 60_000,
    automatic_cooldown_ms: 60_000,
    manual_cooldown_ms: 10_000,
    core_sample_interval_ms: 250,
    core_sample_max_cells: 16_384,
    core_sample_budget_us: 2_000,
    core_sample_suppress_ms: 1_000,
    core_scrollback_tail_rows: 128,
    capture_history_rows: 256,
    browser_history_tail_rows: 128,
    browser_rows_max: 512,
    storage_files: 50,
    storage_bytes: 500 * 1024 * 1024,
};

/// The cap a control frame's evidence fields are admitted under.
///
/// A wire schema counts UTF-16 code units, so the authoritative bound is UTF-8
/// bytes and is re-checked by the validator; the wire bound can only be lower,
/// never higher, or the two layers would disagree about what fits.
pub const TERMINAL_CAPTURE_EVIDENCE_MAX_CHARS: usize =
    TERMINAL_CAPTURE_LIMITS.browser_evidence_bytes;

/// Whether browser evidence is within its cap, counted in UTF-8 bytes.
pub fn has_at_most_browser_evidence_bytes(value: &str) -> bool {
    value.len() <= TERMINAL_CAPTURE_LIMITS.browser_evidence_bytes
}

/// What a capture is doing, as the worker reports it.
///
/// A closed set on purpose: the coordinator rebuilds a worker's answer from
/// the fields it recognizes and treats an unfamiliar value as a worker
/// failure, so a status invented on one side and not the other is a capture
/// nobody can download rather than a warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalCaptureStatus {
    Recording,
    Captured,
    Stopped,
    /// A capture that froze evidence but could not write the whole bundle.
    Partial,
    Error,
}

/// Every way a capture can fail, and nothing else.
///
/// A capture failure never carries a validation or parser message, because
/// those quote the terminal text they failed on: an exception message from a
/// grid walk is a piece of somebody's screen, and this vocabulary crosses a
/// trust boundary into an operator-visible download.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalCaptureErrorCode {
    InvalidArgument,
    PermissionDenied,
    SessionUnknown,
    WorkerOffline,
    WorkerTimeout,
    WorkerFailed,
    LeaseConflict,
    LeaseExpired,
    LeaseAbsent,
    ResourceExhausted,
    EvidenceTooLarge,
    EvidenceMalformed,
    CaptureInFlight,
    RateLimited,
    CaptureExpired,
    StorageFailed,
    Internal,
}

/// A capture the worker froze, named so an operator can download it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalCaptureFileRef {
    pub capture_id: String,
    pub path: String,
    pub byte_length: u64,
    pub status: TerminalCaptureStatus,
}

/// The worker's answer to one capture command.
///
/// Spelled in the worker's own wire case, which is snake_case; the coordinator
/// rebuilds its camelCase projection from the fields it recognizes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalCaptureWorkerAck {
    pub status: TerminalCaptureStatus,
    pub path: Option<String>,
    pub byte_length: Option<u64>,
    pub error: Option<TerminalCaptureErrorCode>,
    pub expires_at_ms: Option<u64>,
    /// The last incident the WORKER froze on its own, so a capture the browser
    /// never asked for is still downloadable. The capture being answered is
    /// excluded: echoing it there would claim the worker independently found
    /// an incident the operator requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recent_worker_capture: Option<TerminalCaptureFileRef>,
}

impl TerminalCaptureWorkerAck {
    /// A failure answer, carrying no path and no length.
    pub fn failed(error: TerminalCaptureErrorCode) -> Self {
        Self {
            status: TerminalCaptureStatus::Error,
            path: None,
            byte_length: None,
            error: Some(error),
            expires_at_ms: None,
            recent_worker_capture: None,
        }
    }

    /// Name the last incident the worker froze, unless it is this one.
    pub fn with_recent_worker_capture(
        mut self,
        recent: Option<TerminalCaptureFileRef>,
        answering_capture_id: &str,
    ) -> Self {
        self.recent_worker_capture = recent
            .filter(|capture| capture.capture_id != answering_capture_id);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{
        TERMINAL_CAPTURE_EVIDENCE_MAX_CHARS, TERMINAL_CAPTURE_LIMITS,
        has_at_most_browser_evidence_bytes,
    };

    #[test]
    fn the_evidence_cap_admits_its_last_byte_and_refuses_the_next() {
        let at_limit = "e".repeat(TERMINAL_CAPTURE_EVIDENCE_MAX_CHARS);
        assert!(has_at_most_browser_evidence_bytes(&at_limit));
        assert!(!has_at_most_browser_evidence_bytes(&format!("{at_limit}e")));
    }

    #[test]
    fn the_evidence_cap_is_measured_in_bytes_not_characters() {
        // A string of 512 KiB multi-byte characters is over the cap long before
        // it reaches the cap's character count, so a wire bound counted in
        // characters would have to be stricter, never looser.
        let multibyte = "é".repeat(TERMINAL_CAPTURE_EVIDENCE_MAX_CHARS);
        assert_eq!(
            multibyte.chars().count(),
            TERMINAL_CAPTURE_EVIDENCE_MAX_CHARS
        );
        assert!(!has_at_most_browser_evidence_bytes(&multibyte));
    }

    #[test]
    fn the_evidence_cap_admits_the_cap_and_refuses_one_byte_more() {
        // The pairing of the two caps is a compile-time fact between two
        // `const`s, so what is worth testing is the predicate at its own
        // boundary: a payload at the cap is admitted and one byte more is not.
        let at_the_cap = "e".repeat(TERMINAL_CAPTURE_LIMITS.browser_evidence_bytes);
        assert!(has_at_most_browser_evidence_bytes(&at_the_cap));
        assert!(!has_at_most_browser_evidence_bytes(&format!(
            "{at_the_cap}x"
        )));
    }
}
