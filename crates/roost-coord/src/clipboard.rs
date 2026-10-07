//! Coordinator-owned universal clipboard history and its four device RPCs.
//!
//! Browser copy paths and OSC 52 worker metadata enter here; rows and live
//! deltas are committed before publication. Clipboard text never enters logs.

use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_proto as proto;

use crate::auth::principal::require_account_device;
use crate::coord_core::{Caller, CoordCore};
use crate::db::CoordDb;
use crate::events::bus_messages::{ClipboardHistoryChange, ClipboardHistoryChangeKind};
use crate::rpc::service::ok_response;

pub const CLIPBOARD_TEXT_MAX_BYTES: usize = 256 * 1024;
pub const CLIPBOARD_HISTORY_LIMIT: i64 = 50;
pub const CLIPBOARD_RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// Remove history rows past the retention window on a scheduled maintenance turn.
pub async fn prune_clipboard_history(database: &CoordDb, now_ms: i64) -> Result<u64, sqlx::Error> {
    let outcome = sqlx::query("DELETE FROM clipboard_entries WHERE created_at_ms < $1")
        .bind(now_ms.saturating_sub(CLIPBOARD_RETENTION_MS))
        .execute(database.pool())
        .await?;
    Ok(outcome.rows_affected())
}
#[derive(Debug, Clone, sqlx::FromRow)]
struct ClipboardRow {
    id: String,
    text: String,
    source_session_id: Option<String>,
    source_worker_fp: Option<String>,
    source_kind: String,
    created_at_ms: i64,
}

/// List the newest retained entries for one paired browser.
pub async fn handle_clipboard_list(
    core: &CoordCore,
    caller: &Caller,
    _request: proto::ClipboardListRequest,
) -> ServiceResult<proto::ClipboardListResponse> {
    require_account_device(caller)?;
    let rows = sqlx::query_as::<_, ClipboardRow>(
        "SELECT id, text, source_session_id, source_worker_fp, source_kind, created_at_ms \
         FROM clipboard_entries ORDER BY created_at_ms DESC, id DESC LIMIT $1",
    )
    .bind(CLIPBOARD_HISTORY_LIMIT)
    .fetch_all(core.services.db.pool())
    .await
    .map_err(internal)?;
    ok_response(proto::ClipboardListResponse {
        entries: rows.iter().map(row_to_proto).collect(),
        ..Default::default()
    })
}

/// Store a browser terminal copy, validating its session and source kind.
pub async fn handle_clipboard_add(
    core: &CoordCore,
    caller: &Caller,
    request: proto::ClipboardAddRequest,
) -> ServiceResult<proto::ClipboardAddResponse> {
    require_account_device(caller)?;
    if request.text.len() > CLIPBOARD_TEXT_MAX_BYTES {
        return Err(refusal(
            ErrorCode::InvalidArgument,
            "clipboard text exceeds 256 KiB",
        ));
    }
    let source_kind = match request.source_kind.as_str() {
        "selection" | "command_output" => request.source_kind.as_str(),
        _ => {
            return Err(refusal(
                ErrorCode::InvalidArgument,
                "invalid clipboard source kind",
            ));
        }
    };
    let dashboard_id = core.services.boot.require_tenant()?.dashboard_id.clone();
    let source_worker_fp: Option<String> =
        sqlx::query_scalar("SELECT worker_fp FROM sessions WHERE id = $1 AND dashboard_id = $2")
            .bind(&request.session_id)
            .bind(dashboard_id)
            .fetch_optional(core.services.db.pool())
            .await
            .map_err(internal)?;
    let Some(worker_fp) = source_worker_fp else {
        return Err(refusal(
            ErrorCode::NotFound,
            "clipboard source session not found",
        ));
    };
    let entry = store_entry(
        &core.services.db,
        &request.text,
        Some(&request.session_id),
        Some(&worker_fp),
        source_kind,
        crate::serve::now_ms(),
    )
    .await
    .map_err(internal)?;
    publish_added(core, &entry);
    ok_response(proto::ClipboardAddResponse {
        entry: proto::buffa::MessageField::some(row_to_proto(&entry)),
        ..Default::default()
    })
}

/// Remove one history row. Missing ids are idempotently accepted.
pub async fn handle_clipboard_delete(
    core: &CoordCore,
    caller: &Caller,
    request: proto::ClipboardDeleteRequest,
) -> ServiceResult<proto::ClipboardDeleteResponse> {
    require_account_device(caller)?;
    let outcome = sqlx::query("DELETE FROM clipboard_entries WHERE id = $1")
        .bind(&request.id)
        .execute(core.services.db.pool())
        .await
        .map_err(internal)?;
    if outcome.rows_affected() != 0 {
        core.services
            .buses
            .clipboard_history_bus
            .publish(ClipboardHistoryChange {
                kind: ClipboardHistoryChangeKind::Removed,
                entry: None,
                id: request.id,
            });
    }
    ok_response(proto::ClipboardDeleteResponse {
        ok: true,
        ..Default::default()
    })
}

/// Clear every history row and publish one live clear event.
pub async fn handle_clipboard_clear(
    core: &CoordCore,
    caller: &Caller,
    _request: proto::ClipboardClearRequest,
) -> ServiceResult<proto::ClipboardClearResponse> {
    require_account_device(caller)?;
    let outcome = sqlx::query("DELETE FROM clipboard_entries")
        .execute(core.services.db.pool())
        .await
        .map_err(internal)?;
    if outcome.rows_affected() != 0 {
        core.services
            .buses
            .clipboard_history_bus
            .publish(ClipboardHistoryChange {
                kind: ClipboardHistoryChangeKind::Cleared,
                entry: None,
                id: String::new(),
            });
    }
    ok_response(proto::ClipboardClearResponse {
        ok: true,
        ..Default::default()
    })
}

