//! Boot steps 2-4 as `db::open` performs them: the pre-migration backup is
//! taken only when a file already existed AND a migration is pending, and a
//! database whose rows violate a foreign key is refused rather than opened.
//!
//! Both are refusals a later step depends on: an archive spent on a restart that
//! migrated nothing evicts a real nightly one, and an orphan row opened as a
//! coordinator database binds a port over data no handler's joins expect.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use async_compression::tokio::bufread::GzipDecoder;
use tokio::io::AsyncReadExt;

use roost_coord::db::DbError;
use roost_coord::maintenance::backup::{backups_dir, list_archives};
use roost_host::DatabaseLocation;

/// A scratch directory, removed on drop.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-boot-database-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        Self { root }
    }

    fn database_path(&self) -> PathBuf {
        self.root.join("coord.db")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A plain connection with foreign keys OFF, the way a hand repair or a bad
/// import would write an orphan the coordinator's own handle never could.
async fn unenforced_connection(path: &Path) -> sqlx::SqlitePool {
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .foreign_keys(false);
    sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("a raw connection")
}

async fn archive_count(database_path: &Path) -> usize {
    list_archives(&backups_dir(database_path)).await.len()
}

/// Ports `apps/coord/src/main.ts:60-65` (the hook is passed only when
/// `existsSync(cfg.dbPath)`): a file the connect itself created is not backed up.
#[tokio::test]
async fn a_fresh_database_is_migrated_without_a_backup() {
    let scratch = Scratch::new("fresh");
    let database = roost_coord::db::open(&DatabaseLocation::SqliteFile(scratch.database_path()))
        .await
        .expect("a fresh file opens and migrates");
    database.pool().close().await;

    assert_eq!(archive_count(&scratch.database_path()).await, 0);
}

/// Ports `apps/coord/tests/db/backup.test.ts` "migration callback receives all
/// pending names exactly once before applying" and `migrate.ts:319`: the hook
/// runs only when something is pending, so a restart spends no archive.
#[tokio::test]
async fn reopening_a_migrated_database_takes_no_backup() {
    let scratch = Scratch::new("reopen");
    for _ in 0..3 {
        let database =
            roost_coord::db::open(&DatabaseLocation::SqliteFile(scratch.database_path()))
                .await
                .expect("the file opens");
        database.pool().close().await;
    }

    assert_eq!(
        archive_count(&scratch.database_path()).await,
        0,
        "a restart that migrated nothing must not spend one of the kept archives"
    );
}

/// Ports `apps/coord/tests/db/backup.test.ts` "scheduled and pre-migration
/// archives are consistent during WAL activity" (the pre-migration half): an
/// existing file with a pending migration is archived before it is migrated,
/// and the archive holds the rows the file had before the migration.
#[tokio::test]
async fn an_existing_file_with_a_pending_migration_is_backed_up_first() {
    let scratch = Scratch::new("pending");
    let raw = unenforced_connection(&scratch.database_path()).await;
    sqlx::query("CREATE TABLE operator_notes (body TEXT)")
        .execute(&raw)
        .await
        .expect("a pre-existing table");
    sqlx::query("INSERT INTO operator_notes (body) VALUES ('kept')")
        .execute(&raw)
        .await
        .expect("a pre-existing row");
    raw.close().await;

    let database = roost_coord::db::open(&DatabaseLocation::SqliteFile(scratch.database_path()))
        .await
        .expect("the existing file migrates");
    database.pool().close().await;

    let archives = list_archives(&backups_dir(&scratch.database_path())).await;
    assert_eq!(archives.len(), 1, "exactly one pre-migration archive");
    let restored = scratch.root.join("restored.db");
    let compressed = std::fs::read(&archives[0]).expect("the archive reads");
    let mut decompressed = Vec::new();
    GzipDecoder::new(&compressed[..])
        .read_to_end(&mut decompressed)
        .await
        .expect("the archive decompresses");
    std::fs::write(&restored, &decompressed).expect("the archive restores");
    let restored_pool = unenforced_connection(&restored).await;
    let (body,): (String,) = sqlx::query_as("SELECT body FROM operator_notes")
        .fetch_one(&restored_pool)
        .await
        .expect("the archived row");
    assert_eq!(body, "kept");
    let (migrated,): (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE name = 'authorized_keys')",
    )
    .fetch_one(&restored_pool)
    .await
    .expect("the archived schema");
    assert!(!migrated, "the archive predates the migration it protects");
    restored_pool.close().await;
}

/// Ports `apps/coord/tests/workers/worker-tombstone-migration.test.ts`
/// ("0025 foreign key check failed") and `migration-validation.ts`
/// `validateForeignKeys`: an orphan row refuses the open and the refusal names
/// the violating table and its parent.
#[tokio::test]
async fn a_database_with_a_foreign_key_violation_refuses_to_open() {
    let scratch = Scratch::new("fk-violation");
    let database = roost_coord::db::open(&DatabaseLocation::SqliteFile(scratch.database_path()))
        .await
        .expect("a clean database opens");
    database.pool().close().await;

    let raw = unenforced_connection(&scratch.database_path()).await;
    sqlx::query(
        "INSERT INTO account_devices (fingerprint, account_id, added_at_ms, last_seen_at_ms) \
         VALUES ('orphan-key', 'no-such-account', 1, 1)",
    )
    .execute(&raw)
    .await
    .expect("an orphan the enforced handle would refuse");
    raw.close().await;

    let refused = roost_coord::db::open(&DatabaseLocation::SqliteFile(scratch.database_path()))
        .await
        .expect_err("an orphan row refuses the open");
    let DbError::ForeignKeyCheck { rows, violations } = &refused else {
        panic!("expected a foreign key refusal, got {refused}");
    };
    assert_eq!(*rows, 2, "one row, two broken references");
    assert!(
        violations.contains("account_devices -> authorized_keys")
            && violations.contains("account_devices -> accounts"),
        "the refusal names each violating table and parent: {violations}"
    );
}
