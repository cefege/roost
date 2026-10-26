//! SQLite backend: asynchronous facade, migrations and storage.

pub mod database;
pub mod migrations;
pub mod node;
pub mod storage;

pub use database::{
    DEFAULT_BUSY_TIMEOUT_MS, DEFAULT_WAL_AUTO_CHECKPOINT_PAGES, SqliteDatabase,
    SqliteDatabaseOptions, SqliteExecutor, SqliteTransaction, SqliteValue, SqliteValueRef,
};
pub use migrations::{
    CURRENT_SQLITE_SCHEMA_VERSION, SqliteMigration, apply_sqlite_migrations, sqlite_migrations,
};
pub use node::{open_node_sqlite_database, open_node_sqlite_storage};
pub use storage::SqliteStorage;
