//! The four statements an append transaction projects with, and the order they
//! run in.
//!
//! Ported from the statement half of `apps/coord/src/events/event-projection.ts`
//! and the projection arms of `event-transaction.ts:185-272`. Every one of them
//! takes the transaction's own connection: a projection write that reached the
//! pool instead would commit on its own and break the ordering the whole
//! subsystem exists to keep.
//!
//! WHY A SNAPSHOT UPDATES THIRTEEN COLUMNS AND NOT NINETEEN. The conflict arm
//! takes the **worker-owned** columns only: every column the worker tracks takes
//! the announced value -- `channel` above all, because a reconcile can hand a
//! session a new keeper channel, and a row left on the old one re-primes the
//! dead route on the next coordinator restart, and lies to the pre-reconcile
//! route fallback -- while the coordinator-owned and user-owned columns are
//! immutable across a worker snapshot: original creation/spawn time, workspace
//! grouping, custom title, and the retained dashboard scope
//! (`event-projection.ts:171-186`).
//!
//! THE BREADCRUMB MODEL. Sessions open in the coordinator but **absent** from a
//! worker's snapshot are not pruned: a worker restart kills the PTY, and the row
//! survives as an offline breadcrumb so the sidebar still shows where you were
//! working. Only an explicit `closed` -- a real PTY exit -- or the user's ✕
//! removes it. This mirrors the fold's snapshot case, so the browser's projection
//! and the coordinator's cannot disagree about what exists.
//!
//! `id` IS NOT IN THE FULL-ROW UPDATE. It is the key the statement matched on, so
//! assigning it to itself is a no-op SQLite must still rewrite the row for. The
//! `dashboard_id` column **is** in that statement, because the tenancy trigger
//! re-asserts the scope on every write of those columns and a row whose scope
//! disagreed with the caller is a row the trigger is there to stop.

use std::collections::HashSet;

use roost_protocol::wire::{Session, SessionEvent, SessionMap, WorkspaceId};
use sqlx::QueryBuilder;
use sqlx::Row;
use sqlx::sqlite::SqliteConnection;

use crate::events::projection::{ProjectionError, load_session, session_to_row};

/// Delete a session's junction rows and any workspace left with no sessions,
/// returning the workspaces that were orphaned.
///
/// The ownership read happens **first**: `workspace_sessions` cascades away with
/// the session, so capturing the evidence and then deleting it in the other order
/// loses the answer (`event-projection.ts:96-99`). The caller publishes the
/// returned ids as workspace deletions *after* the transaction commits.
///
/// This is the ONLY deletion trigger in the projection, and it is shared by the
/// `closed` event and by the synthetic ghost-close path inside a snapshot --
/// which is why a worker restart that snapshotted without a session no longer
/// leaves an orphaned junction row and parent workspace behind until the next
/// coordinator-startup janitor (`event-projection.ts:83-86`).
pub async fn cascade_closed_session(
    connection: &mut SqliteConnection,
    session_id: &str,
) -> Result<Vec<WorkspaceId>, ProjectionError> {
    let owner_ids = workspace_ids_for_session(connection, session_id).await?;
    sqlx::query("DELETE FROM workspace_sessions WHERE session_id = ?")
        .bind(session_id)
        .execute(&mut *connection)
        .await?;
    if owner_ids.is_empty() {
        return Ok(Vec::new());
    }

    let still_has_sessions = workspaces_with_sessions(connection, &owner_ids).await?;
    let orphans = owner_ids
        .into_iter()
        .filter(|id| !still_has_sessions.contains(id.as_str()))
        .collect::<Vec<_>>();
    if !orphans.is_empty() {
        let mut delete = QueryBuilder::<sqlx::Sqlite>::new("DELETE FROM workspaces WHERE id IN (");
        {
            let mut separated = delete.separated(", ");
            for id in &orphans {
                separated.push_bind(id.as_str());
            }
            separated.push_unseparated(")");
        }
        delete.build().execute(&mut *connection).await?;
    }
    Ok(orphans)
}

