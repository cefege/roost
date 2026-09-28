//! The JSON shape of one terminal incident bundle — the owner-only
//! `terminal-incident-<capture-id>.json.gz` a worker writes — its closed
//! literal sets, and the file naming its storage owner may delete by. Ports
//! `packages/protocol/src/terminal-capture-bundle.ts` and the reason list and
//! file names of `terminal-capture.ts`. Built by `roost_worker::capture`,
//! checked by [`super::validate`]; uint64 offsets are decimal STRINGS.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::frame_json::{serialize_rows, serialize_shared_frame};
use super::view::TerminalCanonicalDifference;
use crate::cell::{CellGridFrame, CellRow};
use crate::viewport::{TerminalGeometry, is_terminal_uuid};

/// The schema literal every bundle and every evidence envelope carries.
pub const TERMINAL_INCIDENT_SCHEMA: &str = "roost.terminal-incident.v1";

/// Which process authored a piece of evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalCaptureLayer {
    Browser,
    Coordinator,
    Worker,
}

impl TerminalCaptureLayer {
    /// The literal a bundle spells the layer with, which is also the member
    /// name its section nests under.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Browser => "browser",
            Self::Coordinator => "coordinator",
            Self::Worker => "worker",
        }
    }
}

/// Why a capture was taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalCaptureReason {
    Manual,
    HistoryIdentity,
    ViewportModel,
    WorkerEmission,
    PreRepair,
}

/// v2 `TERMINAL_CAPTURE_REASONS`, in its order.
pub const TERMINAL_CAPTURE_REASONS: [TerminalCaptureReason; 5] = [
    TerminalCaptureReason::Manual,
    TerminalCaptureReason::HistoryIdentity,
    TerminalCaptureReason::ViewportModel,
    TerminalCaptureReason::WorkerEmission,
    TerminalCaptureReason::PreRepair,
];

