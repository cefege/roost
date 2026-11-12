//! Populates the complete pre-Rpi schema and snapshots retained row values.
//! Called by rpi_migration against the selected real SQLx backend.
//! Column-name ordering makes snapshots independent of SQLite column replacement.

use roost_coord::db::{self, CoordDb};
use roost_host::DatabaseLocation;
use sqlx::any::AnyPoolOptions;
use sqlx::migrate::Migrator;
use sqlx::{AnyPool, AssertSqlSafe};

use super::db_support;

const RETAINED_TABLES: &[&str] = &[
    "accounts",
    "agent_conversations",
    "agent_entries",
    "agent_credentials",
    "agent_credential_blocks",
    "agent_credential_sticky",
    "agent_settings",
    "agent_commands",
    "agent_run_leases",
    "agent_chat_events",
    "agent_logins",
    "agent_tool_calls",
    "agent_advisor_state",
    "agent_prompt_history",
    "agent_durable_batches",
    "agent_durable_ids",
    "agent_schedules",
    "omp_sessions",
    "omp_inputs",
    "omp_settings",
    "omp_worker_catalogs",
    "omp_host_services",
];

pub(super) async fn open_through_0019(location: &DatabaseLocation) -> AnyPool {
    sqlx::any::install_default_drivers();
    let (url, embedded) = match location {
        DatabaseLocation::SqliteFile(path) => {
            (db::sqlite_url(path), sqlx::migrate!("./migrations/sqlite"))
        }
        DatabaseLocation::Postgres(url) => (url.clone(), sqlx::migrate!("./migrations/postgres")),
    };
    let sqlite = matches!(location, DatabaseLocation::SqliteFile(_));
    let pool = AnyPoolOptions::new()
        .max_connections(1)
        .after_connect(move |connection, _| {
            Box::pin(async move {
                if sqlite {
                    sqlx::query("PRAGMA foreign_keys = ON")
                        .execute(connection)
                        .await?;
                }
                Ok(())
            })
        })
        .connect(&url)
        .await
        .expect("an empty database with foreign keys enabled");
    let mut migrator = Migrator::with_migrations(
        embedded
            .iter()
            .filter(|migration| migration.version <= 19)
            .cloned()
            .collect(),
    );
    migrator.set_locking(false);
    let mut transaction = pool.begin().await.expect("a migration transaction");
    migrator
        .run_direct(None, &mut *transaction, false)
        .await
        .expect("the complete schema through 0019");
    transaction.commit().await.expect("commit pre-Rpi schema");
    let version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .expect("the applied migration boundary");
    assert_eq!(version, 19);
    pool
}

pub(super) async fn insert_conversation(
    pool: &AnyPool,
    conversation_id: &str,
    backend: Option<&str>,
    parent_id: Option<&str>,
) -> sqlx::Result<()> {
    let (backend_column, backend_value) = if backend.is_some() {
        (", backend", ", $3")
    } else {
        ("", "")
    };
    let statement = format!(
        "INSERT INTO agent_conversations \
         (id, title, worker_fp, worker_label, worker_os, cwd, thinking_level, mode, \
          parent_id, run_state, created_ms, updated_ms{backend_column}) \
         VALUES ($1, 'saved conversation', 'worker', 'saved worker', 'linux', '/scratch', \
                 'high', 'normal', $2, 'idle', 100, 200{backend_value})"
    );
    let query = sqlx::query(AssertSqlSafe(statement))
        .bind(conversation_id)
        .bind(parent_id);
    let query = if let Some(backend) = backend {
        query.bind(backend)
    } else {
        query
    };
    query.execute(pool).await?;
    Ok(())
}

