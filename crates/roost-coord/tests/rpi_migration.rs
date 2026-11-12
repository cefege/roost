//! Rpi's schema upgrade preserves populated native archives and OMP projections.
//! Uses the actual embedded migration chain and coordinator startup transaction.
//! db_support selects bundled SQLite or an isolated real Postgres database.

use std::path::PathBuf;

use roost_coord::db;
use sqlx::{AnyPool, AssertSqlSafe};

mod db_support;
#[path = "rpi_migration/fixture.rs"]
mod fixture;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-rpi-migration-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch migration directory");
        Self(root)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn rpi_migration_preserves_populated_archives_and_omp() {
    let scratch = Scratch::new();
    let location = db_support::test_database_location(scratch.0.as_path()).await;
    let old_pool = fixture::open_through_0019(&location).await;
    fixture::seed_retained_rows(&old_pool).await;
    assert!(
        fixture::insert_conversation(&old_pool, "premature-rpi", Some("rpi"), None)
            .await
            .is_err(),
        "0019 must still refuse the new backend"
    );
    if db_support::running_on_postgres() {
        sqlx::query(
            "ALTER TABLE agent_conversations RENAME CONSTRAINT \
             agent_conversations_backend_check TO retained_backend_discriminator",
        )
        .execute(&old_pool)
        .await
        .expect("constraint discovery must not assume its old name");
    }
    let before = fixture::snapshot(&old_pool).await;
    old_pool.close().await;

    let database = db::open(&location)
        .await
        .expect("0020 upgrades the populated full schema in the real migration transaction");
    assert_eq!(before, fixture::snapshot(database.pool()).await);
    fixture::assert_foreign_keys(&database).await;
    let applied: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = 20")
            .fetch_one(database.pool())
            .await
            .expect("0020 history");
    assert_eq!(applied, 1);
    assert_backend_constraints(database.pool()).await;
    assert_rpi_constraints_and_deletion(database.pool()).await;
    fixture::assert_foreign_keys(&database).await;
    database.pool().close().await;

    let reopened = db::open(&location)
        .await
        .expect("the migrated database reopens without replacing retained data");
    fixture::assert_foreign_keys(&reopened).await;
    reopened.pool().close().await;
}

async fn assert_backend_constraints(pool: &AnyPool) {
    for (conversation_id, backend) in [
        ("new-default", None),
        ("new-native", Some("native")),
        ("new-omp", Some("omp")),
        ("rpi", Some("rpi")),
    ] {
        fixture::insert_conversation(pool, conversation_id, backend, None)
            .await
            .expect("all three stored backends and the native default remain valid");
        let stored: String =
            sqlx::query_scalar("SELECT backend FROM agent_conversations WHERE id = $1")
                .bind(conversation_id)
                .fetch_one(pool)
                .await
                .expect("a stored backend");
        assert_eq!(stored, backend.unwrap_or("native"));
    }
    for backend in ["", "unknown", "RPI"] {
        assert!(
            fixture::insert_conversation(pool, "invalid-backend", Some(backend), None)
                .await
                .is_err(),
            "stored discriminator must reject {backend:?}"
        );
    }
    assert!(
        sqlx::query("UPDATE agent_conversations SET backend = NULL WHERE id = 'rpi'")
            .execute(pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query(
            "UPDATE agent_conversations SET permission_profile = 'invalid' WHERE id = 'rpi'"
        )
        .execute(pool)
        .await
        .is_err(),
        "expanding backend admission must not drop an unrelated constraint"
    );
    let parent: Option<String> =
        sqlx::query_scalar("SELECT parent_id FROM agent_conversations WHERE id = 'native-child'")
            .fetch_one(pool)
            .await
            .expect("the retained parent relationship");
    assert_eq!(parent.as_deref(), Some("native-parent"));
}

async fn assert_rpi_constraints_and_deletion(pool: &AnyPool) {
    sqlx::query("INSERT INTO rpi_sessions (conversation_id, transcript_json) VALUES ('rpi', '{}')")
        .execute(pool)
        .await
        .expect("a new Rpi session");
    let counters: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT next_command_seq, last_stop_seq, applied_event_seq, transcript_cursor \
         FROM rpi_sessions WHERE conversation_id = 'rpi'",
    )
    .fetch_one(pool)
    .await
    .expect("Rpi counter defaults");
    assert_eq!(counters, (0, 0, 0, 0));
    let unbound: (Option<String>, Option<String>, Option<i64>) = sqlx::query_as(
        "SELECT actor_id, bound_worker_epoch, deleted_ms FROM rpi_sessions WHERE conversation_id = 'rpi'",
    )
    .fetch_one(pool)
    .await
    .expect("a new session has no actor binding or deletion");
    assert_eq!(unbound, (None, None, None));
    let resumed: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM rpi_sessions WHERE needs_resume = $1")
            .bind(false)
            .fetch_one(pool)
            .await
            .expect("resume defaults to false");
    assert_eq!(resumed, 1);
    for column in [
        "next_command_seq",
        "last_stop_seq",
        "applied_event_seq",
        "transcript_cursor",
    ] {
        assert!(
            sqlx::query(AssertSqlSafe(format!(
                "UPDATE rpi_sessions SET {column} = -1 WHERE conversation_id = 'rpi'"
            )))
            .execute(pool)
            .await
            .is_err(),
            "{column} must be nonnegative"
        );
        sqlx::query(AssertSqlSafe(format!(
            "UPDATE rpi_sessions SET {column} = 4294967297 WHERE conversation_id = 'rpi'"
        )))
        .execute(pool)
        .await
        .expect("Rpi counters are 64-bit");
    }
    assert!(
        sqlx::query(
            "INSERT INTO rpi_sessions (conversation_id, transcript_json) VALUES ('absent', '{}')"
        )
        .execute(pool)
        .await
        .is_err(),
        "Rpi sessions reference a real shared conversation"
    );
    insert_command(pool, "request", 1)
        .await
        .expect("a pending command");
    let state: (String, Option<String>) = sqlx::query_as(
        "SELECT delivery_state, result_json FROM rpi_commands WHERE conversation_id = 'rpi'",
    )
    .fetch_one(pool)
    .await
    .expect("command defaults");
    assert_eq!(state, ("pending".to_owned(), None));
    assert!(insert_command(pool, "request", 2).await.is_err());
    assert!(insert_command(pool, "other-request", 1).await.is_err());
    assert!(insert_command(pool, "negative", -1).await.is_err());
    for state in ["accepted", "settled"] {
        sqlx::query("UPDATE rpi_commands SET delivery_state = $1, result_json = '{}' WHERE conversation_id = 'rpi'")
            .bind(state)
            .execute(pool)
            .await
            .expect("valid durable command states");
    }
    for statement in [
        "UPDATE rpi_commands SET delivery_state = 'completed' WHERE conversation_id = 'rpi'",
        "UPDATE rpi_commands SET kind = 'arbitrary_rpc' WHERE conversation_id = 'rpi'",
        "UPDATE rpi_commands SET conversation_id = 'absent' WHERE conversation_id = 'rpi'",
    ] {
        assert!(sqlx::query(statement).execute(pool).await.is_err());
    }
    assert_creation_receipt_survives_delete(pool).await;
}

