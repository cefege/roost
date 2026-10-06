//! `SessionsList`'s database selection and its public and private row shapes.
//! Ports `apps/coord/src/sessions/session-list-projection.ts`.
//!
//! Called by `sessions::rpc_sessions::handle_sessions_list`; reads the rows
//! through `events::projection`'s column list and row parser so a list and every
//! other session reader agree on what a row is.
//!
//! THE PRIVATE COLUMNS ARE A SCOPE, NOT A FLAG. The agent-conversation reference
//! is worker-recovery state and must never reach a browser, so the only way to
//! select it is [`SessionListScope::OwnWorkerRecovery`], which cannot be built
//! without the owning worker (v2 threw at runtime for the same combination).
//! A stored reference that fails to parse is refused without echoing its value.

use std::str::FromStr;

use connectrpc::{ConnectError, ErrorCode};
use roost_proto::{Session as PbSession, SessionRecoveryMetadata as PbSessionRecoveryMetadata};
use roost_protocol::agent_conversation_reference::{
    AgentConversationRecoveryMetadata, AgentConversationReferenceV1,
};
use roost_protocol::proto_adapters::agent_conversation_reference_proto::session_recovery_metadata_to_proto;
use roost_protocol::wire::{SessionId, SessionStatus, session_to_proto};
use sqlx::{AssertSqlSafe, FromRow};

use crate::db::CoordDb;
use crate::events::projection::{SESSION_COLUMNS, StoredSessionRow, session_from_row};

/// Which rows a list may read, and whether their private columns come with them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionListScope<'a> {
    /// A browser's list: public columns, optionally narrowed to one worker.
    Public {
        /// The worker to narrow to, when the request named one.
        worker_fp: Option<&'a str>,
    },
    /// The authenticated owning worker's boot recovery: its rows, with the
    /// private agent-conversation recovery columns.
    OwnWorkerRecovery {
        /// The worker the caller authenticated as.
        worker_fp: &'a str,
    },
}

/// One list answer: the ids, their public rows, and the private metadata that
/// only the owning worker's scope fills.
#[derive(Debug, Default)]
pub struct SessionsListProjection {
    /// Every listed session's id, in row order.
    pub session_ids: Vec<String>,
    /// The public rows.
    pub sessions: Vec<PbSession>,
    /// The recovery rows; empty outside [`SessionListScope::OwnWorkerRecovery`].
    pub recovery_metadata: Vec<PbSessionRecoveryMetadata>,
}

/// The two private columns, beside the public row they belong to.
#[derive(Debug, FromRow)]
struct SessionRecoveryRow {
    #[sqlx(flatten)]
    session: StoredSessionRow,
    agent_reference_json: Option<String>,
    agent_reference_client_seq: Option<i64>,
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
    let filter = "WHERE ($1 IS NULL OR worker_fp = $1) AND ($2 IS NULL OR status = $2)";
    match scope {
        SessionListScope::Public { worker_fp } => {
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
                recovery_metadata: Vec::new(),
            })
        }
        SessionListScope::OwnWorkerRecovery { worker_fp } => {
            let rows: Vec<SessionRecoveryRow> = sqlx::query_as(AssertSqlSafe(format!(
                "SELECT {columns}, agent_reference_json, agent_reference_client_seq \
                 FROM sessions {filter}"
            )))
            .bind(worker_fp)
            .bind(status)
            .fetch_all(db.pool())
            .await
            .map_err(read_failed)?;
            Ok(SessionsListProjection {
                session_ids: rows.iter().map(|row| row.session.id.clone()).collect(),
                sessions: rows
                    .iter()
                    .map(|row| session_row_to_proto(&row.session))
                    .collect::<Result<_, _>>()?,
                recovery_metadata: rows
                    .iter()
                    .map(session_recovery_row_to_proto)
                    .collect::<Result<_, _>>()?,
            })
        }
    }
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

/// The private recovery row. Neither the stored JSON nor the parser's reason
/// is echoed: the value is a filesystem path the operator's log must not carry.
fn session_recovery_row_to_proto(
    row: &SessionRecoveryRow,
) -> Result<PbSessionRecoveryMetadata, ConnectError> {
    let parsed = row
        .agent_reference_json
        .as_deref()
        .map(|text| {
            serde_json::from_str(text)
                .ok()
                .and_then(|value| AgentConversationReferenceV1::parse(value).ok())
                .ok_or(())
        })
        .transpose()
        .and_then(|agent_reference| {
            let metadata = AgentConversationRecoveryMetadata {
                session_id: SessionId::try_from(row.session.id.as_str()).map_err(|_| ())?,
                agent_reference,
                agent_reference_client_seq: row.agent_reference_client_seq.unwrap_or(0),
            };
            session_recovery_metadata_to_proto(&metadata).map_err(|_| ())
        });
    parsed.map_err(|()| {
        tracing::error!(
            session_id = row.session.id,
            "sessions: stored agent conversation recovery metadata is invalid"
        );
        ConnectError::new(
            ErrorCode::Internal,
            "stored agent conversation recovery metadata is invalid",
        )
    })
}

fn read_failed(error: sqlx::Error) -> ConnectError {
    tracing::error!(%error, "sessions: the session list read failed");
    ConnectError::new(ErrorCode::Internal, "session list read failed")
}
