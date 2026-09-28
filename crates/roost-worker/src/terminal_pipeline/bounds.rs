//! Deterministic admission, numeric normalization, fixed age buckets and exact
//! response-size fencing for worker terminal-pipeline evidence. Ports
//! `apps/worker/src/terminal/terminal-pipeline-snapshot-bounds.ts`. Called by
//! `terminal_pipeline::snapshot`; depends only on the generated wire messages.
//! It holds no terminal, keeper or generic diagnostic content.

use std::cmp::Ordering;

use roost_proto::buffa::Message as _;
use roost_proto::{
    DTerminalPipelineSnapshotRequest, TerminalPipelineReason, TerminalPipelineSessionSnapshot,
    TerminalPipelineStage, TerminalPipelineStageSnapshot, TerminalPipelineTarget,
    WTerminalPipelineSnapshot,
};

use crate::link_ports::LinkPipelineState;

pub const TERMINAL_PIPELINE_MAX_TARGETS: usize = 64;
pub const TERMINAL_PIPELINE_MAX_RESPONSE_BYTES: usize = 64 * 1024;
pub const TERMINAL_PIPELINE_MAX_HISTOGRAM_BUCKETS: usize = 16;
/// Tag 19 and its nested length use at most five bytes; conservative headroom.
const TERMINAL_PIPELINE_MAX_ENVELOPE_BYTES: usize = 8;
/// Target ids are echoed into every record, so they are capped before encoding.
pub const TERMINAL_PIPELINE_MAX_IDENTIFIER_BYTES: usize = 512;
pub const TERMINAL_PIPELINE_MAX_REQUEST_ID_BYTES: usize = 256;
/// v2 numbers are JavaScript numbers: every count is held to an exact integer.
pub const TERMINAL_PIPELINE_MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;
/// Inclusive upper bounds of the first fifteen buckets; the sixteenth is +Inf.
const HISTOGRAM_UPPER_BOUND_MS: [u64; TERMINAL_PIPELINE_MAX_HISTOGRAM_BUCKETS - 1] = [
    1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1_000, 2_000, 5_000, 10_000, 30_000,
];

/// One admitted target and its request position, the sort's last tie-break.
#[derive(Debug, Clone, Copy)]
pub struct OrderedTarget<'a> {
    pub target: &'a TerminalPipelineTarget,
    pub ordinal: usize,
}

/// The fields one stage record carries; unset numbers are zero.
#[derive(Debug, Clone, Default)]
pub struct StageInput {
    pub generation: u64,
    pub stream_id: String,
    pub sequence: u64,
    pub queue_frames: u64,
    pub queue_bytes: u64,
    pub native_buffered_bytes: u64,
    pub oldest_age_ms: u64,
    pub count: u64,
    pub histogram_buckets: Vec<u64>,
}

/// The first sixty-four targets, ordered by session id, view id, then request
/// position. Ids compare by UTF-16 code unit, as v2's `<` does.
pub fn admit_targets(request: &DTerminalPipelineSnapshotRequest) -> Vec<OrderedTarget<'_>> {
    let mut admitted: Vec<OrderedTarget<'_>> = request
        .targets
        .iter()
        .take(TERMINAL_PIPELINE_MAX_TARGETS)
        .enumerate()
        .map(|(ordinal, target)| OrderedTarget { target, ordinal })
        .collect();
    admitted.sort_by(|left, right| {
        compare_ids(&left.target.session_id, &right.target.session_id)
            .then_with(|| compare_ids(&left.target.view_id, &right.target.view_id))
            .then(left.ordinal.cmp(&right.ordinal))
    });
    admitted
}

pub fn dropped_targets(request: &DTerminalPipelineSnapshotRequest, admitted: usize) -> u32 {
    bounded_count(request.targets.len().saturating_sub(admitted) as u64)
}

pub fn target_identifiers_are_bounded(target: &TerminalPipelineTarget) -> bool {
    identifier_is_bounded(&target.session_id) && identifier_is_bounded(&target.view_id)
}

