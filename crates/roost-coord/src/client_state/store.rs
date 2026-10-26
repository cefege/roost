//! SQL for the account-scoped `client_state` table. The batch write runs in one
//! serialized transaction, so the keys-per-account decision and the upserts are
//! one unit on both backends.

use std::collections::BTreeSet;

use roost_protocol::client_state::MAX_KEYS_PER_ACCOUNT;

use crate::db::CoordDb;

use super::ClientStateError;

/// One stored row, which is also an incoming write once it has passed validation.
#[derive(Debug, Clone, sqlx::FromRow)]
pub(super) struct ClientStateRow {
    pub key: String,
    pub value: String,
    pub updated_ms: i64,
}

/// A tie goes to the incoming write (`>=`); an older write leaves the row as it is.
const UPSERT_SQL: &str = "INSERT INTO client_state (account_id, key, value, updated_ms) \
     VALUES ($1, $2, $3, $4) \
     ON CONFLICT(account_id, key) DO UPDATE SET value = excluded.value, updated_ms = excluded.updated_ms \
     WHERE excluded.updated_ms >= client_state.updated_ms";

pub(super) async fn list_rows(
    database: &CoordDb,
    account_id: &str,
) -> Result<Vec<ClientStateRow>, sqlx::Error> {
    sqlx::query_as::<_, ClientStateRow>(
        "SELECT key, value, updated_ms FROM client_state WHERE account_id = $1 ORDER BY key",
    )
    .bind(account_id)
    .fetch_all(database.pool())
    .await
}

/// Applies a validated batch all-or-nothing, then returns the stored row for every
/// requested key, so a write that lost to a newer one reports the winner.
pub(super) async fn apply_batch(
    database: &CoordDb,
    account_id: &str,
    batch: &[ClientStateRow],
) -> Result<Vec<ClientStateRow>, ClientStateError> {
    let mut transaction = database.begin_write().await?;
    let stored_keys: BTreeSet<String> =
        sqlx::query_scalar::<_, String>("SELECT key FROM client_state WHERE account_id = $1")
            .bind(account_id)
            .fetch_all(&mut *transaction)
            .await?
            .into_iter()
            .collect();
    let batch_keys: BTreeSet<&str> = batch.iter().map(|row| row.key.as_str()).collect();
    let new_keys = batch_keys
        .iter()
        .filter(|key| !stored_keys.contains(**key))
        .count();
    if stored_keys.len() + new_keys > MAX_KEYS_PER_ACCOUNT {
        return Err(ClientStateError::KeyCapReached {
            limit: MAX_KEYS_PER_ACCOUNT,
        });
    }
    for row in batch {
        sqlx::query(UPSERT_SQL)
            .bind(account_id)
            .bind(row.key.as_str())
            .bind(row.value.as_str())
            .bind(row.updated_ms)
            .execute(&mut *transaction)
            .await?;
    }
    let mut winners = Vec::with_capacity(batch.len());
    for row in batch {
        let winner = sqlx::query_as::<_, ClientStateRow>(
            "SELECT key, value, updated_ms FROM client_state WHERE account_id = $1 AND key = $2",
        )
        .bind(account_id)
        .bind(row.key.as_str())
        .fetch_one(&mut *transaction)
        .await?;
        winners.push(winner);
    }
    transaction.commit().await?;
    Ok(winners)
}
