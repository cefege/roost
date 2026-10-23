//! Applies pending schema migrations as one committed transition.
//! CoordDb calls this after history validation and its pre-migration backup.
//! The migration lock covers the commit so replicas cannot race the history.

use sqlx::migrate::{Migrate, MigrateError, Migrator};
use sqlx::{AnyPool, Connection};

use super::DbError;

pub(super) async fn apply_pending_migrations(
    pool: &AnyPool,
    migrator: &mut Migrator,
) -> Result<(), DbError> {
    migrator.set_locking(false);
    let mut connection = pool.acquire().await?;
    connection.lock().await.map_err(migration_error)?;
    let result = async {
        let mut transaction = connection.begin().await?;
        // SQLx's Acquire wrapper makes this startup future insufficiently Send.
        migrator
            .run_direct(None, &mut *transaction, false)
            .await
            .map_err(migration_error)?;
        transaction.commit().await?;
        Ok(())
    }
    .await;
    let unlocked = connection.unlock().await.map_err(migration_error);
    result.and(unlocked)
}

fn migration_error(error: MigrateError) -> DbError {
    DbError::Migration {
        name: "embedded".to_owned(),
        reason: error.to_string(),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::apply_pending_migrations;
    use sqlx::SqlSafeStr;
    use sqlx::any::AnyPoolOptions;
    use sqlx::migrate::{Migration, MigrationType, Migrator};

    fn migration(version: i64, statement: &'static str) -> Migration {
        Migration::new(
            version,
            format!("step {version}").into(),
            MigrationType::Simple,
            statement.into_sql_str(),
            false,
        )
    }

    #[tokio::test]
    async fn failed_upgrade_leaves_no_partial_schema_and_can_retry() {
        sqlx::any::install_default_drivers();
        let pool = AnyPoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("database");
        let first = migration(1, "CREATE TABLE upgrade_probe(value TEXT NOT NULL)");
        let mut broken = Migrator::with_migrations(vec![
            first.clone(),
            migration(2, "INSERT INTO absent_table VALUES ('failed')"),
        ]);
        assert!(apply_pending_migrations(&pool, &mut broken).await.is_err());
        let partial: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('upgrade_probe', '_sqlx_migrations')",
        )
        .fetch_one(&pool)
        .await
        .expect("schema after failed upgrade");
        assert_eq!(partial, 0);
        let mut repaired = Migrator::with_migrations(vec![
            first,
            migration(2, "INSERT INTO upgrade_probe VALUES ('completed')"),
        ]);
        apply_pending_migrations(&pool, &mut repaired)
            .await
            .expect("retry upgrade");
        let value: String = sqlx::query_scalar("SELECT value FROM upgrade_probe")
            .fetch_one(&pool)
            .await
            .expect("completed upgrade");
        assert_eq!(value, "completed");
        pool.close().await;
    }
}
