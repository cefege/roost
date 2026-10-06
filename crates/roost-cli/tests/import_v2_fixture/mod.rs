//! A throwaway v2 coordinator database and a throwaway v3 one, for
//! `tests/import_v2_copy.rs`. Depends on the coordinator's own migration, so the
//! schema under test is the schema the command will really read.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use roost_cli::import_v2::copy::{self, TableReport};
use roost_cli::import_v2::plan::ImportMode;
use roost_host::DatabaseLocation;
use sqlx::AssertSqlSafe;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};

/// A v2 database and a v3 database, each in a throwaway tree that removes
/// itself.
pub struct Fixture {
    /// The tree, kept so `Drop` can remove it.
    pub root: PathBuf,
    /// The stand-in for the v2 coordinator's database.
    pub v2: PathBuf,
    /// Where this install's coordinator would put its own.
    pub v3: PathBuf,
}

impl Fixture {
    /// A v2 database holding the coordinator's schema plus an identity, one
    /// paired browser, two machine keys, one revocation and one setting.
    pub async fn new(case: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-import-v2-{}-{case}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&root).expect("the throwaway tree is created");
        let fixture = Self {
            v2: root.join("v2.db"),
            v3: root.join("coordinator_v3.db"),
            root,
        };
        fixture.seed_v2().await;
        fixture
    }

    /// A pool over a file, creating it if it is not there.
    pub async fn open(path: &Path) -> SqlitePool {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(false);
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .expect("a pool")
    }

    async fn seed_v2(&self) {
        let migration = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../roost-coord/migrations/sqlite/0001_init.sql"),
        )
        .expect("the coordinator's migration is readable");
        let pool = Self::open(&self.v2).await;
        // The migration is a checked-in file, not input, and it is the schema
        // under test rather than a statement this test composes.
        sqlx::raw_sql(AssertSqlSafe(migration))
            .execute(&pool)
            .await
            .expect("the coordinator schema applies to a fresh file");
        for statement in [
            "INSERT INTO accounts (id, email_normalized, password_hash, status, created_at_ms) \
             VALUES ('acct-import', 'owner@roost.test', 'argon2id-hash', 'active', 1)",
            "INSERT INTO organizations (id, slug, name, status, created_at_ms) \
             VALUES ('org-1', 'roost', 'Roost', 'active', 1)",
            "INSERT INTO organization_memberships (organization_id, account_id, role, created_at_ms) \
             VALUES ('org-1', 'acct-import', 'owner', 1)",
            "INSERT INTO dashboards (id, organization_id, slug, name, status, created_at_ms) \
             VALUES ('dash-1', 'org-1', 'main', 'Main', 'active', 1)",
            "INSERT INTO dashboard_memberships (dashboard_id, account_id, role, created_at_ms) \
             VALUES ('dash-1', 'acct-import', 'admin', 1)",
            "INSERT INTO account_devices (fingerprint, account_id, added_at_ms, last_seen_at_ms) \
             VALUES ('aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', \
                     'acct-import', 1, 1)",
            "INSERT INTO app_settings (dashboard_id, key, value, updated_at_ms) \
             VALUES ('dash-1', 'agent.selected', 'omp', 1)",
        ] {
            sqlx::raw_sql(AssertSqlSafe(statement))
                .execute(&pool)
                .await
                .expect("a seeded row");
        }
        // One paired browser and two machines. The machines are the rows the
        // device filter exists to leave behind.
        for (fingerprint, label) in [
            ("a".repeat(64), "paired-browser"),
            ("b".repeat(64), "machine-one"),
            ("c".repeat(64), "machine-two"),
        ] {
            sqlx::query(
                "INSERT INTO authorized_keys (fingerprint, public_key, label, added_at) \
                 VALUES ($1, x'00', $2, 1)",
            )
            .bind(&fingerprint)
            .bind(label)
            .execute(&pool)
            .await
            .expect("a key");
        }
        // The revocation goes in AFTER the keys, because the coordinator's own
        // trigger refuses to insert a key that is already revoked. A fixture
        // seeded the other way round fails with the trigger's message and looks
        // like a defect in the importer.
        sqlx::query(
            "INSERT INTO authorized_key_revocations \
             (fingerprint, revoked_at_ms, revoked_by_fp, reason) VALUES ($1, 2, 'revoker', $2)",
        )
        .bind("c".repeat(64))
        .bind("a machine key that was revoked")
        .execute(&pool)
        .await
        .expect("a revocation covering a machine key");
        pool.close().await;
    }

    /// Pair one more browser, the way an operator would during the cutover
    /// window, so a re-run has something new to carry.
    pub async fn pair_another_browser(&self) -> String {
        let fingerprint = "d".repeat(64);
        let pool = Self::open(&self.v2).await;
        sqlx::query(
            "INSERT INTO authorized_keys (fingerprint, public_key, label, added_at) \
             VALUES ($1, x'00', 'paired-later', 3)",
        )
        .bind(&fingerprint)
        .execute(&pool)
        .await
        .expect("the later key");
        sqlx::query(
            "INSERT INTO account_devices (fingerprint, account_id, added_at_ms, last_seen_at_ms) \
             VALUES ($1, 'acct-import', 3, 3)",
        )
        .bind(&fingerprint)
        .execute(&pool)
        .await
        .expect("the later device");
        pool.close().await;
        fingerprint
    }

    /// Revoke a key on the v2 side, the way the coordinator would when a
    /// browser is unpaired there.
    pub async fn revoke_on_v2(&self, fingerprint: &str) {
        let pool = Self::open(&self.v2).await;
        sqlx::query(
            "INSERT INTO authorized_key_revocations (fingerprint, revoked_at_ms, revoked_by_fp, reason) \
             VALUES ($1, 4, 'revoker', 'unpaired during the cutover window')",
        )
        .bind(fingerprint)
        .execute(&pool)
        .await
        .expect("the revocation");
        pool.close().await;
    }

    /// A raw statement against the v3 database, for the state a test arranges.
    pub async fn on_v3(&self, statement: &str) {
        let pool = Self::open(&self.v3).await;
        sqlx::raw_sql(AssertSqlSafe(statement.to_string()))
            .execute(&pool)
            .await
            .expect("a v3 statement");
        pool.close().await;
    }

    /// One import, through the same path `import_v2::apply` takes.
    pub async fn import(&self) -> (ImportMode, Vec<TableReport>) {
        let source_pool = copy::open_source_read_only(&self.v2)
            .await
            .expect("the source opens read-only");
        let account = copy::source_account(&source_pool)
            .await
            .expect("the source has one account");
        source_pool.close().await;
        let database = roost_coord::db::open(&DatabaseLocation::SqliteFile(self.v3.clone()))
            .await
            .expect("the target opens and migrates");
        copy::attach(database.pool(), &self.v2)
            .await
            .expect("the source attaches");
        let (mode, reports) = copy::apply(database.pool(), &account)
            .await
            .expect("the copy applies");
        // Closed HERE, deliberately, and not left to the drop. SQLite
        // checkpoints the WAL and writes the main file when the LAST
        // connection closes, so a caller that reads the file's bytes the
        // instant this returns can read a file a checkpoint has not finished
        // writing — which is a race in the reader, not in the import. A test
        // that compares a database byte for byte has to say WHEN it snapshots,
        // and this is when.
        database.pool().close().await;
        (mode, reports)
    }

    /// An EMPTY SQLite file where the v3 database would go, created by
    /// opening and closing one: a real database with NO schema, so a dry run
    /// that ran the coordinator's migrating opener would leave tables in it.
    pub async fn empty_target(&self) {
        Self::open(&self.v3).await.close().await;
    }

    /// The same import, insisting on the refusal the caller is asserting about.
    pub async fn import_refused(&self) -> roost_cli::command_error::CommandFailure {
        let source_pool = copy::open_source_read_only(&self.v2)
            .await
            .expect("the source opens read-only");
        let account = copy::source_account(&source_pool)
            .await
            .expect("the source has one account");
        source_pool.close().await;
        let database = roost_coord::db::open(&DatabaseLocation::SqliteFile(self.v3.clone()))
            .await
            .expect("the target opens and migrates");
        copy::attach(database.pool(), &self.v2)
            .await
            .expect("the source attaches");
        let failure = copy::apply(database.pool(), &account)
            .await
            .expect_err("the import must be refused");
        database.pool().close().await;
        failure
    }

    /// A connection closed before the caller looks at the file, for the same
    /// reason [`Fixture::import`] closes its own.
    pub async fn close_target(&self) {
        if !self.v3.exists() {
            return;
        }
        Self::open(&self.v3).await.close().await;
    }

    /// One count against either database.
    pub async fn count(&self, database: &Path, sql: &str) -> i64 {
        let pool = Self::open(database).await;
        let value: i64 = sqlx::query_scalar(AssertSqlSafe(sql))
            .fetch_one(&pool)
            .await
            .expect("the count query");
        pool.close().await;
        value
    }

    /// A count against the v3 database.
    pub async fn count_v3(&self, sql: &str) -> i64 {
        self.count(&self.v3, sql).await
    }

    /// A count against the v2 database.
    pub async fn count_v2(&self, sql: &str) -> i64 {
        self.count(&self.v2, sql).await
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The report line a test is asserting about.
#[must_use]
pub fn report_for<'a>(reports: &'a [TableReport], table: &str) -> &'a TableReport {
    reports
        .iter()
        .find(|report| report.table == table)
        .unwrap_or_else(|| panic!("the report must have a line for {table}"))
}
