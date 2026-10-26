//! Immutable, ordered SQLite schema history (port of
//! `src/storage/sqlite/migrations.ts` @ `abe508e1b`).

use crate::errors::{StorageError, StorageResult};
use crate::storage::sqlite::database::SqliteDatabase;

/// One schema migration step.
#[derive(Debug, Clone)]
pub struct SqliteMigration {
    /// Contiguous version starting at 1.
    pub version: u64,
    /// Statements applied in order inside the migration transaction.
    pub statements: Vec<String>,
}

// next_id is TEXT because SQLite bindings reject INTEGER values outside the
// JavaScript safe integer range.
fn initial_schema() -> Vec<String> {
    vec![
        r#"CREATE TABLE durable_metadata (
		singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
		next_id TEXT NOT NULL,
		next_seq INTEGER NOT NULL
	) STRICT"#
            .to_string(),
        "INSERT INTO durable_metadata (singleton, next_id, next_seq) VALUES (1, '2', 1)".to_string(),
        r#"CREATE TABLE record_ids (
		id INTEGER PRIMARY KEY,
		record_type TEXT NOT NULL CHECK (record_type IN ('conversation', 'entry', 'task', 'submission', 'document'))
	) STRICT"#
            .to_string(),
        r#"CREATE TABLE conversations (
		id INTEGER PRIMARY KEY,
		owner_conversation_id INTEGER,
		owner_task_id INTEGER,
		record TEXT NOT NULL CHECK (json_valid(record))
	) STRICT"#
            .to_string(),
        "CREATE INDEX conversations_by_owner_conversation ON conversations (owner_conversation_id, id)".to_string(),
        "CREATE INDEX conversations_by_owner_task ON conversations (owner_task_id, id)".to_string(),
        r#"CREATE TABLE entries (
		id INTEGER PRIMARY KEY,
		conversation_id INTEGER NOT NULL,
		head INTEGER,
		commit_seq INTEGER NOT NULL,
		record TEXT NOT NULL CHECK (json_valid(record))
	) STRICT"#
            .to_string(),
        "CREATE INDEX entries_by_conversation ON entries (conversation_id, id DESC)".to_string(),
        "CREATE INDEX entry_heads_by_conversation ON entries (conversation_id, id DESC) WHERE head IS NOT NULL"
            .to_string(),
        r#"CREATE TABLE tasks (
		id INTEGER PRIMARY KEY,
		conversation_id INTEGER NOT NULL,
		kind TEXT NOT NULL,
		status TEXT NOT NULL CHECK (status IN ('pending', 'running', 'waiting', 'completing', 'terminal')),
		abort_requested INTEGER NOT NULL CHECK (abort_requested IN (0, 1)),
		background INTEGER NOT NULL CHECK (background IN (0, 1)),
		record TEXT NOT NULL CHECK (json_valid(record))
	) STRICT"#
            .to_string(),
        "CREATE INDEX tasks_by_status ON tasks (status, id)".to_string(),
        "CREATE INDEX tasks_by_conversation ON tasks (conversation_id, id)".to_string(),
        "CREATE INDEX tasks_by_kind ON tasks (kind, id)".to_string(),
        "CREATE INDEX tasks_by_abort_requested ON tasks (abort_requested, id)".to_string(),
        "CREATE INDEX tasks_by_background ON tasks (background, id)".to_string(),
        r#"CREATE TABLE submissions (
		id INTEGER PRIMARY KEY,
		conversation_id INTEGER NOT NULL,
		request_id TEXT,
		status TEXT NOT NULL CHECK (status IN ('queued', 'placed', 'done', 'unanswered')),
		record TEXT NOT NULL CHECK (json_valid(record))
	) STRICT"#
            .to_string(),
        "CREATE INDEX submissions_by_request ON submissions (conversation_id, request_id)".to_string(),
        "CREATE INDEX submissions_by_conversation ON submissions (conversation_id, id)".to_string(),
        "CREATE INDEX submissions_by_status ON submissions (status, id)".to_string(),
        r#"CREATE TABLE documents (
		id INTEGER PRIMARY KEY,
		kind TEXT NOT NULL,
		family INTEGER NOT NULL CHECK (family IN (0, 1)),
		key_value TEXT NOT NULL,
		scope_kind TEXT NOT NULL CHECK (scope_kind IN ('session', 'conversation', 'task')),
		owner_id INTEGER NOT NULL,
		created_at INTEGER NOT NULL,
		retired_at INTEGER,
		record TEXT NOT NULL CHECK (json_valid(record))
	) STRICT"#
            .to_string(),
        r#"CREATE INDEX documents_by_address
		ON documents (kind, scope_kind, owner_id, family, key_value, created_at DESC, retired_at)"#
            .to_string(),
        "CREATE INDEX documents_by_scope ON documents (scope_kind, owner_id, id)".to_string(),
        "CREATE INDEX documents_by_scope_kind ON documents (scope_kind, owner_id, kind, id)".to_string(),
        r#"CREATE TABLE document_revisions (
		document_id INTEGER NOT NULL,
		seq INTEGER NOT NULL,
		kind TEXT NOT NULL CHECK (kind IN ('base', 'delta')),
		version INTEGER NOT NULL,
		content TEXT NOT NULL CHECK (json_valid(content)),
		PRIMARY KEY (document_id, seq)
	) STRICT"#
            .to_string(),
        "CREATE INDEX document_revisions_by_kind ON document_revisions (document_id, kind, seq DESC)".to_string(),
    ]
}
fn sleeping_tasks_schema() -> Vec<String> {
    vec![
        "ALTER TABLE tasks RENAME TO tasks_v1".to_string(),
        r#"CREATE TABLE tasks (
            id INTEGER PRIMARY KEY,
            conversation_id INTEGER NOT NULL,
            kind TEXT NOT NULL,
            status TEXT NOT NULL CHECK (status IN ('pending', 'running', 'waiting', 'sleeping', 'completing', 'terminal')),
            abort_requested INTEGER NOT NULL CHECK (abort_requested IN (0, 1)),
            background INTEGER NOT NULL CHECK (background IN (0, 1)),
            record TEXT NOT NULL CHECK (json_valid(record))
        ) STRICT"#
            .to_string(),
        "INSERT INTO tasks (id, conversation_id, kind, status, abort_requested, background, record) SELECT id, conversation_id, kind, status, abort_requested, background, record FROM tasks_v1".to_string(),
        "DROP TABLE tasks_v1".to_string(),
        "CREATE INDEX tasks_by_status ON tasks (status, id)".to_string(),
        "CREATE INDEX tasks_by_conversation ON tasks (conversation_id, id)".to_string(),
        "CREATE INDEX tasks_by_kind ON tasks (kind, id)".to_string(),
        "CREATE INDEX tasks_by_abort_requested ON tasks (abort_requested, id)".to_string(),
        "CREATE INDEX tasks_by_background ON tasks (background, id)".to_string(),
    ]
}

