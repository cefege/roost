//! `CoordinatorService.DiagSnapshot` — the on-demand state dump: the
//! coordinator's per-session view, every admitted worker's own snapshot and
//! pipeline sample, and whatever the SPA sent, as one JSON document.
//!
//! Ports `diagSnapshot` and `normalizeDiagSnapshotSessionFilterIds` of
//! `apps/coord/src/rpc/handlers-system.ts`. Called by the `diag_snapshot` arm
//! of `rpc/service_impl.rs`; assembles `diagnostics::session_state` and
//! `diagnostics::worker_results`.
//!
//! SCOPE IS DURABLE FIRST. Open session rows joined to live (non-deleted)
//! worker rows decide which sessions and workers the dump may mention; the
//! volatile registry only narrows that to the workers a request may be sent
//! to. A filtered dump names at most 64 sessions; an unfiltered one is capped
//! at the same bound and says `truncated` when it hit it.
//!
//! A TERMINAL CAPTURE REQUEST IS NOT A DUMP. It is one authenticated capture
//! step on exactly one session (`terminal_capture::bridge`), answered with the
//! capture result and nothing else, after v2's own scope validation.

use std::collections::{BTreeSet, HashSet};

use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_host::build_identity::{COMPILED_ROOST_ARTIFACT_VERSION, DEV_BUILD_STAMP};
use roost_proto as proto;
use roost_protocol::wire::WorkerFp;
use serde_json::{Map, Value, json};
use sqlx::AnyPool;

use crate::auth::principal::require_account_device;
use crate::coord_core::{Caller, CoordCore};
use crate::db::{IN_LIST_CHUNK, SqlBuilder, push_in_list};
use crate::diagnostics::session_state::{
    DiagSessionRow, DiagSessionScope, coord_session_diagnostic,
};
use crate::diagnostics::worker_results::{
    ScopedDiagnosticSession, ScopedWorkerDiagnosticOptions, collect_scoped_worker_diagnostics,
};
use crate::rpc::service::ok_response;
use crate::terminal_capture::bridge::CaptureBridge;
use crate::terminal_screen::pipeline_cache::WorkerTerminalPipelineSnapshotCache;

/// The most sessions one dump names, filtered or not.
pub const DIAG_SNAPSHOT_MAX_SESSION_FILTER_IDS: usize = 64;