async fn insert_command(pool: &AnyPool, request: &str, sequence: i64) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO rpi_commands (conversation_id, request_id, command_seq, kind, payload_json, \
         payload_fingerprint, created_ms, updated_ms) VALUES ('rpi', $1, $2, 'open', '{}', 'semantic', 1, 1)",
    )
    .bind(request)
    .bind(sequence)
    .execute(pool)
    .await?;
    Ok(())
}

async fn assert_creation_receipt_survives_delete(pool: &AnyPool) {
    let receipt = "INSERT INTO rpi_creations \
        (account_id, request_id, conversation_id, original_conversation_id, payload_fingerprint, \
         conversation_json, created_ms, updated_ms) \
        VALUES ($1, 'create-request', 'rpi', 'rpi', 'semantic-create', '{\"id\":\"rpi\"}', 1, 1)";
    sqlx::query(receipt)
        .bind("account")
        .execute(pool)
        .await
        .expect("a durable account-scoped creation receipt");
    assert!(
        sqlx::query(receipt)
            .bind("account")
            .execute(pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query(receipt)
            .bind("absent")
            .execute(pool)
            .await
            .is_err()
    );
    sqlx::query("INSERT INTO accounts (id, email_normalized, status, created_at_ms) VALUES ('other-account', 'other@example.invalid', 'active', 1)")
        .execute(pool)
        .await
        .expect("another authenticated account");
    sqlx::query(receipt)
        .bind("other-account")
        .execute(pool)
        .await
        .expect("request IDs are scoped to an account, not globally");
    let mut transaction = pool.begin().await.expect("a deletion transaction");
    sqlx::query("UPDATE rpi_creations SET deleted_ms = 99, updated_ms = 99 WHERE original_conversation_id = 'rpi'")
        .execute(&mut *transaction)
        .await
        .expect("retain the deleted outcome before removing the projection");
    sqlx::query("DELETE FROM agent_conversations WHERE id = 'rpi'")
        .execute(&mut *transaction)
        .await
        .expect("delete an acknowledged Rpi projection");
    transaction.commit().await.expect("commit deletion");
    let receipts: Vec<(Option<String>, String, String, String, i64)> = sqlx::query_as(
        "SELECT conversation_id, original_conversation_id, payload_fingerprint, conversation_json, deleted_ms \
         FROM rpi_creations ORDER BY account_id",
    )
    .fetch_all(pool)
    .await
    .expect("creation identities outlive the projection");
    assert_eq!(receipts.len(), 2);
    for receipt in receipts {
        assert_eq!(
            receipt,
            (
                None,
                "rpi".to_owned(),
                "semantic-create".to_owned(),
                "{\"id\":\"rpi\"}".to_owned(),
                99
            )
        );
    }
    for table in ["rpi_sessions", "rpi_commands"] {
        let count: i64 = sqlx::query_scalar(AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
            .fetch_one(pool)
            .await
            .expect("Rpi child row cascade");
        assert_eq!(count, 0, "{table} must cascade with its parent");
    }
    assert!(
        sqlx::query(receipt)
            .bind("account")
            .execute(pool)
            .await
            .is_err()
    );
}