/// Upsert every session a worker announced, then report the one that lost a race.
///
/// One statement per session, because the tenancy triggers fire per row and the
/// statement count is what an operator reads when a snapshot is refused.
///
/// `None` means the session row was already there and did not belong to this
/// append's caller, which the append answers by deleting its own `events` row so a
/// losing insert leaves no phantom log row (`event-transaction.ts:208-227`).
pub async fn project_snapshot_sessions(
    connection: &mut SqliteConnection,
    sessions: &[Session],
    dashboard_id: &str,
) -> Result<(), ProjectionError> {
    for session in sessions {
        let row = session_to_row(session, dashboard_id);
        sqlx::query(
            "INSERT INTO sessions (id, dashboard_id, worker_fp, channel, kind, cwd, workspace_id, \
                    status, created_at, closed_at, custom_title, git_branch, git_remote, pr_number, \
                    pr_state, pr_checks, pr_url, ports_json, spawn_cwd) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET \
                    worker_fp = excluded.worker_fp, channel = excluded.channel, \
                    kind = excluded.kind, cwd = excluded.cwd, status = excluded.status, \
                    closed_at = excluded.closed_at, git_branch = excluded.git_branch, \
                    git_remote = excluded.git_remote, pr_number = excluded.pr_number, \
                    pr_state = excluded.pr_state, pr_checks = excluded.pr_checks, \
                    pr_url = excluded.pr_url, ports_json = excluded.ports_json",
        )
        .bind(row.id)
        .bind(row.dashboard_id)
        .bind(row.worker_fp)
        .bind(row.channel)
        .bind(row.kind)
        .bind(row.cwd)
        .bind(row.workspace_id)
        .bind(row.status)
        .bind(row.created_at)
        .bind(row.closed_at)
        .bind(row.custom_title)
        .bind(row.git_branch)
        .bind(row.git_remote)
        .bind(row.pr_number)
        .bind(row.pr_state)
        .bind(row.pr_checks)
        .bind(row.pr_url)
        .bind(row.ports_json)
        .bind(row.spawn_cwd)
        .execute(&mut *connection)
        .await?;
    }
    Ok(())
}

/// Insert the row an `opened` event folds into, and report whether this call won.
///
/// `None` is the lost race: the row already existed. The caller compares that
/// against whether the row existed *before* the transaction, because only the
/// combination "it did not exist, and the insert lost" is a refusal
/// (`event-transaction.ts:208-227`).
pub async fn insert_opened_session(
    connection: &mut SqliteConnection,
    session: &Session,
    dashboard_id: &str,
) -> Result<bool, ProjectionError> {
    let row = session_to_row(session, dashboard_id);
    let inserted = sqlx::query(
        "INSERT INTO sessions (id, dashboard_id, worker_fp, channel, kind, cwd, workspace_id, \
                status, created_at, closed_at, custom_title, git_branch, git_remote, pr_number, \
                pr_state, pr_checks, pr_url, ports_json, spawn_cwd) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(id) DO NOTHING",
    )
    .bind(row.id)
    .bind(row.dashboard_id)
    .bind(row.worker_fp)
    .bind(row.channel)
    .bind(row.kind)
    .bind(row.cwd)
    .bind(row.workspace_id)
    .bind(row.status)
    .bind(row.created_at)
    .bind(row.closed_at)
    .bind(row.custom_title)
    .bind(row.git_branch)
    .bind(row.git_remote)
    .bind(row.pr_number)
    .bind(row.pr_state)
    .bind(row.pr_checks)
    .bind(row.pr_url)
    .bind(row.ports_json)
    .bind(row.spawn_cwd)
    .execute(&mut *connection)
    .await?;
    Ok(inserted.rows_affected() == 1)
}

/// Delete the session row a `closed` event retires.
///
/// The cascade has already run, so this is a plain idempotent delete: a `closed`
/// for a session that is already gone changes nothing, which is what makes a
/// double close and a deduped replay both harmless.
pub async fn delete_session(
    connection: &mut SqliteConnection,
    session_id: &str,
) -> Result<(), ProjectionError> {
    sqlx::query("DELETE FROM sessions WHERE id = ?")
        .bind(session_id)
        .execute(&mut *connection)
        .await?;
    Ok(())
}

/// Fold an event onto its stored row and write the result back.
///
/// `None` means the fold produced no row for this session, which is a no-op event
/// on a projection that does not hold the session -- the same answer the fold
/// itself gives (`event-transaction.ts:243-256`).
pub async fn fold_and_update_session(
    connection: &mut SqliteConnection,
    event: &SessionEvent,
    dashboard_id: &str,
) -> Result<Option<Session>, ProjectionError> {
    let Some(session_id) = event.session_id() else {
        return Ok(None);
    };
    let Some(existing) = load_session(&mut *connection, session_id.as_str()).await? else {
        return Ok(None);
    };
    let mut projection = SessionMap::new();
    projection.insert(existing.id.clone(), existing);
    let folded = roost_protocol::wire::fold_event(&projection, event);
    let Some(updated) = folded.get(session_id).cloned() else {
        return Ok(None);
    };
    update_session_row(connection, &updated, dashboard_id).await?;
    Ok(Some(updated))
}

