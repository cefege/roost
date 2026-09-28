//! `SessionsList`, `SessionsAttach`, `SessionsKill` and `SessionsRename`, plus the
//! lease, viewer key, row lookups and coordinator-side append the sibling
//! `spawn`, `cursor_pos` and `assign_workspace` handlers share. Called from the
//! arms in `rpc/service_impl.rs`. Ports `apps/coord/src/sessions/handlers-sessions.ts`.
//!
//! A WORKER PRINCIPAL SEES ONLY ITS OWN OPEN SESSIONS. `SessionsList` is the one
//! session RPC a worker may call, for boot recovery, and only for exactly its
//! fingerprint, `status=open` and no Sync socket; every other session RPC stays
//! device-only (FAILURE-INDEX "A worker reconnects but respawns every terminal").

use std::sync::Arc;

use connectrpc::{ConnectError, ErrorCode, Response, ServiceResult};
use roost_proto::{
    SessionsAttachRequest, SessionsAttachResponse, SessionsKillRequest, SessionsKillResponse,
    SessionsListRequest, SessionsListResponse, SessionsRenameRequest, SessionsRenameResponse,
};
use roost_protocol::wire::control::ClientControlFrame;
use roost_protocol::wire::{SessionEvent, SessionId, WorkerFp};
use serde_json::Value;

use crate::attachments::relay::settle_within;
use crate::auth::principal::require_account_device;
use crate::coord_core::{Caller, CoordCore};
use crate::events::append::{AppendEventResult, AppendOptions, AtomicExtraWork};
use crate::rpc::service::now_ms;
use crate::sessions::list_projection::{
    SessionListScope, read_sessions_list_projection, session_status_filter,
};
use crate::terminal_screen::pending_rpcs::DEFAULT_PENDING_RPC_TIMEOUT_MS;
use crate::terminal_screen::rpc_relay::send_browser_command;
use crate::write_gate::SharedLease;

/// The longest custom title a rename keeps, in UTF-16 code units as v2 counts.
const CUSTOM_TITLE_MAX_UTF16: usize = 200;

/// List sessions: a browser's public rows, or a worker's own recovery rows.
pub async fn handle_sessions_list(
    core: &CoordCore,
    caller: &Caller,
    req: SessionsListRequest,
) -> ServiceResult<SessionsListResponse> {
    let (scope, snapshot_caller) = if caller.principal.is_worker() {
        let worker_fp = caller.fingerprint();
        let own_open_recovery = req.worker_fp.as_deref() == Some(worker_fp)
            && req.status.as_deref() == Some("open")
            && req.sync_socket_id.is_none();
        if !own_open_recovery {
            tracing::warn!(
                worker_fp,
                "sessions: a worker asked to list beyond its own open sessions"
            );
            return Err(ConnectError::new(
                ErrorCode::PermissionDenied,
                "worker session listing is restricted to its own open sessions",
            ));
        }
        (SessionListScope::OwnWorkerRecovery { worker_fp }, None)
    } else {
        let browser_fp = require_account_device(caller)?;
        let worker_fp = req.worker_fp.as_deref().filter(|fp| !fp.is_empty());
        (SessionListScope::Public { worker_fp }, Some(browser_fp))
    };
    let status = session_status_filter(req.status.as_deref())?;
    let projection = read_sessions_list_projection(&core.services.db, scope, status).await?;
    let socket_id = req.sync_socket_id.as_deref().filter(|id| !id.is_empty());
    let sync_snapshot_token = match (socket_id, snapshot_caller) {
        (Some(socket_id), Some(browser_fp)) => core.services.feed.bind_session_snapshot(
            socket_id,
            browser_fp,
            projection.session_ids.iter().cloned().collect(),
        ),
        _ => None,
    };
    tracing::debug!(
        caller = caller.fingerprint(),
        listed = projection.session_ids.len(),
        recovery = !projection.recovery_metadata.is_empty(),
        "sessions: listed"
    );
    Response::ok(SessionsListResponse {
        sessions: projection.sessions,
        sync_snapshot_token,
        recovery_metadata: projection.recovery_metadata,
        ..Default::default()
    })
}