/// `CoordinatorService.DiagSnapshot`. `pipelines` is the handler-lifetime
/// pipeline sample cache; `git_sha` is this process's build commit.
pub async fn handle_diag_snapshot(
    core: &CoordCore,
    caller: &Caller,
    pipelines: &WorkerTerminalPipelineSnapshotCache,
    git_sha: &str,
    request: proto::DiagSnapshotRequest,
) -> ServiceResult<proto::DiagSnapshotResponse> {
    require_account_device(caller)?;
    if let Some(capture) = request.terminal_capture.as_option() {
        assert_terminal_capture_session_filter(
            &request.session_filter_id,
            &request.session_filter_ids,
            &capture.session_id,
        )?;
        let bridge = CaptureBridge {
            services: &core.services,
            runtime: &core.services.terminal_capture,
            git_sha,
        };
        let terminal_capture = bridge.handle(capture, &caller.principal).await?;
        return ok_response(proto::DiagSnapshotResponse {
            snapshot_json: json!({
                "captured_at_ms": core.services.terminal_capture.now_ms(),
                "terminal_capture": terminal_capture,
            })
            .to_string(),
            ..Default::default()
        });
    }
    let filter_ids =
        normalize_session_filter_ids(&request.session_filter_id, &request.session_filter_ids)?;
    let services = &core.services;
    let captured_at_ms = services.scrollback.now_ms();
    let scope = read_scope(services.db.pool(), &filter_ids)
        .await
        .map_err(|error| ConnectError::new(ErrorCode::Internal, error.to_string()))?;

    let dispatchable: BTreeSet<WorkerFp> = scope
        .allowed_worker_fps
        .iter()
        .filter(|worker_fp| services.workers.current_routable(worker_fp).is_some())
        .cloned()
        .collect();
    let session_scope = DiagSessionScope {
        allowed_worker_fps: &scope.allowed_worker_fps,
        dispatchable_worker_fps: &dispatchable,
    };
    let now_ms = u64::try_from(captured_at_ms).unwrap_or_default();
    let sessions: Map<String, Value> = scope
        .sessions
        .iter()
        .map(|row| {
            let diagnostic = coord_session_diagnostic(services, row, session_scope, now_ms);
            (row.id.clone(), diagnostic)
        })
        .collect();

    let scoped_sessions: Vec<ScopedDiagnosticSession> = scope
        .sessions
        .iter()
        .map(|row| ScopedDiagnosticSession {
            id: row.id.clone(),
            worker_fp: row.worker_fp.clone(),
        })
        .collect();
    let allowed_session_ids: HashSet<String> =
        scope.sessions.iter().map(|row| row.id.clone()).collect();
    let workers = collect_scoped_worker_diagnostics(
        &services.scrollback,
        pipelines,
        ScopedWorkerDiagnosticOptions {
            worker_fps: &dispatchable,
            sessions: &scoped_sessions,
            allowed_session_ids: &allowed_session_ids,
        },
    )
    .await;

    let spa = if request.spa_state_json.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&request.spa_state_json).unwrap_or(Value::Null)
    };
    let mut snapshot = json!({
        "captured_at_ms": captured_at_ms,
        "coord": {
            "build": {
                "git_sha": git_sha,
                "artifact_version": COMPILED_ROOST_ARTIFACT_VERSION.unwrap_or(DEV_BUILD_STAMP),
            },
            "sessions": sessions,
        },
        "workers": workers,
        "spa": spa,
    });
    if scope.truncated
        && let Value::Object(fields) = &mut snapshot
    {
        fields.insert("truncated".to_owned(), Value::Bool(true));
    }
    let snapshot_json = snapshot.to_string();
    tracing::info!(
        src = "coord",
        snapshot_size = snapshot_json.len(),
        "diag.snapshot"
    );
    ok_response(proto::DiagSnapshotResponse {
        snapshot_json,
        ..Default::default()
    })
}

/// v2 `normalizeDiagSnapshotSessionFilterIds`: one of the two filter forms, at
/// most 64 ids, each nonempty and unique. Empty result = unfiltered.
pub fn normalize_session_filter_ids(
    session_filter_id: &str,
    session_filter_ids: &[String],
) -> Result<Vec<String>, ConnectError> {
    if !session_filter_id.is_empty() && !session_filter_ids.is_empty() {
        return Err(invalid(
            "diag snapshot session_filter_id and session_filter_ids cannot be combined",
        ));
    }
    if session_filter_ids.len() > DIAG_SNAPSHOT_MAX_SESSION_FILTER_IDS {
        return Err(invalid(
            "diag snapshot accepts at most 64 session_filter_ids",
        ));
    }
    let normalized: Vec<String> = if session_filter_id.is_empty() {
        session_filter_ids.to_vec()
    } else {
        vec![session_filter_id.to_owned()]
    };
    let unique: HashSet<&String> = normalized.iter().collect();
    if normalized.iter().any(String::is_empty) || unique.len() != normalized.len() {
        return Err(invalid(
            "diag snapshot session_filter_ids must be unique and nonempty",
        ));
    }
    Ok(normalized)
}

/// v2 `assertTerminalCaptureSessionFilter`: a capture names exactly one
/// session, twice, and the legacy scalar filter cannot express that.
fn assert_terminal_capture_session_filter(
    session_filter_id: &str,
    session_filter_ids: &[String],
    capture_session_id: &str,
) -> Result<(), ConnectError> {
    if !session_filter_id.is_empty() {
        return Err(invalid(
            "terminal capture requires session_filter_ids, not session_filter_id",
        ));
    }
    if session_filter_ids.len() != 1 || session_filter_ids[0] != capture_session_id {
        return Err(invalid(
            "terminal capture requires exactly one session_filter_ids entry matching terminal_capture.session_id",
        ));
    }
    Ok(())
}

fn invalid(message: &str) -> ConnectError {
    ConnectError::new(ErrorCode::InvalidArgument, message)
}