pub(super) async fn seed_retained_rows(pool: &AnyPool) {
    insert_conversation(pool, "native-parent", None, None)
        .await
        .expect("the stored default is native");
    insert_conversation(pool, "native-child", Some("native"), Some("native-parent"))
        .await
        .expect("an archived parent relationship");
    insert_conversation(pool, "omp", Some("omp"), Some("native-parent"))
        .await
        .expect("an active OMP projection");
    for statement in [
        "INSERT INTO accounts (id, email_normalized, status, created_at_ms) \
         VALUES ('account', 'migration@example.invalid', 'active', 1)",
        "UPDATE agent_conversations SET model_provider = 'saved-provider', model_id = 'saved-model', \
         pre_plan_model = 'saved-plan', agent = 'saved-agent', error = 'retained error', \
         revision = 19, permission_profile = 'approve_execution', leaf_seq = 2, journal_format = 2, \
         tree_revision = 8, branched_from_conversation_id = 'native-parent', branched_from_seq = 1, \
         additional_dirs = '[\"/other\"]', provider_session_key = 'opaque-session' \
         WHERE id = 'native-child'",
        "INSERT INTO agent_entries (conversation_id, seq, entry_json, created_ms, parent_seq) \
         VALUES ('native-child', 1, '{\"type\":\"message\",\"text\":\"retained α\"}', 11, NULL), \
                ('native-child', 2, '{\"type\":\"leaf\",\"opaque\":\"bytes\"}', 12, 1)",
        "INSERT INTO agent_credentials \
         (id, provider, kind, identity_key, label, data_json, disabled_cause, created_ms, updated_ms) \
         VALUES (41, 'saved-provider', 'oauth', 'identity', 'old login', \
                 '{\"ciphertext\":\"AAEC/+/=\",\"nonce\":\"AQID==\"}', NULL, 1, 2), \
                (42, 'other-provider', 'api_key', 'other-identity', 'old key', \
                 '{\"ciphertext\":\"BAUG/+/=\"}', 'disabled', 3, 4)",
        "INSERT INTO agent_credential_blocks VALUES (41, 9000)",
        "INSERT INTO agent_credential_sticky VALUES ('native-child', 'saved-provider', 41)",
        "INSERT INTO agent_settings VALUES ('saved', '{\"opaque\":\"settings\"}')",
        "INSERT INTO agent_commands VALUES ('native-child', 'old-request', 'submit', \
         '{\"opaque\":\"result\"}', 13)",
        "INSERT INTO agent_run_leases VALUES ('native-child', 'old-instance', 3, 9001)",
        "INSERT INTO agent_chat_events VALUES ('native-child', 1, '[{\"opaque\":\"event\"}]', 1, 14)",
        "INSERT INTO agent_logins VALUES ('old-login', 'saved-provider', \
         '{\"ciphertext\":\"BwgJ==\"}', 9002)",
        "INSERT INTO agent_tool_calls VALUES ('old-call', 'native-child', 'worker', 3, \
         'completed', '{\"opaque\":\"tool result\"}', 15, 16)",
        "INSERT INTO agent_advisor_state (conversation_id, roster_slug, state_json, updated_ms) \
         VALUES ('native-child', 'reviewer', '{\"opaque\":\"advisor\"}', 17)",
        "INSERT INTO agent_prompt_history (account_id, worker_fp, cwd, text, created_ms) \
         VALUES ('account', 'worker', '/scratch', 'saved prompt α', 18)",
        "INSERT INTO agent_durable_batches VALUES ('native-child', 1, \
         '[{\"opaque\":\"journal\"}]', '[1,2]', 19)",
        "INSERT INTO agent_durable_ids VALUES ('native-child', 99)",
        "INSERT INTO agent_schedules VALUES ('native-child', 'old-schedule', \
         'saved schedule', 9003, 1000, 0, 20)",
        "INSERT INTO omp_sessions (conversation_id, acp_session_id, current_model, \
         current_thinking_level, applied_title, run_id, worker_epoch, applied_seq, \
         transcript_cursor, transcript_json, omp_ui_transport_id) \
         VALUES ('omp', 'acp', 'omp-model', 'medium', 'OMP title', 'omp-run', 'omp-epoch', \
                 4294967297, 7, '{\"opaque\":\"OMP transcript\"}', 'transport')",
        "INSERT INTO omp_inputs (conversation_id, request_id, kind, created_ms, fingerprint, \
         client_id, tab_id, view_id, transport_id, worker_fp, receipt, expires_ms) \
         VALUES ('omp', 'omp-input', 'ui', 21, 'device', 'client', 'tab', 'view', \
                 'transport', 'worker', 1, 9004)",
        "INSERT INTO omp_settings VALUES ('defaults', '', '', 'omp-default', 'low', 22), \
         ('folder', 'worker', '/scratch', 'omp-folder', 'high', 23)",
        "INSERT INTO omp_worker_catalogs VALUES ('worker', '[{\"id\":\"omp-model\"}]', 24, \
         '[{\"id\":\"review\"}]')",
        "INSERT INTO omp_host_services VALUES ('service', 'omp', 'omp-input', 'device', \
         'client', 'tab', 'view', 'transport', 'worker', 'clipboard', 25, 9005)",
    ] {
        sqlx::query(statement)
            .execute(pool)
            .await
            .unwrap_or_else(|error| panic!("seed retained row: {statement}: {error}"));
    }
    sqlx::query("UPDATE agent_conversations SET advisor = $1, pinned = $1, parked = $1, read_only = $1 WHERE id = 'native-child'")
        .bind(true)
        .execute(pool)
        .await
        .expect("retained boolean flags");
}

pub(super) async fn snapshot(pool: &AnyPool) -> Vec<(String, Vec<String>)> {
    let mut snapshot = Vec::new();
    for table in RETAINED_TABLES {
        let columns_query = if db_support::running_on_postgres() {
            "SELECT column_name::text FROM information_schema.columns \
             WHERE table_schema = current_schema() AND table_name = $1 ORDER BY column_name"
        } else {
            "SELECT name FROM pragma_table_info($1) ORDER BY name"
        };
        let columns: Vec<String> = sqlx::query_scalar(columns_query)
            .bind(*table)
            .fetch_all(pool)
            .await
            .expect("retained table columns");
        assert!(!columns.is_empty(), "missing retained table {table}");
        let values = columns
            .iter()
            .map(|column| format!("CAST(\"{column}\" AS TEXT)"))
            .collect::<Vec<_>>()
            .join(", ");
        let expression = if db_support::running_on_postgres() {
            format!("CAST(json_build_array({values}) AS TEXT)")
        } else {
            format!("json_array({values})")
        };
        let mut rows: Vec<String> =
            sqlx::query_scalar(AssertSqlSafe(format!("SELECT {expression} FROM {table}")))
                .fetch_all(pool)
                .await
                .expect("retained row bytes");
        assert!(!rows.is_empty(), "fixture did not populate {table}");
        rows.sort();
        snapshot.push(((*table).to_owned(), rows));
    }
    snapshot
}

pub(super) async fn assert_foreign_keys(database: &CoordDb) {
    if matches!(database.backend(), db::DbBackend::Sqlite) {
        let enabled: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(database.pool())
            .await
            .expect("foreign key enforcement");
        assert_eq!(enabled, 1);
        assert!(
            sqlx::query("PRAGMA foreign_key_check")
                .fetch_all(database.pool())
                .await
                .expect("full-schema foreign key check")
                .is_empty()
        );
    }
}
