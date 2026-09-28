//! The `workers` section of a DiagSnapshot: the generic `diag-snapshot` answer
//! and the typed terminal-pipeline sample of every dispatchable worker, both
//! cut down to the session ids the caller's durable scope admitted.
//! Ports `apps/coord/src/diagnostics/diag-snapshot-worker-results.ts`
//! (`createScopedWorkerDiagnosticCollector`). Called by
//! `diagnostics::diag_snapshot`; the pipeline cache is the handler-lifetime
//! one v2 creates beside the handlers.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use roost_protocol::wire::WorkerFp;
use serde_json::{Map, Value, json};

use crate::terminal_screen::pipeline_cache::WorkerTerminalPipelineSnapshotCache;
use crate::terminal_screen::pipeline_projection::terminal_pipeline_diagnostic_snapshot;
use crate::terminal_screen::pipeline_request::WorkerTerminalPipelineSnapshotResult;
use crate::terminal_screen::pipeline_snapshot::TerminalPipelineDiagnosticTarget;
use crate::terminal_screen::scrollback_relay::ScrollbackRelay;
use crate::workers::diag_send::{
    DIAG_SNAPSHOT_TIMEOUT_MS, WorkerDiagSnapshotResult, collect_worker_diag_snapshots,
};

/// One durable open session the handler admitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedDiagnosticSession {
    pub id: String,
    pub worker_fp: WorkerFp,
}

/// What the handler resolved before any worker is asked.
#[derive(Debug)]
pub struct ScopedWorkerDiagnosticOptions<'a> {
    /// Workers the durable scope admitted AND the registry can dispatch to.
    pub worker_fps: &'a BTreeSet<WorkerFp>,
    pub sessions: &'a [ScopedDiagnosticSession],
    pub allowed_session_ids: &'a HashSet<String>,
}

/// `workers[fp]` for every dispatchable worker: the generic envelope, plus a
/// `terminal_pipeline` envelope when that worker had pipeline targets.
pub async fn collect_scoped_worker_diagnostics(
    relay: &ScrollbackRelay,
    pipelines: &WorkerTerminalPipelineSnapshotCache,
    options: ScopedWorkerDiagnosticOptions<'_>,
) -> Map<String, Value> {
    let mut targets_by_worker: BTreeMap<WorkerFp, Vec<TerminalPipelineDiagnosticTarget>> =
        BTreeMap::new();
    for session in options.sessions {
        if !options.worker_fps.contains(&session.worker_fp) {
            continue;
        }
        // A coordinator diagnostic target, not a browser view claim.
        targets_by_worker
            .entry(session.worker_fp.clone())
            .or_default()
            .push(TerminalPipelineDiagnosticTarget::new(session.id.clone(), ""));
    }
    let (worker_snapshots, pipeline_results) = tokio::join!(
        collect_worker_diag_snapshots(
            relay,
            options.worker_fps.iter().cloned(),
            DIAG_SNAPSHOT_TIMEOUT_MS,
        ),
        pipelines.collect(&targets_by_worker, None),
    );
    worker_snapshots
        .into_iter()
        .map(|(worker_fp, result)| {
            let mut envelope =
                scoped_worker_diagnostic(&worker_fp, &result, options.allowed_session_ids);
            if let (Some(pipeline), Value::Object(fields)) =
                (pipeline_results.get(&worker_fp), &mut envelope)
            {
                fields.insert(
                    "terminal_pipeline".to_owned(),
                    scoped_pipeline_diagnostic(pipeline, options.allowed_session_ids),
                );
            }
            (worker_fp.as_str().to_owned(), envelope)
        })
        .collect()
}

/// An ok answer keeps only `captured_at_ms`, `build`, the authenticated
/// `worker_fp` and the admitted sessions; anything else the worker said is
/// dropped. An error envelope passes through unchanged.
#[must_use]
pub fn scoped_worker_diagnostic(
    worker_fp: &WorkerFp,
    result: &WorkerDiagSnapshotResult,
    allowed_session_ids: &HashSet<String>,
) -> Value {
    let WorkerDiagSnapshotResult::Ok {
        response_ms,
        snapshot,
    } = result
    else {
        return result.to_json();
    };
    let sessions: Map<String, Value> = match snapshot.get("sessions") {
        Some(Value::Object(sessions)) => sessions
            .iter()
            .filter(|(session_id, _)| allowed_session_ids.contains(session_id.as_str()))
            .map(|(session_id, value)| (session_id.clone(), value.clone()))
            .collect(),
        _ => Map::new(),
    };
    let mut scoped = Map::new();
    // v2 spreads `undefined` for an absent key, which JSON.stringify omits.
    for key in ["captured_at_ms", "build"] {
        if let Some(value) = snapshot.get(key) {
            scoped.insert(key.to_owned(), value.clone());
        }
    }
    scoped.insert("worker_fp".to_owned(), json!(worker_fp.as_str()));
    scoped.insert("sessions".to_owned(), Value::Object(sessions));
    json!({ "status": "ok", "response_ms": response_ms, "snapshot": scoped })
}

fn scoped_pipeline_diagnostic(
    result: &WorkerTerminalPipelineSnapshotResult,
    allowed_session_ids: &HashSet<String>,
) -> Value {
    match result {
        WorkerTerminalPipelineSnapshotResult::Ok {
            response_ms,
            snapshot,
        } => json!({
            "status": "ok",
            "response_ms": response_ms,
            "snapshot": terminal_pipeline_diagnostic_snapshot(snapshot, allowed_session_ids),
        }),
        WorkerTerminalPipelineSnapshotResult::Error {
            response_ms,
            code,
            message,
        } => json!({
            "status": "error",
            "response_ms": response_ms,
            "error": { "code": code.as_str(), "message": message },
        }),
    }
}
