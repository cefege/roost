//! The terminal-pipeline evidence boundary: the wire-shape check the worker
//! frame dispatcher runs before it settles a pending pipeline sample, target
//! normalization, and the reply-matches-request check the requester applies.
//! Ports the validation half of
//! `apps/coord/src/terminal/screen/worker-terminal-pipeline-snapshot.ts`.
//! Called by `worker_link::live_frames`, `pipeline_request` and `pipeline_cache`.

use std::collections::{BTreeMap, HashSet};

use roost_proto::buffa::Message;
use roost_proto::{
    TerminalPipelineReason, TerminalPipelineStage, TerminalPipelineStageSnapshot,
    WTerminalPipelineSnapshot,
};

/// The most (session, view) targets one sample request or reply carries.
pub const TERMINAL_PIPELINE_DIAG_MAX_TARGETS: usize = 64;

const TERMINAL_PIPELINE_MAX_RESPONSE_BYTES: u32 = 64 * 1024;
const TERMINAL_PIPELINE_MAX_REQUEST_ID_BYTES: usize = 256;
const TERMINAL_PIPELINE_MAX_IDENTIFIER_BYTES: usize = 512;
const TERMINAL_PIPELINE_MAX_STAGES_PER_SESSION: usize = 16;
const TERMINAL_PIPELINE_MAX_HISTOGRAM_BUCKETS: usize = 16;

/// One session-scoped sampling target: a durable session and the view that
/// asked about it (`""` when the sample is not view-correlated).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TerminalPipelineDiagnosticTarget {
    pub session_id: String,
    pub view_id: String,
}

impl TerminalPipelineDiagnosticTarget {
    pub fn new(session_id: impl Into<String>, view_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            view_id: view_id.into(),
        }
    }
}

/// Whether a worker's pipeline reply is inside every bound the coordinator
/// accepts. The frame dispatcher refuses to settle a pending sample with a
/// reply that fails this, so an oversized or malformed reply never reaches
/// the requester.
pub fn is_terminal_pipeline_snapshot_wire_shape(snapshot: &WTerminalPipelineSnapshot) -> bool {
    if !is_bounded_text(&snapshot.request_id, TERMINAL_PIPELINE_MAX_REQUEST_ID_BYTES) {
        return false;
    }
    if snapshot.sessions.len() > TERMINAL_PIPELINE_DIAG_MAX_TARGETS {
        return false;
    }
    let mut session_keys: HashSet<(&str, &str)> = HashSet::with_capacity(snapshot.sessions.len());
    for session in &snapshot.sessions {
        if !is_bounded_text(&session.session_id, TERMINAL_PIPELINE_MAX_IDENTIFIER_BYTES)
            || !is_bounded_text(&session.view_id, TERMINAL_PIPELINE_MAX_IDENTIFIER_BYTES)
            || session.stages.len() > TERMINAL_PIPELINE_MAX_STAGES_PER_SESSION
        {
            return false;
        }
        if !session_keys.insert((session.session_id.as_str(), session.view_id.as_str())) {
            return false;
        }
        if !session.stages.iter().all(is_terminal_pipeline_stage_shape) {
            return false;
        }
    }
    snapshot
        .try_encoded_len()
        .is_ok_and(|encoded_len| encoded_len <= TERMINAL_PIPELINE_MAX_RESPONSE_BYTES)
}

/// Drops targets with oversized identifiers, de-duplicates by (session, view),
/// orders by session then view, and keeps the first
/// [`TERMINAL_PIPELINE_DIAG_MAX_TARGETS`].
pub fn normalize_terminal_pipeline_diagnostic_targets(
    targets: &[TerminalPipelineDiagnosticTarget],
) -> Vec<TerminalPipelineDiagnosticTarget> {
    let unique_targets: BTreeMap<(&str, &str), &TerminalPipelineDiagnosticTarget> = targets
        .iter()
        .filter(|target| {
            is_bounded_text(&target.session_id, TERMINAL_PIPELINE_MAX_IDENTIFIER_BYTES)
                && is_bounded_text(&target.view_id, TERMINAL_PIPELINE_MAX_IDENTIFIER_BYTES)
        })
        .map(|target| {
            (
                (target.session_id.as_str(), target.view_id.as_str()),
                target,
            )
        })
        .collect();
    unique_targets
        .into_values()
        .take(TERMINAL_PIPELINE_DIAG_MAX_TARGETS)
        .cloned()
        .collect()
}

/// Whether a well-shaped reply answers exactly this request: same id, nothing
/// dropped on the target side, every returned session was asked for, and
/// returned plus dropped records account for every target.
pub fn terminal_pipeline_snapshot_matches_request(
    snapshot: &WTerminalPipelineSnapshot,
    request_id: &str,
    targets: &[TerminalPipelineDiagnosticTarget],
) -> bool {
    if !is_terminal_pipeline_snapshot_wire_shape(snapshot) || snapshot.request_id != request_id {
        return false;
    }
    let dropped_records = snapshot.dropped_records as usize;
    if snapshot.dropped_targets != 0 || dropped_records > targets.len() {
        return false;
    }
    let target_keys: HashSet<(&str, &str)> = targets
        .iter()
        .map(|target| (target.session_id.as_str(), target.view_id.as_str()))
        .collect();
    let mut returned_keys: HashSet<(&str, &str)> = HashSet::with_capacity(snapshot.sessions.len());
    for session in &snapshot.sessions {
        let key = (session.session_id.as_str(), session.view_id.as_str());
        if !target_keys.contains(&key) {
            return false;
        }
        returned_keys.insert(key);
    }
    returned_keys.len() + dropped_records == targets.len()
}

fn is_terminal_pipeline_stage_shape(stage: &TerminalPipelineStageSnapshot) -> bool {
    is_worker_pipeline_stage(stage)
        && is_pipeline_reason(stage)
        && is_bounded_text(&stage.stream_id, TERMINAL_PIPELINE_MAX_IDENTIFIER_BYTES)
        && stage.histogram_buckets.len() <= TERMINAL_PIPELINE_MAX_HISTOGRAM_BUCKETS
}

/// Only the worker-owned stages; an unknown number is refused rather than
/// range-compared, because it has no generated variant to compare.
fn is_worker_pipeline_stage(stage: &TerminalPipelineStageSnapshot) -> bool {
    let first = TerminalPipelineStage::WorkerPty as i32;
    let last = TerminalPipelineStage::WorkerCoordLink as i32;
    stage
        .stage
        .as_known()
        .is_some_and(|known| (first..=last).contains(&(known as i32)))
}

fn is_pipeline_reason(stage: &TerminalPipelineStageSnapshot) -> bool {
    let first = TerminalPipelineReason::None as i32;
    let last = TerminalPipelineReason::RawMetadataPending as i32;
    stage
        .reason
        .as_known()
        .is_some_and(|known| (first..=last).contains(&(known as i32)))
}

fn is_bounded_text(value: &str, maximum_bytes: usize) -> bool {
    value.len() <= maximum_bytes
}
