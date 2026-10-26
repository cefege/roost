//! Node-adapter equivalents: open a file-backed database or storage with the
//! upstream defaults (port of `src/storage/sqlite/node.ts` entry points @ `abe508e1b`).

use crate::errors::StorageResult;
use crate::storage::sqlite::database::{SqliteDatabase, SqliteDatabaseOptions};
use crate::storage::sqlite::storage::SqliteStorage;

/// Open and configure a SQLite database facade.
pub async fn open_node_sqlite_database(
    path: &str,
    options: SqliteDatabaseOptions,
) -> StorageResult<SqliteDatabase> {
    SqliteDatabase::open(path, options).await
}

/// Open or create file-backed durable storage.
pub async fn open_node_sqlite_storage(
    path: &str,
    options: SqliteDatabaseOptions,
) -> StorageResult<SqliteStorage> {
    SqliteStorage::open(SqliteDatabase::open(path, options).await?).await
}
