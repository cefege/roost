//! `db::postgres_to_sqlite` against a real Postgres server: a SQLite file
//! copied into Postgres and back is the same file, row for row and byte for
//! byte, with its triggers back in place and its ids continuing; a target
//! holding rows is refused unless replaced.
//!
//! Runs only when `ROOST_TEST_DATABASE_URL` is set — CI's Postgres job sets it.

use std::path::{Path, PathBuf};

use roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant;
use roost_coord::db::postgres_to_sqlite::copy_postgres_to_sqlite;
use roost_coord::db::sqlite_to_postgres::{ExistingRows, TransferError, copy_sqlite_to_postgres};
use roost_coord::db::{self, CoordDb};
use roost_host::DatabaseLocation;
use sqlx::AssertSqlSafe;

use super::db_support;
use super::sqlite_to_postgres::{insert_event, seed_source};

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("roost-pg-to-sqlite-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        Self { root }
    }

    async fn postgres(&self) -> Option<String> {
        if !db_support::running_on_postgres() {
            return None;
        }
        match db_support::test_database_location(&self.root.join("pg")).await {
            DatabaseLocation::Postgres(url) => Some(url),
            DatabaseLocation::SqliteFile(_) => None,
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

async fn open_sqlite(path: &Path) -> CoordDb {
    db::open(&DatabaseLocation::SqliteFile(path.to_path_buf()))
        .await
        .expect("the SQLite file opens as a coordinator would")
}

/// Every table's rows, rendered by SQLite's own `quote()` (exact for integers,
/// text and blobs), ordered by every column, plus the trigger definitions.
async fn contents(database: &CoordDb) -> Vec<String> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' \
         AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' AND name <> '_sqlx_migrations' ORDER BY name",
    )
    .fetch_all(database.pool())
    .await
    .expect("the table list");
    let mut rendered = Vec::new();
    for table in tables {
        let columns: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info($1)")
            .bind(&table)
            .fetch_all(database.pool())
            .await
            .expect("the column list");
        let quoted: Vec<String> = columns
            .iter()
            .map(|column| format!("quote(\"{column}\")"))
            .collect();
        let order: Vec<String> = (1..=columns.len()).map(|index| index.to_string()).collect();
        let rows: Vec<String> = sqlx::query_scalar(AssertSqlSafe(format!(
            "SELECT {} FROM \"{table}\" ORDER BY {}",
            quoted.join(" || '|' || "),
            order.join(", ")
        )))
        .fetch_all(database.pool())
        .await
        .expect("the rows");
        rendered.push(format!("{table}: {}", rows.join(" / ")));
    }
    let triggers: Vec<String> =
        sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE type = 'trigger' ORDER BY name")
            .fetch_all(database.pool())
            .await
            .expect("the triggers");
    rendered.extend(triggers);
    rendered
}

#[tokio::test]
async fn a_round_trip_through_postgres_lands_the_same_file_and_ids_continue() {
    let scratch = Scratch::new("round-trip");
    let Some(postgres) = scratch.postgres().await else {
        return;
    };
    let original = scratch.root.join("original.db");
    let returned = scratch.root.join("nested/returned.db");
    seed_source(&original).await;
    copy_sqlite_to_postgres(&original, &postgres, ExistingRows::Refuse)
        .await
        .expect("the forward copy");

    let report = copy_postgres_to_sqlite(&postgres, &returned, ExistingRows::Refuse)
        .await
        .expect("the reverse copy into a new file in a new directory");
    assert!(report.total_rows() > 0, "{report:?}");

    let original_db = open_sqlite(&original).await;
    let returned_db = open_sqlite(&returned).await;
    assert_eq!(
        contents(&original_db).await,
        contents(&returned_db).await,
        "every row, byte and trigger comes back as it left"
    );
    let tenant = ensure_self_hosted_tenant(&returned_db, 5_000)
        .await
        .expect("the copied tenant is the one tenant");
    let next_id = insert_event(&returned_db, 40, &tenant.dashboard_id)
        .await
        .expect("a new event after the copy");
    assert_eq!(next_id, 4, "the id continues past the largest copied id");
    original_db.pool().close().await;
    returned_db.pool().close().await;
}

#[tokio::test]
async fn a_target_with_rows_is_refused_unless_replaced_and_a_replace_does_not_double() {
    let scratch = Scratch::new("replace");
    let Some(postgres) = scratch.postgres().await else {
        return;
    };
    let original = scratch.root.join("original.db");
    let returned = scratch.root.join("returned.db");
    seed_source(&original).await;
    copy_sqlite_to_postgres(&original, &postgres, ExistingRows::Refuse)
        .await
        .expect("the forward copy");
    copy_postgres_to_sqlite(&postgres, &returned, ExistingRows::Refuse)
        .await
        .expect("the first reverse copy");

    let refused = copy_postgres_to_sqlite(&postgres, &returned, ExistingRows::Refuse)
        .await
        .expect_err("a second copy into the same file");
    assert!(
        matches!(&refused, TransferError::TargetNotEmpty { tables } if tables.contains("events")),
        "{refused}"
    );
    copy_postgres_to_sqlite(&postgres, &returned, ExistingRows::Replace)
        .await
        .expect("a replacing copy");
    let original_db = open_sqlite(&original).await;
    let returned_db = open_sqlite(&returned).await;
    assert_eq!(contents(&original_db).await, contents(&returned_db).await);
    original_db.pool().close().await;
    returned_db.pool().close().await;
}
