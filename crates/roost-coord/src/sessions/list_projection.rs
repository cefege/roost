//! `SessionsList`'s database selection and its public row shapes. Called by
//! `sessions::rpc_sessions::handle_sessions_list`; reads rows through
//! `events::projection`'s column list and row parser so every session reader
//! agrees on what a row is.

use std::str::FromStr;

use connectrpc::{ConnectError, ErrorCode};
use roost_proto::Session as PbSession;
use roost_protocol::wire::{SessionStatus, session_to_proto};
use sqlx::AssertSqlSafe;

use crate::db::CoordDb;
use crate::events::projection::{SESSION_COLUMNS, StoredSessionRow, session_from_row};

/// The rows a list may read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionListScope<'a> {
    /// A browser's list: public columns, optionally narrowed to one worker.
    Public {
        /// The worker to narrow to, when the request named one.
        worker_fp: Option<&'a str>,
    },
}

/// One list answer.
#[derive(Debug, Default)]
pub struct SessionsListProjection {
    /// Every listed session's id, in row order.
    pub session_ids: Vec<String>,
    /// The public rows.
    pub sessions: Vec<PbSession>,
}

/// Parse a request's status: absent or empty is `open`, `all` is no filter.
pub fn session_status_filter(raw: Option<&str>) -> Result<Option<SessionStatus>, ConnectError> {
    let status = raw.filter(|value| !value.is_empty()).unwrap_or("open");
    if status == "all" {
        return Ok(None);
    }
    SessionStatus::from_str(status).map(Some).map_err(|_| {
        ConnectError::new(
            ErrorCode::InvalidArgument,
            format!("invalid session status {status:?}"),
        )
    })
}

/// Read the rows a scope may see.
pub async fn read_sessions_list_projection(
    db: &CoordDb,
    scope: SessionListScope<'_>,
    status: Option<SessionStatus>,
) -> Result<SessionsListProjection, ConnectError> {
    let status = status.map(SessionStatus::as_str);
    let columns = SESSION_COLUMNS.join(", ");
    let SessionListScope::Public { worker_fp } = scope;
    let filter = "WHERE ($1 IS NULL OR worker_fp = $1) AND ($2 IS NULL OR status = $2)";
    let rows: Vec<StoredSessionRow> = sqlx::query_as(AssertSqlSafe(format!(
        "SELECT {columns} FROM sessions {filter}"
    )))
    .bind(worker_fp)
    .bind(status)
    .fetch_all(db.pool())
    .await
    .map_err(read_failed)?;
    Ok(SessionsListProjection {
        session_ids: rows.iter().map(|row| row.id.clone()).collect(),
        sessions: rows
            .iter()
            .map(session_row_to_proto)
            .collect::<Result<_, _>>()?,
    })
}

fn session_row_to_proto(row: &StoredSessionRow) -> Result<PbSession, ConnectError> {
    session_from_row(row)
        .map_err(|error| error.to_string())
        .and_then(|session| session_to_proto(&session).map_err(|error| error.to_string()))
        .map_err(|reason| {
            tracing::error!(
                session_id = row.id,
                reason,
                "sessions: a stored session row is unreadable"
            );
            ConnectError::new(ErrorCode::Internal, "stored session row is invalid")
        })
}

fn read_failed(error: sqlx::Error) -> ConnectError {
    tracing::error!(%error, "sessions: the session list read failed");
    ConnectError::new(ErrorCode::Internal, "session list read failed")
}
