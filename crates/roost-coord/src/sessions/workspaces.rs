//! The workspace tree: the row and its one projection, the order a client sees,
//! the membership a write moves, and the cascade a delete performs.
//!
//! Owned by the workspaces slice. `rpc_workspaces.rs` is the only caller, and it
//! owns the write whose ORDER is the request's own semantics.

use std::collections::{BTreeMap, BTreeSet};

use roost_platform::same_worker_folder;
use roost_protocol::wire::{SessionId, WorkerFp, Workspace, WorkspaceId};
use roost_protocol::ProtocolError;
use sqlx::{FromRow, SqliteConnection};

use crate::db::CoordDb;

/// The `workspaces` columns one projection reads, named once so the ordered
/// list, a write's read-back and a delta cannot read different sets.
pub(crate) const COLUMNS: &str =
    "id, worker_fp, name, folder_path, color, position, version, created_at_ms, updated_at_ms";

/// Why a workspace read or write could not be completed.
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    /// The statement failed, or the transaction could not commit.
    #[error("sqlite: {0}")]
    Sqlite(#[from] sqlx::Error),
    /// A bound id list could not be encoded.
    #[error("id list: {0}")]
    IdList(#[from] serde_json::Error),
    /// A stored value is not legal; the message names the field.
    #[error("workspace value: {0}")]
    Value(#[from] ProtocolError),
    /// The caller's `if_version` named no row, or a stale one.
    #[error("version mismatch")]
    VersionMismatch,
    /// The named worker is not enrolled, or is tombstoned.
    #[error("worker not found")]
    WorkerNotFound,
    /// A requested session does not exist.
    #[error("session not found")]
    SessionNotFound,
}

/// The `workspaces` row as the database spells it.
#[derive(Debug, Clone, FromRow)]
pub(crate) struct Row {
    pub(crate) id: String,
    pub(crate) worker_fp: String,
    pub(crate) name: String,
    pub(crate) folder_path: String,
    pub(crate) color: Option<String>,
    pub(crate) position: i64,
    pub(crate) version: i64,
    pub(crate) created_at_ms: i64,
    pub(crate) updated_at_ms: i64,
}

/// Every workspace, in the order a client sees them.
///
/// THE ORDER AND THE VALUES COME FROM ONE QUERY AND ONE PROJECTION, because a
/// client that re-fetches the list and one that applies a broadcast delta must
/// not disagree. `position` alone is not total, so the id breaks the tie.
pub async fn list_workspaces(database: &CoordDb) -> Result<Vec<Workspace>, WorkspaceError> {
    let mut connection = database.pool().acquire().await?;
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM workspaces ORDER BY position, id"
    ))
    .fetch_all(&mut *connection)
    .await?;
    let ids: Vec<String> = rows.iter().map(|row| row.id.clone()).collect();
    let membership = junction(&mut connection, &ids).await?;
    rows.into_iter()
        .map(|row| project(&row, membership.get(&row.id).map_or(Vec::new, |(_, m)| m.clone())))
        .collect()
}