/// Latest schema version supported by this build.
pub const CURRENT_SQLITE_SCHEMA_VERSION: u64 = 2;

/// Immutable, ordered schema history. Append new migrations after the initial
/// schema ships.
pub fn sqlite_migrations() -> Vec<SqliteMigration> {
    vec![
        SqliteMigration {
            version: 1,
            statements: initial_schema(),
        },
        SqliteMigration {
            version: 2,
            statements: sleeping_tasks_schema(),
        },
    ]
}

/// Apply all pending schema migrations atomically.
pub async fn apply_sqlite_migrations(
    db: &SqliteDatabase,
    migrations: &[SqliteMigration],
) -> StorageResult<()> {
    for (index, migration) in migrations.iter().enumerate() {
        if migration.version != index as u64 + 1 {
            return Err(StorageError::message(
                "Durable SQLite migrations must have contiguous versions starting at 1",
            ));
        }
    }
    let current_version = migrations
        .last()
        .map(|migration| migration.version)
        .unwrap_or(0);
    db.transaction(move |tx| async move {
        tx.exec(
            r#"CREATE TABLE IF NOT EXISTS durable_schema (
			singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
			version INTEGER NOT NULL CHECK (version >= 0)
		) STRICT"#,
        )
        .await?;
        tx.run("INSERT OR IGNORE INTO durable_schema (singleton, version) VALUES (1, 0)", vec![]).await?;
        let row = tx
            .get_i64("SELECT version FROM durable_schema WHERE singleton = 1", vec![])
            .await?;
        let Some(version) = row else {
            return Err(StorageError::message("Durable SQLite schema metadata is missing"));
        };
        let version = version as u64;
        if version > current_version {
            return Err(StorageError::message(format!(
                "Durable SQLite schema version {version} is newer than supported version {current_version}"
            )));
        }
        for migration in migrations {
            if migration.version <= version {
                continue;
            }
            for statement in &migration.statements {
                tx.exec(statement).await?;
            }
            tx.run(
                "UPDATE durable_schema SET version = ? WHERE singleton = 1",
                vec![migration.version.into()],
            )
            .await?;
        }
        Ok(())
    })
    .await
}
