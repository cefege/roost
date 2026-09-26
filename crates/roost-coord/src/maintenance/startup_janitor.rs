//! The boot-time purge of durable rows that cannot represent live state after
//! a restart.
//!
//! Owned by the coordinator. `run_startup_janitor` runs after migrations and
//! tenancy and BEFORE any sync feed installs bus listeners, so its deletes
//! publish no deltas: a reconnecting SPA learns about the pruning from the
//! sync feed's seed snapshot, and that seed is what protects the sidebar
//! (`startup-janitor.ts:1-6`).

use std::future::Future;

use crate::db::CoordDb;

/// What the janitor deleted, and how many statements refused to run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JanitorReport {
    /// Closed sessions purged.
    pub deleted_sessions: u64,
    /// Junction rows whose workspace had no open session.
    pub pruned_workspace_sessions: u64,
    /// Workspaces left with no junction rows at all.
    pub pruned_orphan_workspaces: u64,
    /// Statements that threw. Every throw is caught.
    pub failures: usize,
}

/// Purge durable rows that cannot represent live coordinator state after boot.
///
/// Never returns an error: a janitor that refuses to boot the coordinator is
/// strictly worse than a coordinator that boots with some closed sessions still
/// in it (`startup-janitor.ts:45-47`). Each statement is guarded on its own, so
/// one failure does not skip the two that follow it.
pub async fn run_startup_janitor(database: &CoordDb) -> JanitorReport {
    // Closed sessions are DELETED, not parked (no "closed" limbo). This never
    // touches an 'open' row -- a live long-running terminal must never be
    // deleted by a janitor, and truly-dead open sessions are reconciled by the
    // worker snapshot's ghost-close on reconnect, not by an age cutoff. There is
    // no time window anywhere in this function, and that is the point: a
    // wall-clock cutoff is the bug that comment forbids
    // (`startup-janitor.ts:15-19`).
    let deleted_sessions = guarded("delete closed sessions", || async {
        sqlx::query("DELETE FROM sessions WHERE status = 'closed'")
            .execute(database.pool())
            .await
            .map(|outcome| outcome.rows_affected())
    })
    .await;

    // Junction rows for workspaces that have no OPEN session.
    let pruned_workspace_sessions = guarded("prune junction rows", || async {
        sqlx::query(
            "DELETE FROM workspace_sessions WHERE workspace_id NOT IN (\
               SELECT ws.workspace_id FROM workspace_sessions ws \
               INNER JOIN sessions s ON s.id = ws.session_id WHERE s.status = 'open')",
        )
        .execute(database.pool())
        .await
        .map(|outcome| outcome.rows_affected())
    })
    .await;

    // Workspaces with no junction rows. Deleted WITHOUT workspace bus deltas:
    // this runs before the sync feeds install their bus listeners, so any
    // publish here is a structurally guaranteed no-op (`startup-janitor.ts:29-37`).
    let pruned_orphan_workspaces = guarded("prune orphan workspaces", || async {
        sqlx::query(
            "DELETE FROM workspaces WHERE id NOT IN \
             (SELECT workspace_id FROM workspace_sessions)",
        )
        .execute(database.pool())
        .await
        .map(|outcome| outcome.rows_affected())
    })
    .await;

    let outcomes = [
        deleted_sessions,
        pruned_workspace_sessions,
        pruned_orphan_workspaces,
    ];
    let report = JanitorReport {
        deleted_sessions: outcomes[0].rows,
        pruned_workspace_sessions: outcomes[1].rows,
        pruned_orphan_workspaces: outcomes[2].rows,
        failures: outcomes.iter().filter(|outcome| outcome.failed).count(),
    };
    if report.failures == 0 {
        tracing::info!(
            deleted_sessions = report.deleted_sessions,
            pruned_orphan_workspaces = report.pruned_orphan_workspaces,
            "startup janitor"
        );
    }
    report
}

/// One statement's outcome: how many rows it removed, and whether it threw.
struct StatementOutcome {
    rows: u64,
    failed: bool,
}

/// Run one statement, turning a throw into a warning and a zero.
async fn guarded<F, Fut>(what: &'static str, statement: F) -> StatementOutcome
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<u64, sqlx::Error>>,
{
    match statement().await {
        Ok(rows) => StatementOutcome {
            rows,
            failed: false,
        },
        Err(error) => {
            tracing::warn!(statement = what, error = %error, "startup janitor statement failed");
            StatementOutcome {
                rows: 0,
                failed: true,
            }
        }
    }
}