/// Add a workspace, or answer with the one already at that folder.
///
/// The dedupe scan runs INSIDE the insert transaction, and that is load-bearing:
/// `same_worker_folder` folds `/tmp` onto `/private/tmp`, and two concurrent
/// creates for one folder must serialise behind SQLite's write lock.
pub(crate) async fn create_workspace(
    database: &CoordDb,
    request: &roost_proto::WorkspacesCreateRequest,
    dashboard_id: &str,
    now_ms: i64,
) -> Result<CreatedWorkspace, WorkspaceError> {
    let mut transaction = database.pool().begin().await?;
    let Some(os) = sqlx::query_scalar::<_, String>(
        "SELECT os FROM workers WHERE fp = ? AND deleted_at_ms IS NULL",
    )
    .bind(&request.worker_fp)
    .fetch_optional(&mut *transaction)
    .await?
    else {
        return Err(WorkspaceError::WorkerNotFound);
    };
    let members = unique(&request.attach_session_ids);
    let attached = session_cwds(&mut transaction, &members).await?;
    if attached.len() != members.len() {
        return Err(WorkspaceError::SessionNotFound);
    }
    // A session's realpath is the folder its pane actually opened, so it wins
    // over the path the request named.
    let folder_path = members
        .first()
        .and_then(|session_id| attached.get(session_id))
        .filter(|cwd| !cwd.is_empty())
        .cloned()
        .unwrap_or_else(|| request.folder_path.clone());
    let existing = sqlx::query_as::<_, Row>(&format!(
        "SELECT {COLUMNS} FROM workspaces WHERE worker_fp = ?"
    ))
    .bind(&request.worker_fp)
    .fetch_all(&mut *transaction)
    .await?
    .into_iter()
    .find(|row| same_worker_folder(&os, &row.folder_path, &folder_path));
    if let Some(existing) = existing {
        let session_ids = members_of(&mut transaction, &existing.id).await?;
        let workspace = project(&existing, session_ids)?;
        transaction.commit().await?;
        return Ok(CreatedWorkspace {
            workspace,
            created: false,
        });
    }
    let position: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspaces")
        .fetch_one(&mut *transaction)
        .await?;
    // v2 called `randomUUID()`; no entropy crate is in this crate's dependency
    // graph, and SQLite's `randomblob` is the one CSPRNG already reachable. The
    // version and variant nibbles are pinned so the result is a v4 UUID.
    let id: String = sqlx::query_scalar(
        "SELECT lower(substr(h,1,8) || '-' || substr(h,9,4) || '-4' || substr(h,14,3) || '-8' \
         || substr(h,18,3) || '-' || substr(h,21,12)) \
         FROM (SELECT lower(hex(randomblob(16))) AS h)",
    )
    .fetch_one(&mut *transaction)
    .await?;
    let row = sqlx::query_as::<_, Row>(&format!(
        "INSERT INTO workspaces (id, dashboard_id, worker_fp, name, folder_path, color, position, \
                version, created_at_ms, updated_at_ms) VALUES (?, ?, ?, ?, ?, ?, ?, 0, ?, ?) \
         RETURNING {COLUMNS}"
    ))
    .bind(&id)
    .bind(dashboard_id)
    .bind(&request.worker_fp)
    .bind(&request.name)
    .bind(&folder_path)
    .bind(&request.color)
    .bind(position)
    .bind(now_ms)
    .bind(now_ms)
    .fetch_one(&mut *transaction)
    .await?;
    set_membership(&mut transaction, &row.id, dashboard_id, &members, now_ms).await?;
    let workspace = project(&row, members_of(&mut transaction, &row.id).await?)?;
    transaction.commit().await?;
    Ok(CreatedWorkspace {
        workspace,
        created: true,
    })
}

/// Rewrite the named fields and bump the version, conditioned on `if_version`.
///
/// The version is bumped even when the request named no field: the call claimed
/// a write, and a client that re-reads must see that its claim was spent.
pub async fn update_workspace(
    database: &CoordDb,
    request: &roost_proto::WorkspacesUpdateRequest,
    now_ms: i64,
) -> Result<Workspace, WorkspaceError> {
    let mut transaction = database.pool().begin().await?;
    let mut update = sqlx::QueryBuilder::<sqlx::Sqlite>::new("UPDATE workspaces SET ");
    {
        let mut separated = update.separated(", ");
        for (column, value) in [
            ("name", request.name.as_deref()),
            ("folder_path", request.folder_path.as_deref()),
            ("color", request.color.as_deref()),
        ] {
            if let Some(value) = value {
                separated.push(column).push(" = ").push_bind(value);
            }
        }
        if let Some(position) = request.position {
            separated
                .push("position = ")
                .push_bind(i64::from(position));
        }
        separated.push("updated_at_ms = ").push_bind(now_ms);
        separated.push("version = version + 1");
        separated
            .push("WHERE id = ")
            .push_bind(&request.id)
            .push("AND version = ")
            .push_bind(i64::try_from(request.if_version).unwrap_or(i64::MAX));
    }
    update
        .push_unseparated(" RETURNING ")
        .push_unseparated(COLUMNS);
    let row = update
        .build()
        .fetch_optional::<Row>(&mut *transaction)
        .await?
        .ok_or(WorkspaceError::VersionMismatch)?;
    let session_ids = members_of(&mut transaction, &request.id).await?;
    let workspace = project(&row, session_ids)?;
    transaction.commit().await?;
    Ok(workspace)
}

