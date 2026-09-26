//! The session row shape: the eighteen projection columns, how a folded session
//! becomes one, and how one is read back.
//!
//! Ported from the row half of `apps/coord/src/events/event-projection.ts`. The
//! statements live in `projection_writes`; this file is the shape they all share,
//! so a column cannot be added to one and forgotten by the other.
//!
//! THE AGENT COLUMNS ARE NOT IN THE ROW SHAPE, ON PURPOSE. v2's `sessionToRow`
//! return type omits `agent_json`, `agent_reference_json` and
//! `agent_reference_client_seq` (`event-projection.ts:29-33`), so no ordinary
//! session write can overwrite them -- the same predicate that keeps
//! `agent_reference` off every browser lane keeps it out of the projection's
//! write set. `agent_conversation_recovery` owns those three columns and is
//! their only writer.
//!
//! STORED ENUMS ARE PARSED, NOT CAST. v2 cast `kind`, `status`, `pr_state` and
//! `pr_checks` with `as` and never checked them
//! (`event-projection.ts:126-134`), so a hand-edited row produced a session
//! carrying a string where an enum belonged, and the failure surfaced later, in
//! a frame builder, as something unrelated. Here an unreadable value names its own
//! column, which is the only difference the port needed.
//!
//! A STORED ROW HAS BEEN LOOKED AT, SO `git_remote` HAS TWO STATES. The column
//! cannot distinguish "never resolved" from "resolved, no origin", and v2 read
//! both as an explicit null (`event-projection.ts:131`). The absent state belongs
//! to a wire event, not to a row.

use std::str::FromStr;

use roost_protocol::json::safe_json_parse;
use roost_protocol::wire::{
    ChannelId, PullRequestChecks, PullRequestState, Session, SessionId, SessionKind, SessionStatus,
    WorkerFp, WorkspaceId,
};
use serde_json::json;
use sqlx::{Executor, FromRow};

/// The exact session projection column list, shared with every reader that wants
/// the same row-to-proto adapters (`event-projection.ts:13-17`).
///
/// `agent_json`, `agent_reference_json` and `agent_reference_client_seq` are
/// absent by design; see the module header.
pub const SESSION_COLUMNS: [&str; 18] = [
    "id",
    "worker_fp",
    "channel",
    "kind",
    "cwd",
    "workspace_id",
    "status",
    "created_at",
    "closed_at",
    "custom_title",
    "git_branch",
    "git_remote",
    "pr_number",
    "pr_state",
    "pr_checks",
    "pr_url",
    "ports_json",
    "spawn_cwd",
];

/// A session row as it is written: the eighteen projection columns plus the
/// dashboard scope, and nothing else.
///
/// The text borrows rather than owns, so writing a row copies no session text.
/// `ports_json` owns its `String` because it is serialized here.
#[derive(Debug, Clone)]
pub struct SessionRow<'a> {
    /// The session's id.
    pub id: &'a str,
    /// The dashboard this row is scoped to. Written; a worker snapshot never
    /// updates it.
    pub dashboard_id: &'a str,
    /// The worker that owns the session.
    pub worker_fp: &'a str,
    /// The worker-local keeper channel.
    pub channel: i64,
    /// The session kind.
    pub kind: &'a str,
    /// The live folder.
    pub cwd: &'a str,
    /// The owning workspace, absent for an orphan.
    pub workspace_id: Option<&'a str>,
    /// `open` or `closed`.
    pub status: &'a str,
    /// When the session opened.
    pub created_at: i64,
    /// When it closed.
    pub closed_at: Option<i64>,
    /// The user's sticky rename.
    pub custom_title: Option<&'a str>,
    /// The resolved branch.
    pub git_branch: Option<&'a str>,
    /// The resolved origin.
    pub git_remote: Option<&'a str>,
    /// The pull-request number.
    pub pr_number: Option<i64>,
    /// The pull-request state.
    pub pr_state: Option<&'a str>,
    /// The pull-request checks.
    pub pr_checks: Option<&'a str>,
    /// The pull-request url.
    pub pr_url: Option<&'a str>,
    /// The listening ports, serialized. Absent for "none", never `[]`.
    pub ports_json: Option<String>,
    /// The folder the session was created in.
    pub spawn_cwd: Option<&'a str>,
}