/// Reassign a session's workspace membership, on both representations.
///
/// `sessions.workspace_id` and the `workspace_sessions` junction are two
/// representations of one fact and the browser reads both, so writing only the
/// column leaves a session out of the junction and double-counts it as both a
/// member and an orphan (`apps/coord/src/sessions/handlers-sessions.ts:277-281`).
pub async fn set_workspace_membership(
    connection: &mut SqliteConnection,
    session_id: &str,
    workspace_id: Option<&str>,
    dashboard_id: &str,
    added_at_ms: i64,
) -> Result<(), ProjectionError> {
    sqlx::query("DELETE FROM workspace_sessions WHERE session_id = ?")
        .bind(session_id)
        .execute(&mut *connection)
        .await?;
    let Some(workspace_id) = workspace_id else {
        return Ok(());
    };
    sqlx::query(
        "INSERT INTO workspace_sessions (workspace_id, session_id, added_at_ms, dashboard_id) \
         VALUES (?, ?, ?, ?) ON CONFLICT DO NOTHING",
    )
    .bind(workspace_id)
    .bind(session_id)
    .bind(added_at_ms)
    .bind(dashboard_id)
    .execute(&mut *connection)
    .await?;
    Ok(())
}

/// Write a folded row back over the stored one.
async fn update_session_row(
    connection: &mut SqliteConnection,
    session: &Session,
    dashboard_id: &str,
) -> Result<(), ProjectionError> {
    let row = session_to_row(session, dashboard_id);
    sqlx::query(
        "UPDATE sessions SET dashboard_id = ?, worker_fp = ?, channel = ?, kind = ?, cwd = ?, \
                workspace_id = ?, status = ?, created_at = ?, closed_at = ?, custom_title = ?, \
                git_branch = ?, git_remote = ?, pr_number = ?, pr_state = ?, pr_checks = ?, \
                pr_url = ?, ports_json = ?, spawn_cwd = ? WHERE id = ?",
    )
    .bind(row.dashboard_id)
    .bind(row.worker_fp)
    .bind(row.channel)
    .bind(row.kind)
    .bind(row.cwd)
    .bind(row.workspace_id)
    .bind(row.status)
    .bind(row.created_at)
    .bind(row.closed_at)
    .bind(row.custom_title)
    .bind(row.git_branch)
    .bind(row.git_remote)
    .bind(row.pr_number)
    .bind(row.pr_state)
    .bind(row.pr_checks)
    .bind(row.pr_url)
    .bind(row.ports_json)
    .bind(row.spawn_cwd)
    .bind(row.id)
    .execute(&mut *connection)
    .await?;
    Ok(())
}

/// The workspaces a session is a member of, deduplicated and in first-seen order.
async fn workspace_ids_for_session(
    connection: &mut SqliteConnection,
    session_id: &str,
) -> Result<Vec<WorkspaceId>, ProjectionError> {
    // No literal `?` in the fragment: a QueryBuilder counts every hole in the
    // text it is given, so a placeholder here plus a `push_bind` is two holes and
    // one argument.
    let mut query = QueryBuilder::<sqlx::Sqlite>::new(
        "SELECT workspace_id FROM workspace_sessions WHERE session_id = ",
    );
    query.push_bind(session_id);
    let rows = query.build().fetch_all(&mut *connection).await?;
    let mut seen: Vec<WorkspaceId> = Vec::new();
    for row in rows {
        let id: String = row.get(0);
        if seen.iter().all(|kept| kept.as_str() != id) {
            seen.push(
                WorkspaceId::try_from(id).map_err(|error| ProjectionError::StoredRow {
                    field: "workspace_sessions.workspace_id",
                    reason: error.reason,
                })?,
            );
        }
    }
    Ok(seen)
}

/// Which of the named workspaces still have at least one session.
async fn workspaces_with_sessions(
    connection: &mut SqliteConnection,
    workspace_ids: &[WorkspaceId],
) -> Result<HashSet<String>, ProjectionError> {
    let mut query = QueryBuilder::<sqlx::Sqlite>::new(
        "SELECT DISTINCT workspace_id FROM workspace_sessions WHERE workspace_id IN (",
    );
    {
        let mut separated = query.separated(", ");
        for id in workspace_ids {
            separated.push_bind(id.as_str());
        }
        separated.push_unseparated(")");
    }
    let rows = query.build().fetch_all(&mut *connection).await?;
    Ok(rows
        .into_iter()
        .map(|row| row.get::<String, _>(0))
        .collect())
}
