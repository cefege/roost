//! What a workspace delete takes with it: the membership read, the unclaim, and
//! the conditioned delete that may only run after both. Split from
//! `workspaces.rs`, which owns the row shape and every write that is not a
//! delete.
//!
//! THE ORDER IS THE WHOLE POINT and the three live together for that reason:
//! `workspace_sessions` cascades away with the workspace row, so a junction read
//! taken after the delete has nothing left to read and the sessions the
//! workspace held keep a `sessions.workspace_id` naming a row that no longer
//! exists. All of it is one transaction, so a version claim that loses rolls the
//! detach back with it.

use sqlx::SqliteConnection;

use roost_protocol::wire::{SessionId, WorkspaceId};
use crate::db::CoordDb;
use crate::sessions::workspaces::{WorkspaceError, members_of};

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

/// The membership a delete takes with it: READ IT, unclaim it, and only then may
/// the caller delete the row. All in one transaction, so a version claim that
/// loses rolls the detach back with it.
///
/// THE JUNCTION READ COMES FIRST, because `workspace_sessions` cascades away with
/// the workspace row: read after the delete and there is nothing to read, and the
/// sessions the workspace held keep a `sessions.workspace_id` naming a row that
/// no longer exists -- the half-deleted tree. The same rule in the other
/// direction is why `cascade_closed_session` captures the owning workspaces
/// before it deletes a session.
pub(crate) async fn detach_members(
    connection: &mut SqliteConnection,
    workspace_id: &str,
) -> Result<(), WorkspaceError> {
    let members = members_of(connection, workspace_id).await?;
    unclaim(connection, workspace_id, &members).await
}

/// Clear the column for exactly these sessions, guarded on BOTH the id it names
/// and the workspace going away, so a session the column already attributes
/// elsewhere is not stolen out of it. The membership is an argument rather than
/// a read, because one caller must read it before a statement cascades it away.
pub(crate) async fn unclaim(
    connection: &mut SqliteConnection,
    workspace_id: &str,
    members: &[SessionId],
) -> Result<(), WorkspaceError> {
    let ids: Vec<String> = members.iter().map(|id| id.as_str().to_owned()).collect();
    sqlx::query(
        "UPDATE sessions SET workspace_id = NULL WHERE workspace_id = ? \
         AND id IN (SELECT value FROM json_each(?))",
    )
    .bind(workspace_id)
    .bind(serde_json::to_string(&ids)?)
    .execute(&mut *connection)
    .await?;
    Ok(())
}