/// Ask a session's worker where a reattaching viewer's replay starts.
pub async fn handle_sessions_attach(
    core: &CoordCore,
    caller: &Caller,
    req: SessionsAttachRequest,
) -> ServiceResult<SessionsAttachResponse> {
    let browser_fp = require_account_device(caller)?;
    let _lease = lease(core)?;
    let relay = &core.services.scrollback;
    let session = SessionId::try_from(req.session_id.as_str())
        .map_err(|_| ConnectError::new(ErrorCode::NotFound, "session not found"))?;
    let binding = relay
        .session_worker_socket(&core.services.db, &session)
        .await?;
    let from_offset = req
        .from_offset
        .map(i64::try_from)
        .transpose()
        .map_err(|_| {
            ConnectError::new(ErrorCode::InvalidArgument, "from_offset is out of range")
        })?;
    let mut pending = relay
        .pending()
        .create_fresh(Some(binding.worker_fp.as_str()), relay.now_ms())?;
    send_browser_command(
        &binding.handle,
        browser_fp,
        pending.request_id(),
        ClientControlFrame::Attach {
            session_id: session.clone(),
            from_offset,
            trace_id: None,
        },
    )?;
    tracing::info!(session_id = %session, worker_fp = %binding.worker_fp, "sessions: attach relayed");
    let reply = settle_within(&mut pending, DEFAULT_PENDING_RPC_TIMEOUT_MS).await?;
    let replay_offset = reply
        .get("replay_offset")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            tracing::warn!(session_id = %session, "sessions: the worker's attach result has no replay offset");
            ConnectError::new(ErrorCode::Internal, "worker returned a malformed attach result")
        })?;
    Response::ok(SessionsAttachResponse {
        replay_offset,
        ..Default::default()
    })
}

/// Kill a session's PTY, or tombstone it when forced and its worker is offline.
///
/// A non-forced kill of an offline worker's session stays `accepted: false`,
/// so a transient disconnect never removes a real session. A forced one appends
/// the permanent `closed` tombstone; if that PTY ever returns, the snapshot
/// reconcile reaps it as an orphan.
pub async fn handle_sessions_kill(
    core: &CoordCore,
    caller: &Caller,
    req: SessionsKillRequest,
) -> ServiceResult<SessionsKillResponse> {
    let browser_fp = require_account_device(caller)?;
    let _lease = lease(core)?;
    let worker_fp: Option<String> =
        sqlx::query_scalar("SELECT worker_fp FROM sessions WHERE id = ?1")
            .bind(&req.session_id)
            .fetch_optional(core.services.db.pool())
            .await
            .map_err(|error| read_failed("kill", &error))?;
    let Some(worker_fp) = worker_fp else {
        return kill_answer(false);
    };
    let session = stored_session_id(&req.session_id)?;
    let handle = WorkerFp::try_from(worker_fp.as_str())
        .ok()
        .and_then(|fp| core.services.workers.current_routable(&fp));
    let Some(handle) = handle else {
        if !req.force {
            tracing::info!(session_id = %session, worker_fp, "sessions: kill refused, the worker is offline");
            return kill_answer(false);
        }
        let closed = SessionEvent::Closed {
            session_id: session.clone(),
            exit_code: None,
            ts: now_ms(),
            trace_id: None,
        };
        append_coordinator_event(core, closed, None).await?;
        tracing::info!(session_id = %session, worker_fp, "sessions: force-closed an offline worker's session");
        return kill_answer(true);
    };
    let kill = ClientControlFrame::Kill {
        session_id: session.clone(),
        trace_id: None,
    };
    let request_id = core.services.scrollback.pending().next_request_id();
    match send_browser_command(&handle, browser_fp, &request_id, kill) {
        Ok(()) => {
            tracing::info!(session_id = %session, worker_fp, "sessions: kill relayed");
            kill_answer(true)
        }
        Err(error) => {
            tracing::warn!(session_id = %session, worker_fp, %error, "sessions: kill send failed");
            kill_answer(false)
        }
    }
}

