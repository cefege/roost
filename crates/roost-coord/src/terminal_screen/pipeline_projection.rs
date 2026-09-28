//! The JSON-safe, content-free projection of a validated terminal-pipeline
//! reply, scoped to the sessions the asking tenant may see. Every u64 counter
//! becomes a decimal string so a JSON reader cannot round it.
//! Ports `terminalPipelineDiagnosticSnapshot` of
//! `apps/coord/src/terminal/screen/worker-terminal-pipeline-snapshot.ts`.
//! Read by the DiagSnapshot worker-results projection.

use std::collections::HashSet;

use roost_proto::{TerminalPipelineStageSnapshot, WTerminalPipelineSnapshot};
use serde::Serialize;

/// One worker's sample as a diagnostic document carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TerminalPipelineDiagnosticSnapshot {
    pub request_id: String,
    pub dropped_targets: u32,
    pub dropped_records: u32,
    pub sessions: Vec<TerminalPipelineDiagnosticSession>,
}

/// One sampled (session, view) path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TerminalPipelineDiagnosticSession {
    pub session_id: String,
    pub view_id: String,
    pub stages: Vec<TerminalPipelineDiagnosticStage>,
}

/// One stage's counters; `stage` and `reason` are the generated enum numbers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TerminalPipelineDiagnosticStage {
    pub stage: i32,
    pub reason: i32,
    pub generation: String,
    pub stream_id: String,
    pub sequence: String,
    pub queue_frames: String,
    pub queue_bytes: String,
    pub native_buffered_bytes: String,
    pub oldest_age_ms: String,
    pub count: String,
    pub histogram_buckets: Vec<String>,
}

/// Converts a validated reply into diagnostic data, keeping only sessions in
/// `allowed_session_ids`.
#[must_use]
pub fn terminal_pipeline_diagnostic_snapshot(
    snapshot: &WTerminalPipelineSnapshot,
    allowed_session_ids: &HashSet<String>,
) -> TerminalPipelineDiagnosticSnapshot {
    TerminalPipelineDiagnosticSnapshot {
        request_id: snapshot.request_id.clone(),
        dropped_targets: snapshot.dropped_targets,
        dropped_records: snapshot.dropped_records,
        sessions: snapshot
            .sessions
            .iter()
            .filter(|session| allowed_session_ids.contains(&session.session_id))
            .map(|session| TerminalPipelineDiagnosticSession {
                session_id: session.session_id.clone(),
                view_id: session.view_id.clone(),
                stages: session.stages.iter().map(diagnostic_stage).collect(),
            })
            .collect(),
    }
}

fn diagnostic_stage(stage: &TerminalPipelineStageSnapshot) -> TerminalPipelineDiagnosticStage {
    TerminalPipelineDiagnosticStage {
        stage: stage.stage.to_i32(),
        reason: stage.reason.to_i32(),
        generation: stage.generation.to_string(),
        stream_id: stage.stream_id.clone(),
        sequence: stage.sequence.to_string(),
        queue_frames: stage.queue_frames.to_string(),
        queue_bytes: stage.queue_bytes.to_string(),
        native_buffered_bytes: stage.native_buffered_bytes.to_string(),
        oldest_age_ms: stage.oldest_age_ms.to_string(),
        count: stage.count.to_string(),
        histogram_buckets: stage.histogram_buckets.iter().map(u64::to_string).collect(),
    }
}