impl TerminalCaptureReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::HistoryIdentity => "history_identity",
            Self::ViewportModel => "viewport_model",
            Self::WorkerEmission => "worker_emission",
            Self::PreRepair => "pre_repair",
        }
    }

    /// v2 `isTerminalCaptureReason`: the reason a literal names, if any.
    pub fn parse(value: &str) -> Option<Self> {
        TERMINAL_CAPTURE_REASONS
            .into_iter()
            .find(|reason| reason.as_str() == value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalCaptureCoverage {
    Complete,
    Partial,
    Unavailable,
}

/// Machine-readable reason a replay boundary is missing. Never free text: a
/// parser message could carry terminal content into a console.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalCoverageReason {
    Complete,
    LayerUnavailable,
    MissingInitialPrefix,
    RawPrefixEvicted,
    MissingResizeBoundary,
    CoreExportUnavailable,
    GridBudgetExceeded,
    SampleBudgetExceeded,
    BaselineInvalidated,
    SegmentEvicted,
    FrameOverBudget,
    EvidenceTrimmed,
    CaptureExpired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalCaptureCoverageReport {
    pub cell_replay: TerminalCaptureCoverage,
    pub cell_replay_reasons: Vec<TerminalCoverageReason>,
    pub core_replay: TerminalCaptureCoverage,
    pub core_replay_reasons: Vec<TerminalCoverageReason>,
    /// Sampled-checkpoint equality proves only its own checkpoints.
    pub core_comparison: TerminalCaptureCoverage,
    pub core_comparison_reasons: Vec<TerminalCoverageReason>,
}

/// Identity of the process that produced one layer's evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalCaptureProcessIdentity {
    pub layer: TerminalCaptureLayer,
    /// Per-process id; distinguishes a restart.
    pub process_id: String,
    pub git_sha: String,
    pub artifact_version: String,
    /// Worker only: the pinned terminal-core identity.
    pub wasm_identity: Option<String>,
    pub worker_fp: Option<String>,
    pub viewer_id: Option<String>,
    pub user_agent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalCaptureStreamIdentity {
    pub stream_id: String,
    pub grid_epoch: String,
    /// Decimal uint64.
    pub seq: String,
    pub base_seq: Option<String>,
    pub cols: u32,
    pub rows: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalCaptureDropCounters {
    pub records: u64,
    pub bytes: u64,
    pub rows: u64,
    pub raw_bytes: u64,
    pub samples: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalCaptureOmissionKind {
    Section,
    Records,
    Rows,
    Raw,
    Sample,
}

/// An absolute decimal range, end exclusive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalCaptureOffsetRange {
    pub start: String,
    pub end: String,
}

/// One thing this bundle does NOT contain, named exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalCaptureOmission {
    pub kind: TerminalCaptureOmissionKind,
    pub name: String,
    pub reason: TerminalCoverageReason,
    pub dropped_count: u64,
    pub dropped_bytes: u64,
    pub range: Option<TerminalCaptureOffsetRange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalCaptureRangeStatus {
    Present,
    Evicted,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalCaptureHistoryRange {
    pub start: String,
    pub end: String,
    pub status: TerminalCaptureRangeStatus,
    pub rows: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalWorkerSegmentOpenReason {
    Armed,
    StreamChange,
    EpochChange,
    CoreRebuild,
}

/// One worker stream generation × core incarnation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalWorkerSegment {
    pub segment_id: String,
    pub stream_id: String,
    pub grid_epoch: String,
    pub core_incarnation: u64,
    pub opened_at_ms: u64,
    pub closed_at_ms: Option<u64>,
    pub open_reason: TerminalWorkerSegmentOpenReason,
    pub geometry: TerminalGeometry,
    /// Absolute raw byte offset at segment open, decimal.
    pub open_offset: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalWorkerComparison {
    Equal,
    Different,
    Unsampled,
    BudgetSkipped,
    BaselineInvalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TerminalWorkerEmissionRecord {
    pub segment_id: String,
    pub emitted_at_ms: u64,
    pub stream: TerminalCaptureStreamIdentity,
    pub full: bool,
    /// The exact accepted full or delta, as emitted.
    #[serde(serialize_with = "serialize_shared_frame")]
    pub frame: Arc<CellGridFrame>,
    pub comparison: TerminalWorkerComparison,
    pub difference: Option<TerminalCanonicalDifference>,
}

/// A fresh viewport-only core scan at one emission's generation and sequence,
/// plus the emitted-frame fold it was compared against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TerminalWorkerCoreSampleRecord {
    pub segment_id: String,
    pub sampled_at_ms: u64,
    pub stream: TerminalCaptureStreamIdentity,
    pub elapsed_us: u64,
    #[serde(serialize_with = "serialize_shared_frame")]
    pub core_frame: Arc<CellGridFrame>,
    #[serde(serialize_with = "serialize_shared_frame")]
    pub fold_frame: Arc<CellGridFrame>,
    pub comparison: TerminalWorkerComparison,
    pub difference: Option<TerminalCanonicalDifference>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalWorkerSamplingStats {
    pub sampled: u64,
    pub skipped_interval: u64,
    pub skipped_budget: u64,
    pub skipped_grid: u64,
    pub suppressed_until_ms: Option<u64>,
    pub max_elapsed_us: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalWorkerResizeOutcome {
    Accepted,
    Rejected,
    LostAck,
    Recovered,
    CoreFailed,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalWorkerResizeRecord {
    pub segment_id: String,
    pub at_ms: u64,
    pub resize_seq: u64,
    /// Absolute raw offset when the capture gate was installed.
    pub install_offset: String,
    /// The keeper-acknowledged parse boundary; null when never proven.
    pub boundary_offset: Option<String>,
    pub from: TerminalGeometry,
    pub to: TerminalGeometry,
    pub outcome: TerminalWorkerResizeOutcome,
    pub grid_epoch_before: String,
    pub grid_epoch_after: Option<String>,
    pub captured_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalWorkerRawRecord {
    pub segment_id: String,
    pub at_ms: u64,
    /// Absolute raw byte offsets, decimal, end exclusive.
    pub start_offset: String,
    pub end_offset: String,
    pub base64: String,
}

/// The raw tail of the always-on byte window, for a capture no recorder held.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalWorkerByteCaptureTail {
    pub end_offset: String,
    pub start_offset: String,
    pub byte_length: u64,
    pub base64: String,
}

/// The worker's own layer. `layer` always serializes as `"worker"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TerminalWorkerSection {
    pub layer: TerminalCaptureLayer,
    pub captured_at_ms: u64,
    pub process: TerminalCaptureProcessIdentity,
    pub stream: Option<TerminalCaptureStreamIdentity>,
    pub geometry: Option<TerminalGeometry>,
    pub dropped: TerminalCaptureDropCounters,
    pub omissions: Vec<TerminalCaptureOmission>,
    pub segments: Vec<TerminalWorkerSegment>,
    pub emissions: Vec<TerminalWorkerEmissionRecord>,
    pub core_samples: Vec<TerminalWorkerCoreSampleRecord>,
    pub sampling: TerminalWorkerSamplingStats,
    pub resizes: Vec<TerminalWorkerResizeRecord>,
    pub raw: Vec<TerminalWorkerRawRecord>,
    pub byte_capture: Option<TerminalWorkerByteCaptureTail>,
    #[serde(serialize_with = "serialize_rows")]
    pub core_scrollback_tail: Vec<CellRow>,
    #[serde(serialize_with = "serialize_rows")]
    pub history_rows: Vec<CellRow>,
    pub history_ranges: Vec<TerminalCaptureHistoryRange>,
    pub scrollback_total: u64,
    pub scrollback_origin: String,
}

/// What fired a capture. `detail` is a fixed invariant token, never text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalCaptureTrigger {
    pub reason: TerminalCaptureReason,
    pub origin: TerminalCaptureLayer,
    pub at_ms: u64,
    pub stream_id: Option<String>,
    pub grid_epoch: Option<String>,
    pub seq: Option<String>,
    pub detail: Option<String>,
    /// Further same-identity occurrences the latch collapsed.
    pub occurrence_count: u64,
}

pub const TERMINAL_CAPTURE_FILE_PREFIX: &str = "terminal-incident-";
pub const TERMINAL_CAPTURE_FILE_SUFFIX: &str = ".json.gz";

/// The capture UUID is the whole name: a client never chooses a path.
pub fn terminal_capture_file_name(capture_id: &str) -> String {
    format!("{TERMINAL_CAPTURE_FILE_PREFIX}{capture_id}{TERMINAL_CAPTURE_FILE_SUFFIX}")
}

/// True for a name the storage owner created, so retention can never unlink a
/// neighbour's log.
pub fn is_terminal_capture_file_name(name: &str) -> bool {
    name.strip_prefix(TERMINAL_CAPTURE_FILE_PREFIX)
        .and_then(|rest| rest.strip_suffix(TERMINAL_CAPTURE_FILE_SUFFIX))
        .is_some_and(is_terminal_uuid)
}
