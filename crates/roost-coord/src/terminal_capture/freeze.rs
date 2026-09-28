//! Freeze one armed session's coordinator records into the bounded
//! `coordinator` evidence payload a CAPTURE forwards to the worker. Runs
//! synchronously, before the bridge awaits anything, so the evidence describes
//! the incident rather than whatever the terminal did while the worker
//! answered. Called by `terminal_capture::bridge`.
//! Ports `freezeCoordinatorEvidence` and `selectRecordsWithinBudget` of
//! `apps/coord/src/terminal/capture/terminal-capture-recorder.ts`.

use roost_host::build_identity::{COMPILED_ROOST_ARTIFACT_VERSION, DEV_BUILD_STAMP};
use roost_protocol::terminal_capture::TERMINAL_CAPTURE_LIMITS;
use roost_protocol::terminal_capture::bundle::{
    TERMINAL_INCIDENT_SCHEMA, TerminalCaptureDropCounters, TerminalCaptureLayer,
    TerminalCaptureOffsetRange, TerminalCaptureOmission, TerminalCaptureOmissionKind,
    TerminalCaptureProcessIdentity, TerminalCoverageReason,
};
use roost_protocol::terminal_capture::coordinator::{
    TerminalCaptureCoordinatorPayload, TerminalCoordinatorRecord, TerminalCoordinatorSection,
};
use roost_protocol::viewport::TerminalGeometry;

use crate::terminal_capture::recorder::{CoordinatorRecorder, RecordedFrame, SequenceRange};

/// Room for the section header, its process identity and its omission list,
/// so selection never needs a second pass after an omission it produced.
const SECTION_HEADER_RESERVE: usize = 2_048;

/// The frozen payload, or `json` empty when the layer is unavailable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoordinatorEvidence {
    pub json: String,
    pub records: usize,
    pub dropped: u64,
    pub bytes: usize,
    pub available: bool,
}

/// Who froze the evidence: the build this coordinator runs.
#[derive(Debug, Clone, Copy)]
pub struct FreezeContext<'a> {
    pub git_sha: &'a str,
    pub captured_at_ms: u64,
}

struct Selection<'a> {
    records: Vec<&'a TerminalCoordinatorRecord>,
    dropped: u64,
    dropped_bytes: u64,
    range: Option<SequenceRange>,
}

