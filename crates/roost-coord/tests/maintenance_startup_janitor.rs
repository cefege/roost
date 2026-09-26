//! The boot-time janitor's promises: closed rows go, open rows never do, there
//! is no time window at all, and nothing it touches can stop the coordinator
//! from booting.
//!
//! The last one is a janitor that throws. A refusal here is a dead coordinator
//! over a table that a migration had not finished creating, which is the
//! opposite of what a boot-time repair step is for.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant;
use roost_coord::db::CoordDb;
use roost_coord::maintenance::startup_janitor::run_startup_janitor;
use sqlx::AssertSqlSafe;

const WORKER_FP: &str = "aa00000000000000000000000000000000000000000000000000000000000000";

struct JanitorFixture {
    database: CoordDb,
    dashboard_id: String,
    root: PathBuf,
}

impl JanitorFixture {
    async fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!("roost-janitor-{label}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database = roost_coord::db::open(&root.join("coord.db"))
            .await
            .expect("a migrated database");
        let tenant = ensure_self_hosted_tenant(&database, 1_000)
            .await
            .expect("an empty database is a self-hosted deployment to create");
        let fixture = Self {
            database,
            dashboard_id: tenant.dashboard_id,
            root,
        };
        fixture.seed_tenant().await;
        fixture
    }

    /// One open session in a live workspace, one closed session in a workspace
    /// that has nothing else, and one workspace that never had a session.
    async fn seed_tenant(&self) {
        self.exec(
            "INSERT INTO workers (fp, label, os, registered_at_ms, last_seen_ms, dashboard_id) \
             VALUES ('aa00000000000000000000000000000000000000000000000000000000000000', 'laptop', 'linux', 0, 0, '{dashboard}')",
        )
        .await;
        for workspace in ["ws-live", "ws-closed", "ws-empty"] {
            self.exec(&format!(
                "INSERT INTO workspaces (id, worker_fp, name, created_at_ms, updated_at_ms, dashboard_id) \
                 VALUES ('{workspace}', '{WORKER_FP}', '{workspace}', 0, 0, '{{dashboard}}')"
            ))
            .await;
        }
        self.session("s-open", "open", "ws-live").await;
        self.session("s-closed", "closed", "ws-closed").await;
    }

    /// A session created at the epoch, so any age cutoff would be visible.
    async fn session(&self, id: &str, status: &str, workspace_id: &str) {
        self.exec(&format!(
            "INSERT INTO sessions (id, worker_fp, channel, kind, cwd, workspace_id, status, created_at, dashboard_id) \
             VALUES ('{id}', '{WORKER_FP}', 0, 'shell', '/tmp', '{workspace_id}', '{status}', 0, '{{dashboard}}')"
        ))
        .await;
        self.exec(&format!(
            "INSERT INTO workspace_sessions (workspace_id, session_id, added_at_ms, dashboard_id) \
             VALUES ('{workspace_id}', '{id}', 0, '{{dashboard}}')"
        ))
        .await;
    }

    async fn exec(&self, sql: &str) {
        let sql = sql
            .replace("{WORKER_FP}", WORKER_FP)
            .replace("{dashboard}", &self.dashboard_id);
        sqlx::query(AssertSqlSafe(sql.clone()))
            .execute(self.database.pool())
            .await
            .unwrap_or_else(|error| panic!("{sql} applies: {error}"));
    }

    async fn column(&self, table: &str, column: &str) -> Vec<String> {
        let sql = format!("SELECT {column} FROM {table} ORDER BY {column}");
        let rows: Vec<(String,)> = sqlx::query_as(AssertSqlSafe(sql))
            .fetch_all(self.database.pool())
            .await
            .expect("the column reads");
        rows.into_iter().map(|row| row.0).collect()
    }
}

impl Drop for JanitorFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn a_closed_session_and_the_workspaces_it_leaves_behind_are_gone() {
    let fixture = JanitorFixture::new("purge").await;

    let report = run_startup_janitor(&fixture.database).await;

    assert_eq!(report.failures, 0);
    assert_eq!(report.deleted_sessions, 1);
    assert_eq!(
        fixture.column("sessions", "id").await,
        ["s-open"],
        "a live long-running terminal is never deleted by a janitor"
    );
    assert_eq!(
        fixture.column("workspaces", "id").await,
        ["ws-live"],
        "a workspace whose only session is closed has nothing left to show"
    );
    assert_eq!(
        fixture.column("workspace_sessions", "workspace_id").await,
        ["ws-live"]
    );
}

#[tokio::test]
async fn there_is_no_time_window_at_all() {
    let fixture = JanitorFixture::new("no-window").await;
    // Both sessions were created at the epoch. A janitor with an age cutoff
    // would delete the open one along with the closed one.
    assert_eq!(
        fixture.column("sessions", "id").await.len(),
        2,
        "the fixture really is old"
    );

    run_startup_janitor(&fixture.database).await;

    assert_eq!(
        fixture.column("sessions", "id").await,
        ["s-open"],
        "reconciling a dead open session is the worker snapshot's ghost-close, not an age cutoff"
    );
}

#[tokio::test]
async fn a_second_run_has_nothing_left_to_do() {
    let fixture = JanitorFixture::new("idempotent").await;

    run_startup_janitor(&fixture.database).await;
    let second = run_startup_janitor(&fixture.database).await;

    assert_eq!(second.failures, 0);
    assert_eq!(second.deleted_sessions, 0);
    assert_eq!(second.pruned_orphan_workspaces, 0);
    assert_eq!(fixture.column("sessions", "id").await, ["s-open"]);
}

#[tokio::test]
async fn a_statement_that_throws_does_not_stop_the_others_or_the_boot() {
    let fixture = JanitorFixture::new("throws").await;
    // The first statement refuses, and only it: the trigger fires on a closed
    // session, which is the one row the janitor's first statement removes.
    fixture
        .exec(
            "CREATE TRIGGER refuse_closed_purge BEFORE DELETE ON sessions \
               WHEN OLD.status = 'closed' \
               BEGIN SELECT RAISE(ABORT, 'refused'); END",
        )
        .await;

    let report = run_startup_janitor(&fixture.database).await;

    assert_eq!(
        report.failures, 1,
        "the throw is caught and counted, not propagated"
    );
    assert_eq!(report.deleted_sessions, 0, "that statement removed nothing");
    assert_eq!(
        report.pruned_workspace_sessions, 1,
        "the statement after the failure still ran"
    );
    assert_eq!(
        report.pruned_orphan_workspaces, 2,
        "and so did the one after that"
    );
    assert_eq!(
        fixture.column("sessions", "id").await,
        ["s-closed", "s-open"],
        "the closed session is still there, because its purge was refused"
    );
    assert_eq!(fixture.column("workspaces", "id").await, ["ws-live"]);

    // The refused statement was attempted, not skipped: with the trigger gone
    // the next run removes exactly what the first one could not.
    fixture.exec("DROP TRIGGER refuse_closed_purge").await;
    let retry = run_startup_janitor(&fixture.database).await;
    assert_eq!(retry.failures, 0);
    assert_eq!(retry.deleted_sessions, 1);
    assert_eq!(fixture.column("sessions", "id").await, ["s-open"]);
    assert_eq!(fixture.column("workspaces", "id").await, ["ws-live"]);
}