/// Delete a workspace and everything it holds, conditioned on `if_version`.
pub async fn delete_workspace(
    database: &CoordDb,
    workspace_id: &str,
    if_version: u64,
) -> Result<WorkspaceId, WorkspaceError> {
    let mut transaction = database.pool().begin().await?;
    detach_members(&mut transaction, workspace_id).await?;
    let deleted = sqlx::query("DELETE FROM workspaces WHERE id = ? AND version = ?")
        .bind(workspace_id)
        .bind(i64::try_from(if_version).unwrap_or(i64::MAX))
        .execute(&mut *transaction)
        .await?;
    if deleted.rows_affected() == 0 {
        return Err(WorkspaceError::VersionMismatch);
    }
    transaction.commit().await?;
    Ok(WorkspaceId::try_from(workspace_id)?)
}

/// The membership a delete takes with it: READ IT, then unclaim it, and only then
/// may the caller delete the row.
///
/// THE JUNCTION READ COMES FIRST, because `workspace_sessions` cascades away with
/// the workspace row: read after the delete and there is nothing to read, and the
/// sessions the workspace held keep a `sessions.workspace_id` naming a row that no
/// longer exists -- the half-deleted tree. The same rule in the other direction is
/// why `events::projection_writes::cascade_closed_session` captures the owning
/// workspaces before it deletes a session. The whole thing is one transaction, so
/// a version claim that loses rolls the detach back with it.
pub(crate) async fn detach_members(
    connection: &mut SqliteConnection,
    workspace_id: &str,
) -> Result<(), WorkspaceError> {
    let members = members_of(connection, workspace_id).await?;
    unclaim(connection, workspace_id, &members).await
}

/// Clear the column for exactly these sessions, guarded on BOTH the id it names
/// and the workspace going away, so a session the column already attributes to a
/// different workspace is not stolen out of it. The membership is an argument
/// rather than a read, because one caller must read it before a statement that
/// cascades it away.
pub(crate) async fn unclaim(
    connection: &mut SqliteConnection,
    workspace_id: &str,
    members: &[SessionId],
) -> Result<(), WorkspaceError> {
    let ids: Vec<String> = members.iter().map(|id| id.as_str().to_owned()).collect();
    if ids.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "UPDATE sessions SET workspace_id = NULL WHERE workspace_id = ? \
         AND id IN (SELECT value FROM json_each(?))",
    )
    .bind(workspace_id)
    .bind(id_list(&ids)?)
    .execute(&mut *connection)
    .await?;
    Ok(())
}

/// Point `session_ids` at `workspace_id` on BOTH representations. A session
/// belongs to one workspace, so this MOVES it: every prior junction row goes
/// first, then the new ones are written, and the column follows -- the two are one
/// fact the browser reads twice, so writing only the junction double-counts.
pub(crate) async fn set_membership(
    connection: &mut SqliteConnection,
    workspace_id: &str,
    dashboard_id: &str,
    session_ids: &[String],
    now_ms: i64,
) -> Result<(), WorkspaceError> {
    if session_ids.is_empty() {
        return Ok(());
    }
    let ids = id_list(session_ids)?;
    sqlx::query("DELETE FROM workspace_sessions WHERE session_id IN (SELECT value FROM json_each(?))")
    .bind(&ids)
    .execute(&mut *connection)
    .await?;
    sqlx::query(
        "INSERT INTO workspace_sessions (workspace_id, session_id, added_at_ms, dashboard_id) \
         SELECT ?, value, ?, ? FROM json_each(?)",
    )
    .bind(workspace_id)
    .bind(now_ms)
    .bind(dashboard_id)
    .bind(&ids)
    .execute(&mut *connection)
    .await?;
    sqlx::query(
        "UPDATE sessions SET workspace_id = ? WHERE id IN (SELECT value FROM json_each(?))",
    )
    .bind(workspace_id)
    .bind(&ids)
    .execute(&mut *connection)
    .await?;
    Ok(())
}