impl CoordinatorRecorder {
    /// Freeze one session's evidence for `capture_id`, when armed for
    /// `recording_id`; another recording's evidence is never readable.
    #[must_use]
    pub fn freeze(
        &self,
        session_id: &str,
        capture_id: &str,
        recording_id: &str,
        context: FreezeContext<'_>,
    ) -> CoordinatorEvidence {
        let armed = self.lock();
        let Some(session) = armed
            .get(session_id)
            .filter(|session| session.recording_id == recording_id)
        else {
            return CoordinatorEvidence::default();
        };
        let budget = TERMINAL_CAPTURE_LIMITS.coordinator_evidence_bytes;
        let trimmed = select_records_within_budget(&session.frames, budget - SECTION_HEADER_RESERVE);
        let dropped = TerminalCaptureDropCounters {
            records: session.dropped_records + trimmed.dropped,
            bytes: session.dropped_bytes + trimmed.dropped_bytes,
            rows: session.over_budget_rows,
            raw_bytes: 0,
            samples: 0,
        };
        let mut omissions = Vec::new();
        if session.dropped_records != 0 {
            omissions.push(omission(
                TerminalCaptureOmissionKind::Records,
                "coordinator.records",
                TerminalCoverageReason::SegmentEvicted,
                (session.dropped_records, session.dropped_bytes),
                session.dropped_range.clone(),
            ));
        }
        if trimmed.dropped != 0 {
            omissions.push(omission(
                TerminalCaptureOmissionKind::Records,
                "coordinator.records",
                TerminalCoverageReason::EvidenceTrimmed,
                (trimmed.dropped, trimmed.dropped_bytes),
                trimmed.range.clone(),
            ));
        }
        if session.over_budget_records != 0 {
            omissions.push(omission(
                TerminalCaptureOmissionKind::Rows,
                "coordinator.records[].canonical",
                TerminalCoverageReason::FrameOverBudget,
                (session.over_budget_records, 0),
                None,
            ));
        }
        let last = session.frames.last().map(|frame| &frame.record);
        let section = TerminalCoordinatorSection {
            layer: TerminalCaptureLayer::Coordinator,
            captured_at_ms: context.captured_at_ms,
            process: TerminalCaptureProcessIdentity {
                layer: TerminalCaptureLayer::Coordinator,
                process_id: self.process_id.clone(),
                git_sha: context.git_sha.to_owned(),
                artifact_version: COMPILED_ROOST_ARTIFACT_VERSION
                    .unwrap_or(DEV_BUILD_STAMP)
                    .to_owned(),
                wasm_identity: None,
                worker_fp: None,
                viewer_id: None,
                user_agent: None,
            },
            stream: last.map(|record| record.stream.clone()),
            geometry: last.map(|record| TerminalGeometry {
                cols: record.stream.cols,
                rows: record.stream.rows,
            }),
            dropped,
            omissions,
            records: trimmed.records.iter().map(|record| (*record).clone()).collect(),
            snapshot: last.map(|record| record.stream.clone()),
            // The retained evidence ends on a complete canonical checkpoint.
            valid: last.is_some_and(|record| record.canonical.is_some()),
        };
        let records = section.records.len();
        let payload = TerminalCaptureCoordinatorPayload {
            schema: TERMINAL_INCIDENT_SCHEMA,
            layer: TerminalCaptureLayer::Coordinator,
            capture_id,
            recording_id,
            session_id,
            coordinator: section,
        };
        let json = match serde_json::to_string(&payload) {
            Ok(json) if json.len() <= budget => json,
            Ok(_) | Err(_) => {
                tracing::warn!(session_id, capture_id, "terminal capture: coordinator evidence unavailable");
                return CoordinatorEvidence {
                    dropped: session.frames.len() as u64,
                    ..CoordinatorEvidence::default()
                };
            }
        };
        CoordinatorEvidence {
            bytes: json.len(),
            json,
            records,
            dropped: dropped.records,
            available: true,
        }
    }
}

fn omission(
    kind: TerminalCaptureOmissionKind,
    name: &str,
    reason: TerminalCoverageReason,
    (dropped_count, dropped_bytes): (u64, u64),
    range: Option<SequenceRange>,
) -> TerminalCaptureOmission {
    TerminalCaptureOmission {
        kind,
        name: name.to_owned(),
        reason,
        dropped_count,
        dropped_bytes,
        range: range.map(|range| TerminalCaptureOffsetRange {
            start: range.start,
            end: range.end,
        }),
    }
}

/// Newest records first, so a capture keeps the evidence around the incident,
/// then restored to chronological order. A leading record with no canonical
/// cannot anchor a replay, so it is dropped with the records before it.
fn select_records_within_budget(frames: &[RecordedFrame], budget: usize) -> Selection<'_> {
    let mut kept = 0_usize;
    let mut used = 2_usize;
    for frame in frames.iter().rev() {
        let cost = serde_json::to_string(&frame.record).map_or(usize::MAX, |json| json.len() + 1);
        if used.saturating_add(cost) > budget {
            break;
        }
        used += cost;
        kept += 1;
    }
    let mut first = frames.len() - kept;
    while first < frames.len() && frames[first].record.canonical.is_none() {
        first += 1;
    }
    let dropped_frames = &frames[..first];
    Selection {
        records: frames[first..].iter().map(|frame| &frame.record).collect(),
        dropped: dropped_frames.len() as u64,
        dropped_bytes: dropped_frames.iter().map(|frame| frame.bytes as u64).sum(),
        range: match (dropped_frames.first(), dropped_frames.last()) {
            (Some(start), Some(end)) => Some(SequenceRange {
                start: start.record.stream.seq.clone(),
                end: end.record.stream.seq.clone(),
            }),
            _ => None,
        },
    }
}