/// Set or clear a session's sticky title. `""` clears it; the rest is trimmed
/// and capped so a runaway paste cannot bloat the row.
pub async fn handle_sessions_rename(
    core: &CoordCore,
    caller: &Caller,
    req: SessionsRenameRequest,
) -> ServiceResult<SessionsRenameResponse> {
    require_account_device(caller)?;
    let _lease = lease(core)?;
    if !session_exists(core, &req.session_id).await? {
        return Response::ok(SessionsRenameResponse {
            ok: false,
            ..Default::default()
        });
    }
    let session = stored_session_id(&req.session_id)?;
    let custom_title = capped_title(&req.title);
    let cleared = custom_title.is_empty();
    let renamed = SessionEvent::Renamed {
        session_id: session.clone(),
        custom_title,
        ts: now_ms(),
        trace_id: None,
    };
    append_coordinator_event(core, renamed, None).await?;
    tracing::info!(session_id = %session, cleared, "sessions: renamed");
    Response::ok(SessionsRenameResponse {
        ok: true,
        ..Default::default()
    })
}

/// The caller's identity scoped to its browser tab, when it named one.
pub(super) fn viewer_key(browser_fp: &str, tab_id: Option<&str>) -> String {
    match tab_id {
        Some(tab_id) if !tab_id.is_empty() => format!("{browser_fp}:{tab_id}"),
        _ => browser_fp.to_owned(),
    }
}

/// A shared write lease, held for the whole mutation, or v2's refusal while a
/// keeper update drains the coordinator.
pub(super) fn lease(core: &CoordCore) -> Result<SharedLease, ConnectError> {
    core.services
        .write_gate()
        .acquire_shared()
        .map_err(|error| ConnectError::new(ErrorCode::Unavailable, error.to_string()))
}

/// Append one coordinator-originated event (a rename, a forced close, a
/// workspace assignment) through the process's one event log.
pub(super) async fn append_coordinator_event(
    core: &CoordCore,
    event: SessionEvent,
    extra_work: Option<AtomicExtraWork<'_>>,
) -> Result<AppendEventResult, ConnectError> {
    let services = &core.services;
    let tenant = services.boot.require_tenant()?;
    let append_caller = crate::events::append::Caller::coordinator(&tenant.dashboard_id);
    let mut options = AppendOptions {
        now_ms: now_ms(),
        buses: &services.buses,
        live_effects: services.event_log.live_effects().as_ref(),
        pending_publications: Some(Arc::clone(services.event_log.pending_publications())),
        can_publish: None,
        extra_work,
        defer_snapshot_reap: false,
    };
    services
        .event_log
        .append_event(event, &append_caller, &mut options)
        .await
        .map_err(|error| {
            tracing::error!(%error, "sessions: a coordinator-side append failed");
            ConnectError::new(ErrorCode::Internal, error.to_string())
        })
}

/// Whether a session row exists under exactly this id.
pub(super) async fn session_exists(
    core: &CoordCore,
    session_id: &str,
) -> Result<bool, ConnectError> {
    let found: Option<String> = sqlx::query_scalar("SELECT id FROM sessions WHERE id = ?1")
        .bind(session_id)
        .fetch_optional(core.services.db.pool())
        .await
        .map_err(|error| read_failed("session lookup", &error))?;
    Ok(found.is_some())
}

/// The branded id of a session a row was just found for. A stored id is always
/// a UUID, so a failure is a corrupt row rather than a caller's mistake.
pub(super) fn stored_session_id(raw: &str) -> Result<SessionId, ConnectError> {
    SessionId::try_from(raw).map_err(|error| {
        tracing::error!(%error, "sessions: a stored session id is malformed");
        ConnectError::new(ErrorCode::Internal, "stored session id is invalid")
    })
}

pub(super) fn read_failed(operation: &'static str, error: &sqlx::Error) -> ConnectError {
    tracing::error!(operation, %error, "sessions: a session read failed");
    ConnectError::new(ErrorCode::Internal, "session read failed")
}

/// Trimmed as JavaScript's `trim()` (which also strips a BOM), then capped at
/// the UTF-16 length v2's `slice(0, 200)` keeps, never splitting a character.
fn capped_title(title: &str) -> String {
    let mut units = 0;
    title
        .trim_matches(|character: char| character.is_whitespace() || character == '\u{feff}')
        .chars()
        .take_while(|character| {
            units += character.len_utf16();
            units <= CUSTOM_TITLE_MAX_UTF16
        })
        .collect()
}

fn kill_answer(accepted: bool) -> ServiceResult<SessionsKillResponse> {
    Response::ok(SessionsKillResponse {
        accepted,
        ..Default::default()
    })
}