/// One statement per id list instead of one per id, never interpolated.
pub(crate) fn id_list(ids: &[String]) -> Result<String, WorkspaceError> {
    Ok(serde_json::to_string(ids)?)
}

/// The sessions that exist, and the folder each one opened.
pub(crate) async fn session_cwds(
    connection: &mut SqliteConnection,
    session_ids: &[String],
) -> Result<BTreeMap<String, String>, WorkspaceError> {
    Ok(sqlx::query_as::<_, (String, String)>(
        "SELECT id, cwd FROM sessions WHERE id IN (SELECT value FROM json_each(?))",
    )
    .bind(id_list(session_ids)?)
    .fetch_all(&mut *connection)
    .await?
    .into_iter()
    .collect())
}

/// The junction rows of many workspaces in one statement, keyed by workspace,
/// each with its row's `version` -- which is what a `sessions-set` delta carries
/// beside the membership, so the two come from one join rather than two reads.
pub(crate) async fn junction(
    connection: &mut SqliteConnection,
    workspace_ids: &[String],
) -> Result<BTreeMap<String, (i64, Vec<SessionId>)>, WorkspaceError> {
    let mut grouped: BTreeMap<String, (i64, Vec<SessionId>)> = BTreeMap::new();
    for (workspace_id, version, session_id) in sqlx::query_as::<_, (String, i64, String)>(
        "SELECT s.workspace_id, w.version, s.session_id FROM workspace_sessions s \
         JOIN workspaces w ON w.id = s.workspace_id \
         WHERE s.workspace_id IN (SELECT value FROM json_each(?)) \
         ORDER BY s.workspace_id, s.session_id",
    )
    .bind(id_list(workspace_ids)?)
    .fetch_all(&mut *connection)
    .await?
    {
        let entry = grouped.entry(workspace_id).or_insert((version, Vec::new()));
        entry.1.push(SessionId::try_from(session_id)?);
    }
    Ok(grouped)
}

/// One workspace's members, in the junction's own order.
pub(crate) async fn members_of(
    connection: &mut SqliteConnection,
    workspace_id: &str,
) -> Result<Vec<SessionId>, WorkspaceError> {
    junction(connection, std::slice::from_ref(&workspace_id.to_owned()))
        .await
        .map(|grouped| grouped.remove(workspace_id).map_or_else(Vec::new, |(_, ids)| ids))
}

/// THE row projection: the list, every response and every delta are built here.
pub(crate) fn project(row: &Row, session_ids: Vec<SessionId>) -> Result<Workspace, WorkspaceError> {
    Ok(Workspace {
        id: WorkspaceId::try_from(row.id.clone())?,
        worker_fp: WorkerFp::try_from(row.worker_fp.clone())?,
        name: row.name.clone(),
        folder_path: row.folder_path.clone(),
        color: row.color.clone(),
        position: row.position,
        version: row.version,
        created_at_ms: row.created_at_ms,
        updated_at_ms: row.updated_at_ms,
        session_ids,
    })
}

/// The distinct values in first-seen order: a session listed twice is one member.
pub(crate) fn unique(values: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    values
        .iter()
        .filter(|value| seen.insert((*value).clone()))
        .cloned()
        .collect()
}