/// The durable boundary of one dump.
struct DiagScope {
    sessions: Vec<DiagSessionRow>,
    allowed_worker_fps: BTreeSet<WorkerFp>,
    truncated: bool,
}

async fn read_scope(pool: &AnyPool, filter_ids: &[String]) -> Result<DiagScope, sqlx::Error> {
    let filtered = !filter_ids.is_empty();
    let filter: Vec<&str> = filter_ids.iter().map(String::as_str).collect();
    let session_rows: Vec<(String, String, i64)> = fetch_live_rows(
        pool,
        "SELECT session.id, session.worker_fp, session.channel \
         FROM sessions AS session JOIN workers AS worker ON worker.fp = session.worker_fp \
         WHERE session.status = 'open' AND worker.deleted_at_ms IS NULL",
        "session.id",
        filtered.then_some(filter.as_slice()),
    )
    .await?;
    let session_row_count = session_rows.len();
    let sessions: Vec<DiagSessionRow> = session_rows
        .into_iter()
        .filter_map(
            |(id, worker_fp, channel)| match WorkerFp::try_from(worker_fp.as_str()) {
                Ok(worker_fp) => Some(DiagSessionRow {
                    id,
                    worker_fp,
                    channel,
                }),
                Err(_) => {
                    tracing::warn!(
                        session_id = id,
                        worker_fp,
                        "diag.snapshot: session row names a malformed worker fp; skipped"
                    );
                    None
                }
            },
        )
        .collect();
    let session_worker_fps: BTreeSet<WorkerFp> =
        sessions.iter().map(|row| row.worker_fp.clone()).collect();
    let worker_rows: Vec<(String,)> = if filtered && session_worker_fps.is_empty() {
        Vec::new()
    } else {
        let worker_fps: Vec<&str> = session_worker_fps.iter().map(WorkerFp::as_str).collect();
        fetch_live_rows(
            pool,
            "SELECT fp FROM workers WHERE deleted_at_ms IS NULL",
            "fp",
            filtered.then_some(worker_fps.as_slice()),
        )
        .await?
    };
    let truncated = !filtered
        && (session_row_count == DIAG_SNAPSHOT_MAX_SESSION_FILTER_IDS
            || worker_rows.len() == DIAG_SNAPSHOT_MAX_SESSION_FILTER_IDS);
    // A capped worker page can miss a worker that owns an admitted session; the
    // session join already proved it live, so it is seeded here.
    let mut allowed_worker_fps = session_worker_fps;
    allowed_worker_fps.extend(worker_rows.into_iter().filter_map(|(fp,)| {
        WorkerFp::try_from(fp.as_str())
            .inspect_err(|_| {
                tracing::warn!(
                    worker_fp = fp,
                    "diag.snapshot: worker row names a malformed fp; skipped"
                );
            })
            .ok()
    }));
    Ok(DiagScope {
        sessions,
        allowed_worker_fps,
        truncated,
    })
}

/// The rows `select` yields: every one whose `id_column` is in `filter`, or the
/// first [`DIAG_SNAPSHOT_MAX_SESSION_FILTER_IDS`] when there is no filter.
async fn fetch_live_rows<Row>(
    pool: &AnyPool,
    select: &'static str,
    id_column: &'static str,
    filter: Option<&[&str]>,
) -> Result<Vec<Row>, sqlx::Error>
where
    Row: for<'row> sqlx::FromRow<'row, sqlx::any::AnyRow> + Send + Unpin,
{
    let Some(filter) = filter else {
        let cap = DIAG_SNAPSHOT_MAX_SESSION_FILTER_IDS as i64;
        let mut statement = SqlBuilder::new(select);
        statement.push(" LIMIT ").push_bind(cap);
        return statement.build_query_as().fetch_all(pool).await;
    };
    let mut rows = Vec::with_capacity(filter.len());
    for chunk in filter.chunks(IN_LIST_CHUNK) {
        let mut statement = SqlBuilder::new(select);
        statement.push(" AND ").push(id_column).push(" IN ");
        push_in_list(&mut statement, chunk);
        rows.extend(statement.build_query_as().fetch_all(pool).await?);
    }
    Ok(rows)
}