pub fn bounded_identifier(value: &str) -> String {
    if identifier_is_bounded(value) {
        value.to_owned()
    } else {
        String::new()
    }
}

pub fn bounded_request_id(request_id: &str) -> String {
    if request_id.len() <= TERMINAL_PIPELINE_MAX_REQUEST_ID_BYTES {
        request_id.to_owned()
    } else {
        String::new()
    }
}

pub fn stage(
    stage: TerminalPipelineStage,
    reason: TerminalPipelineReason,
    input: StageInput,
) -> TerminalPipelineStageSnapshot {
    let mut histogram_buckets = input.histogram_buckets;
    histogram_buckets.truncate(TERMINAL_PIPELINE_MAX_HISTOGRAM_BUCKETS);
    TerminalPipelineStageSnapshot {
        stage: stage.into(),
        reason: reason.into(),
        generation: nonnegative_integer(input.generation),
        stream_id: input.stream_id,
        sequence: nonnegative_integer(input.sequence),
        queue_frames: nonnegative_integer(input.queue_frames),
        queue_bytes: nonnegative_integer(input.queue_bytes),
        native_buffered_bytes: nonnegative_integer(input.native_buffered_bytes),
        oldest_age_ms: nonnegative_integer(input.oldest_age_ms),
        count: nonnegative_integer(input.count),
        histogram_buckets,
        ..Default::default()
    }
}

/// Sixteen counts, or none at all when nothing was aged.
pub fn histogram_for_ages(ages: &[u64]) -> Vec<u64> {
    if ages.is_empty() {
        return Vec::new();
    }
    let mut buckets = vec![0; TERMINAL_PIPELINE_MAX_HISTOGRAM_BUCKETS];
    for age_ms in ages {
        buckets[histogram_index(*age_ms)] += 1;
    }
    buckets
}

pub fn histogram_index(age_ms: u64) -> usize {
    HISTOGRAM_UPPER_BOUND_MS
        .iter()
        .position(|upper_bound| age_ms <= *upper_bound)
        .unwrap_or(TERMINAL_PIPELINE_MAX_HISTOGRAM_BUCKETS - 1)
}

pub fn oldest_age(ages: &[u64]) -> u64 {
    ages.iter().copied().max().unwrap_or(0)
}

pub fn normalize_link_state(state: LinkPipelineState) -> LinkPipelineState {
    LinkPipelineState {
        queue_frames: nonnegative_integer(state.queue_frames),
        queue_bytes: nonnegative_integer(state.queue_bytes),
        native_buffered_bytes: nonnegative_integer(state.native_buffered_bytes),
        attached: state.attached,
    }
}

pub fn response(
    request_id: String,
    sessions: Vec<TerminalPipelineSessionSnapshot>,
    dropped_targets: u32,
    dropped_records: u32,
) -> WTerminalPipelineSnapshot {
    WTerminalPipelineSnapshot {
        request_id,
        sessions,
        dropped_targets,
        dropped_records,
        ..Default::default()
    }
}

/// The encoded response leaves room for the `CoordWorkerUp` envelope.
pub fn response_fits(snapshot: &WTerminalPipelineSnapshot) -> bool {
    snapshot.try_encoded_len().is_ok_and(|bytes| {
        bytes as usize
            <= TERMINAL_PIPELINE_MAX_RESPONSE_BYTES - TERMINAL_PIPELINE_MAX_ENVELOPE_BYTES
    })
}

pub fn bounded_count(value: u64) -> u32 {
    u32::try_from(nonnegative_integer(value)).unwrap_or(u32::MAX)
}

pub fn nonnegative_integer(value: u64) -> u64 {
    value.min(TERMINAL_PIPELINE_MAX_SAFE_INTEGER)
}

fn identifier_is_bounded(value: &str) -> bool {
    value.len() <= TERMINAL_PIPELINE_MAX_IDENTIFIER_BYTES
}

fn compare_ids(left: &str, right: &str) -> Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}