/// A stored session row as the database spells it: every closed enum is text,
/// because a column is text and no `CHECK` constraint decides what this build
/// understands.
#[derive(Debug, FromRow)]
pub struct StoredSessionRow {
    /// The session's id.
    pub id: String,
    /// The owning worker.
    pub worker_fp: String,
    /// The worker-local keeper channel.
    pub channel: i64,
    /// The session kind, as text.
    pub kind: String,
    /// The live folder.
    pub cwd: String,
    /// The owning workspace.
    pub workspace_id: Option<String>,
    /// The status, as text.
    pub status: String,
    /// When the session opened.
    pub created_at: i64,
    /// When it closed.
    pub closed_at: Option<i64>,
    /// The user's sticky rename.
    pub custom_title: Option<String>,
    /// The resolved branch.
    pub git_branch: Option<String>,
    /// The resolved origin.
    pub git_remote: Option<String>,
    /// The pull-request number.
    pub pr_number: Option<i64>,
    /// The pull-request state, as text.
    pub pr_state: Option<String>,
    /// The pull-request checks, as text.
    pub pr_checks: Option<String>,
    /// The pull-request url.
    pub pr_url: Option<String>,
    /// The listening ports, as JSON text.
    pub ports_json: Option<String>,
    /// The folder the session was created in.
    pub spawn_cwd: Option<String>,
}

