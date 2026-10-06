//! The workspace tree: the row, the order a client sees, the membership a write
//! moves, the cascade a delete performs. `rpc_workspaces.rs` is the only caller.

use std::collections::{BTreeMap, BTreeSet};

use roost_platform::same_worker_folder;
use roost_protocol::ProtocolError;
use roost_protocol::wire::{SessionId, WorkerFp, Workspace, WorkspaceId};
use sqlx::{AnyConnection, AssertSqlSafe, FromRow};

use crate::coord_core::ids;
use crate::db::{CoordDb, IN_LIST_CHUNK, SqlBuilder, push_in_list};

/// What a delete takes with it lives in [`super::workspace_delete`], because the
/// detach and the unclaim are only ever correct immediately before the row
/// goes -- a reader that finds the three apart is the half-deleted tree this
/// file's own comments warn about. Re-exported so every caller keeps one path.
pub use super::workspace_delete::delete_workspace;
pub(crate) use super::workspace_delete::{detach_members, unclaim};

/// The `workspaces` columns one projection reads, named once so the ordered
/// list, a write's read-back and a delta cannot read different sets.
pub(crate) const COLUMNS: &str =
    "id, worker_fp, name, folder_path, color, position, version, created_at_ms, updated_at_ms";

/// What one create produced, and whether it inserted a row. The flag is not
/// decoration: a create is idempotent per `(worker, folder)`, so a second browser
/// opening the same folder must get the SAME id back with its version intact, and
/// only a row that was actually written is published.
///
/// THE WORKSPACE IS THE WIRE TYPE, not the proto one: `WorkspaceDelta`'s
/// `created`/`updated` arms carry `wire::Workspace`, and returning the proto
/// message here would force a second projection. `rpc_workspaces::
/// workspace_to_proto` is the single one, and it reads this value.
#[derive(Debug, Clone)]
pub struct CreatedWorkspace {
    /// The stored row, as the tree holds it.
    pub workspace: Workspace,
    /// False when an existing row for the folder was returned unchanged.
    pub created: bool,
}

/// Why a workspace read or write could not be completed.
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    /// The statement failed, or the transaction could not commit.
    #[error("database: {0}")]
    Sqlite(#[from] sqlx::Error),
    /// The kernel CSPRNG could not be read for a new workspace id.
    #[error("entropy: {0}")]
    Entropy(#[from] std::io::Error),
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

/// Every workspace, in the order a client sees them. ONE query and ONE
/// projection answer it, so a client that re-fetches and one that applies a
/// delta cannot disagree; `position` alone is not total, so the id breaks the tie.
pub async fn list_workspaces(database: &CoordDb) -> Result<Vec<Workspace>, WorkspaceError> {
    let mut connection = database.pool().acquire().await?;
    let rows: Vec<Row> = sqlx::query_as(AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM workspaces ORDER BY position, id"
    )))
    .fetch_all(&mut *connection)
    .await?;
    let ids: Vec<String> = rows.iter().map(|row| row.id.clone()).collect();
    let membership = junction(&mut connection, &ids).await?;
    rows.into_iter()
        .map(|row| {
            let members = membership
                .get(&row.id)
                .map_or_else(Vec::new, |e| e.1.clone());
            project(&row, members)
        })
        .collect()
}

/// Add a workspace, or answer with the one already at that folder. The dedupe
/// scan runs INSIDE the insert transaction because `same_worker_folder` folds
/// `/tmp` onto `/private/tmp`: two creates for one folder must serialise behind
/// SQLite's write lock, not each slip past an already-committed SELECT.
pub(crate) async fn create_workspace(
    database: &CoordDb,
    request: &roost_proto::WorkspacesCreateRequest,
    dashboard_id: &str,
    now_ms: i64,
) -> Result<CreatedWorkspace, WorkspaceError> {
    let mut transaction = database.pool().begin().await?;
    let Some(os) = sqlx::query_scalar::<_, String>(
        "SELECT os FROM workers WHERE fp = $1 AND deleted_at_ms IS NULL",
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
    let existing = sqlx::query_as::<_, Row>(AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM workspaces WHERE worker_fp = $1"
    )))
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
    let id = ids::render_v4(ids::draw::<16>()?);
    let row = sqlx::query_as::<_, Row>(AssertSqlSafe(format!(
        "INSERT INTO workspaces (id, dashboard_id, worker_fp, name, folder_path, color, position, \
                version, created_at_ms, updated_at_ms) VALUES ($1, $2, $3, $4, $5, $6, $7, 0, $8, $9) \
         RETURNING {COLUMNS}"
    )))
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
/// The version moves even when the request named no field: the call claimed a write.
pub async fn update_workspace(
    database: &CoordDb,
    request: &roost_proto::WorkspacesUpdateRequest,
    now_ms: i64,
) -> Result<Workspace, WorkspaceError> {
    let mut transaction = database.pool().begin().await?;
    let mut update = SqlBuilder::new("UPDATE workspaces SET ");
    {
        let mut separated = update.separated(", ");
        for (column, value) in [
            ("name", request.name.as_deref()),
            ("folder_path", request.folder_path.as_deref()),
            ("color", request.color.as_deref()),
        ] {
            if let Some(value) = value {
                // `push_bind` on a `Separated` EMITS THE SEPARATOR FIRST
                // (sqlx-core 0.9, `query_builder.rs:589`), and `push(" = ")`
                // has already armed it. Written the 0.8 way this rendered
                // `SET name,  = ?` and EVERY `update_workspace` was a
                // `near ","` syntax error surfacing as `Internal` — the
                // version guard never ran, so a stale write reported an
                // internal fault instead of `FailedPrecondition`.
                separated
                    .push(column)
                    .push_unseparated(" = ")
                    .push_bind_unseparated(value);
            }
        }
        if let Some(position) = request.position {
            separated
                .push("position")
                .push_unseparated(" = ")
                .push_bind_unseparated(i64::from(position));
        }
        separated
            .push("updated_at_ms")
            .push_unseparated(" = ")
            .push_bind_unseparated(now_ms);
        separated.push("version = version + 1");
    }
    // The predicate goes on the QUERY BUILDER, not on `separated`. A
    // `Separated` inserts a COMMA between its elements, so a `WHERE` pushed
    // onto one renders `..., version = version + 1, WHERE id = ?` — a
    // different syntax error from the same mistake.
    update.push(" WHERE id = ").push_bind(&request.id);
    update.push(" AND version = ");
    update.push_bind(i64::try_from(request.if_version).unwrap_or(i64::MAX));
    update.push(" RETURNING ").push(COLUMNS);
    let row = update
        .build_query_as::<Row>()
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(WorkspaceError::VersionMismatch)?;
    let session_ids = members_of(&mut transaction, &request.id).await?;
    let workspace = project(&row, session_ids)?;
    transaction.commit().await?;
    Ok(workspace)
}

