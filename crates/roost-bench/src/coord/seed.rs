//! Enroll the harness's `BenchDevice` in a coordinator's database: adopt the
//! one self-hosted account the coordinator created at boot and attach the key
//! to it. The same two inserts the deleted smoke harness ran against both
//! coordinators (`main:smoke/terminal/stack-runtime.ts`); the tables are in
//! `crates/roost-coord/migrations/0001_init.sql`.

use std::path::Path;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use crate::coord::jwt::{BenchDevice, now_ms};
use crate::error::BenchError;

pub async fn seed_device(db_path: &Path, device: &BenchDevice) -> Result<(), BenchError> {
    let failure = |detail: String| BenchError::Seed {
        path: db_path.to_path_buf(),
        detail,
    };
    // The coordinator is live on this file and takes write locks of its own.
    let options = SqliteConnectOptions::new()
        .filename(db_path)
        .busy_timeout(Duration::from_secs(10));
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .map_err(|error| failure(format!("open: {error}")))?;
    let seeded = async {
        let mut transaction = pool
            .begin()
            .await
            .map_err(|error| failure(format!("begin: {error}")))?;
        let accounts: Vec<String> = sqlx::query_scalar("SELECT id FROM accounts")
            .fetch_all(&mut *transaction)
            .await
            .map_err(|error| failure(format!("read accounts: {error}")))?;
        let [account_id] = accounts.as_slice() else {
            return Err(failure(format!(
                "expected one self-hosted account, found {}",
                accounts.len()
            )));
        };
        let now = now_ms();
        sqlx::query(
            "INSERT INTO authorized_keys (fingerprint, public_key, label, added_at) \
             VALUES (?, ?, 'roost-bench', ?)",
        )
        .bind(&device.fingerprint)
        .bind(device.public_key.as_slice())
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(|error| failure(format!("insert authorized_keys: {error}")))?;
        sqlx::query(
            "INSERT INTO account_devices (fingerprint, account_id, added_at_ms, last_seen_at_ms) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(&device.fingerprint)
        .bind(account_id)
        .bind(now)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(|error| failure(format!("insert account_devices: {error}")))?;
        transaction
            .commit()
            .await
            .map_err(|error| failure(format!("commit: {error}")))
    }
    .await;
    pool.close().await;
    seeded
}