/// Why a projection read failed.
#[derive(Debug, thiserror::Error)]
pub enum ProjectionError {
    /// The statement failed.
    #[error("sqlite: {0}")]
    Sqlite(#[from] sqlx::Error),
    /// A stored row holds a value the domain types cannot represent: an
    /// unparsable enum, or an id that is not the shape its brand requires.
    #[error("stored session {field}: {reason}")]
    StoredRow {
        /// The column that could not be read.
        field: &'static str,
        /// What is wrong with it.
        reason: String,
    },
}

/// Convert a folded session into the row that is written.
///
/// `ports` is `NULL` for both "never reported" and "reported empty"
/// (`event-projection.ts:45`), which is why a session whose fold produced
/// `Some([])` reads back as `Some([])`: the column cannot tell them apart, and v2
/// never needed it to.
#[must_use]
pub fn session_to_row<'a>(session: &'a Session, dashboard_id: &'a str) -> SessionRow<'a> {
    SessionRow {
        id: session.id.as_str(),
        dashboard_id,
        worker_fp: session.worker_fp.as_str(),
        channel: i64::from(session.channel.as_u32()),
        kind: session.kind.as_str(),
        cwd: &session.cwd,
        workspace_id: session.workspace_id.as_ref().map(WorkspaceId::as_str),
        status: session.status.as_str(),
        created_at: session.created_at,
        closed_at: session.closed_at,
        custom_title: session.custom_title.as_deref(),
        git_branch: session.git_branch.as_deref(),
        git_remote: session
            .git_remote
            .as_ref()
            .and_then(|remote| remote.as_deref()),
        pr_number: session.pr_number,
        pr_state: session.pr_state.map(PullRequestState::as_str),
        pr_checks: session.pr_checks.map(PullRequestChecks::as_str),
        pr_url: session.pr_url.as_deref(),
        ports_json: session
            .ports
            .as_ref()
            .filter(|ports| !ports.is_empty())
            .and_then(|ports| serde_json::to_string(ports).ok()),
        spawn_cwd: session.spawn_cwd.as_deref(),
    }
}

/// Load one session row, or `None` when there is no such session.
pub async fn load_session<'executor, E>(
    executor: E,
    id: &str,
) -> Result<Option<Session>, ProjectionError>
where
    E: Executor<'executor, Database = sqlx::Sqlite>,
{
    let row = sqlx::query_as::<_, StoredSessionRow>(
        "SELECT id, worker_fp, channel, kind, cwd, workspace_id, status, created_at, closed_at, \
                custom_title, git_branch, git_remote, pr_number, pr_state, pr_checks, pr_url, \
                ports_json, spawn_cwd \
           FROM sessions WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(executor)
    .await?;
    row.as_ref().map(session_from_row).transpose()
}

/// Rebuild the domain row from a stored one.
pub fn session_from_row(row: &StoredSessionRow) -> Result<Session, ProjectionError> {
    Ok(Session {
        id: brand("sessions.id", &row.id, |value| SessionId::try_from(value))?,
        worker_fp: brand("sessions.worker_fp", &row.worker_fp, |value| {
            WorkerFp::try_from(value)
        })?,
        channel: ChannelId::try_from(row.channel).map_err(|error| ProjectionError::StoredRow {
            field: "sessions.channel",
            reason: error.reason,
        })?,
        kind: closed_enum("sessions.kind", &row.kind, |value| {
            SessionKind::from_str(value)
        })?,
        cwd: row.cwd.clone(),
        spawn_cwd: row.spawn_cwd.clone(),
        workspace_id: optional_brand(
            "sessions.workspace_id",
            row.workspace_id.as_deref(),
            |value| WorkspaceId::try_from(value),
        )?,
        status: closed_enum("sessions.status", &row.status, |value| {
            SessionStatus::from_str(value)
        })?,
        created_at: row.created_at,
        closed_at: row.closed_at,
        custom_title: row.custom_title.clone(),
        git_branch: row.git_branch.clone(),
        git_remote: Some(row.git_remote.clone()),
        pr_number: row.pr_number,
        pr_state: optional_closed_enum(
            "sessions.pr_state",
            row.pr_state.as_deref(),
            PullRequestState::from_str,
        )?,
        pr_checks: optional_closed_enum(
            "sessions.pr_checks",
            row.pr_checks.as_deref(),
            PullRequestChecks::from_str,
        )?,
        pr_url: row.pr_url.clone(),
        ports: Some(ports_from_column(row.ports_json.as_deref())),
    })
}

/// The listening ports, or none.
///
/// The fallback is `[]` and not an error for the reason the source gives (its L11
/// note, `event-projection.ts:135-136`): a hand-edited or partially-written
/// `ports_json` must not throw on the bus publish path, where a throw is an RPC
/// 500 and a split-brain sidebar. An array containing one non-number falls back
/// whole, which is what v2's `number[]` parse did.
fn ports_from_column(ports_json: Option<&str>) -> Vec<i64> {
    safe_json_parse(ports_json, json!([]))
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .map(serde_json::Value::as_i64)
                .collect::<Option<Vec<i64>>>()
        })
        .unwrap_or_default()
}

fn brand<T>(
    field: &'static str,
    value: &str,
    build: impl Fn(&str) -> Result<T, roost_protocol::ProtocolError>,
) -> Result<T, ProjectionError> {
    build(value).map_err(|error| ProjectionError::StoredRow {
        field,
        reason: error.reason,
    })
}

fn optional_brand<T>(
    field: &'static str,
    value: Option<&str>,
    build: impl Fn(&str) -> Result<T, roost_protocol::ProtocolError>,
) -> Result<Option<T>, ProjectionError> {
    value
        .map(build)
        .transpose()
        .map_err(|error| ProjectionError::StoredRow {
            field,
            reason: error.reason,
        })
}

fn closed_enum<T>(
    field: &'static str,
    value: &str,
    build: impl Fn(&str) -> Result<T, roost_protocol::ProtocolError>,
) -> Result<T, ProjectionError> {
    build(value).map_err(|error| ProjectionError::StoredRow {
        field,
        reason: error.reason,
    })
}

fn optional_closed_enum<T>(
    field: &'static str,
    value: Option<&str>,
    build: impl Fn(&str) -> Result<T, roost_protocol::ProtocolError>,
) -> Result<Option<T>, ProjectionError> {
    value
        .map(build)
        .transpose()
        .map_err(|error| ProjectionError::StoredRow {
            field,
            reason: error.reason,
        })
}