/// Point `session_ids` at `workspace_id` on BOTH representations, because a
/// session belongs to one workspace and the two rows are one fact the browser
/// reads twice: writing only the junction double-counts a session as a member
/// and as an orphan. So this MOVES it: sweep, insert, then the column.
pub(crate) async fn set_membership(
    connection: &mut AnyConnection,
    workspace_id: &str,
    dashboard_id: &str,
    session_ids: &[String],
    now_ms: i64,
) -> Result<(), WorkspaceError> {
    for chunk in session_ids.chunks(IN_LIST_CHUNK) {
        let mut sweep = SqlBuilder::new("DELETE FROM workspace_sessions WHERE session_id IN ");
        push_in_list(&mut sweep, chunk);
        sweep.build().execute(&mut *connection).await?;
        let mut insert = SqlBuilder::new(
            "INSERT INTO workspace_sessions (workspace_id, session_id, added_at_ms, dashboard_id) ",
        );
        insert.push_values(chunk, |mut row, session_id| {
            row.push_bind(workspace_id)
                .push_bind(session_id)
                .push_bind(now_ms)
                .push_bind(dashboard_id);
        });
        insert.build().execute(&mut *connection).await?;
        let mut claim = SqlBuilder::new("UPDATE sessions SET workspace_id = ");
        claim.push_bind(workspace_id).push(" WHERE id IN ");
        push_in_list(&mut claim, chunk);
        claim.build().execute(&mut *connection).await?;
    }
    Ok(())
}

pub(crate) async fn session_cwds(
    connection: &mut AnyConnection,
    session_ids: &[String],
) -> Result<BTreeMap<String, String>, WorkspaceError> {
    let mut cwds = BTreeMap::new();
    for chunk in session_ids.chunks(IN_LIST_CHUNK) {
        let mut statement = SqlBuilder::new("SELECT id, cwd FROM sessions WHERE id IN ");
        push_in_list(&mut statement, chunk);
        cwds.extend(
            statement
                .build_query_as::<(String, String)>()
                .fetch_all(&mut *connection)
                .await?,
        );
    }
    Ok(cwds)
}

/// The junction rows of many workspaces in one statement, each with its row's
/// `version` -- what a `sessions-set` delta carries beside the membership.
pub(crate) async fn junction(
    connection: &mut AnyConnection,
    workspace_ids: &[String],
) -> Result<BTreeMap<String, (i64, Vec<SessionId>)>, WorkspaceError> {
    let mut grouped: BTreeMap<String, (i64, Vec<SessionId>)> = BTreeMap::new();
    for chunk in workspace_ids.chunks(IN_LIST_CHUNK) {
        let mut statement = SqlBuilder::new(
            "SELECT s.workspace_id, w.version, s.session_id FROM workspace_sessions s \
             JOIN workspaces w ON w.id = s.workspace_id WHERE s.workspace_id IN ",
        );
        push_in_list(&mut statement, chunk);
        statement.push(" ORDER BY s.workspace_id, s.session_id");
        for (workspace_id, version, session_id) in statement
            .build_query_as::<(String, i64, String)>()
            .fetch_all(&mut *connection)
            .await?
        {
            let entry = grouped.entry(workspace_id).or_insert((version, Vec::new()));
            entry.1.push(SessionId::try_from(session_id)?);
        }
    }
    Ok(grouped)
}

pub(crate) async fn members_of(
    connection: &mut AnyConnection,
    workspace_id: &str,
) -> Result<Vec<SessionId>, WorkspaceError> {
    junction(connection, std::slice::from_ref(&workspace_id.to_owned()))
        .await
        .map(|mut grouped| {
            grouped
                .remove(workspace_id)
                .map_or_else(Vec::new, |(_, ids)| ids)
        })
}

/// THE row projection: the list, every response and every delta are built here,
/// which is what makes "the same source" true rather than claimed.
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

/// The distinct values, in first-seen order: a session listed twice is one row.
pub(crate) fn unique(values: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    values
        .iter()
        .filter(|value| seen.insert((*value).clone()))
        .cloned()
        .collect()
}