/// Persist one OSC 52 write, which the coordinator resolves from the live worker channel.
pub async fn capture_osc52(
    core: &CoordCore,
    session_id: &str,
    worker_fp: &str,
    text: &str,
    created_at_ms: i64,
) -> Result<(), sqlx::Error> {
    if text.len() > CLIPBOARD_TEXT_MAX_BYTES {
        tracing::info!(
            clipboard_bytes = text.len(),
            "OSC 52 clipboard entry exceeded history cap"
        );
        return Ok(());
    }
    let entry = store_entry(
        &core.services.db,
        text,
        Some(session_id),
        Some(worker_fp),
        "osc52",
        created_at_ms,
    )
    .await?;
    publish_added(core, &entry);
    tracing::debug!(
        clipboard_bytes = text.len(),
        "OSC 52 clipboard history entry stored"
    );
    Ok(())
}

async fn store_entry(
    database: &CoordDb,
    text: &str,
    session_id: Option<&str>,
    worker_fp: Option<&str>,
    source_kind: &str,
    now_ms: i64,
) -> Result<ClipboardRow, sqlx::Error> {
    let mut transaction = database.pool().begin().await?;
    sqlx::query("DELETE FROM clipboard_entries WHERE created_at_ms < $1")
        .bind(now_ms.saturating_sub(CLIPBOARD_RETENTION_MS))
        .execute(&mut *transaction)
        .await?;
    let newest = sqlx::query_as::<_, ClipboardRow>(
        "SELECT id, text, source_session_id, source_worker_fp, source_kind, created_at_ms \
         FROM clipboard_entries ORDER BY created_at_ms DESC, id DESC LIMIT 1",
    )
    .fetch_optional(&mut *transaction)
    .await?;
    let row = if let Some(mut newest) = newest.filter(|entry| entry.text == text) {
        sqlx::query(
            "UPDATE clipboard_entries SET source_session_id = $1, source_worker_fp = $2, \
             source_kind = $3, created_at_ms = $4 WHERE id = $5",
        )
        .bind(session_id)
        .bind(worker_fp)
        .bind(source_kind)
        .bind(now_ms)
        .bind(&newest.id)
        .execute(&mut *transaction)
        .await?;
        newest.source_session_id = session_id.map(str::to_owned);
        newest.source_worker_fp = worker_fp.map(str::to_owned);
        newest.source_kind = source_kind.to_owned();
        newest.created_at_ms = now_ms;
        newest
    } else {
        let id = new_id()?;
        sqlx::query(
            "INSERT INTO clipboard_entries (id, text, source_session_id, source_worker_fp, \
             source_kind, created_at_ms) VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(&id)
        .bind(text)
        .bind(session_id)
        .bind(worker_fp)
        .bind(source_kind)
        .bind(now_ms)
        .execute(&mut *transaction)
        .await?;
        ClipboardRow {
            id,
            text: text.to_owned(),
            source_session_id: session_id.map(str::to_owned),
            source_worker_fp: worker_fp.map(str::to_owned),
            source_kind: source_kind.to_owned(),
            created_at_ms: now_ms,
        }
    };
    sqlx::query(
        "DELETE FROM clipboard_entries WHERE id IN (SELECT id FROM clipboard_entries \
         ORDER BY created_at_ms DESC, id DESC LIMIT 9223372036854775807 OFFSET $1)",
    )
    .bind(CLIPBOARD_HISTORY_LIMIT)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(row)
}

fn new_id() -> Result<String, sqlx::Error> {
    crate::coord_core::ids::draw::<16>()
        .map(crate::coord_core::ids::render_v4)
        .map_err(sqlx::Error::Io)
}

fn row_to_proto(row: &ClipboardRow) -> proto::ClipboardEntry {
    proto::ClipboardEntry {
        id: row.id.clone(),
        text: row.text.clone(),
        source_session_id: row.source_session_id.clone().unwrap_or_default(),
        source_worker_fp: row.source_worker_fp.clone().unwrap_or_default(),
        source_kind: row.source_kind.clone(),
        created_at_ms: row.created_at_ms,
        ..Default::default()
    }
}

fn publish_added(core: &CoordCore, row: &ClipboardRow) {
    core.services
        .buses
        .clipboard_history_bus
        .publish(ClipboardHistoryChange {
            kind: ClipboardHistoryChangeKind::Added,
            entry: Some(row_to_sync_entry(row)),
            id: row.id.clone(),
        });
}

fn row_to_sync_entry(row: &ClipboardRow) -> roost_proto::TerminalClipboardEntry {
    roost_proto::TerminalClipboardEntry {
        id: row.id.clone(),
        text: row.text.clone(),
        source_session_id: row.source_session_id.clone().unwrap_or_default(),
        source_worker_fp: row.source_worker_fp.clone().unwrap_or_default(),
        source_kind: row.source_kind.clone(),
        created_at_ms: row.created_at_ms,
        ..Default::default()
    }
}

fn refusal(code: ErrorCode, reason: impl Into<String>) -> ConnectError {
    ConnectError::new(code, reason.into())
}

fn internal(error: sqlx::Error) -> ConnectError {
    refusal(ErrorCode::Internal, error.to_string())
}
