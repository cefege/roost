//! The statements and refusals the auth-device surface runs on.
//!
//! Owned by this file because three modules in the auth layer write the same
//! six tables and read the same three, and a bind list, a transaction opener
//! and an `Internal` refusal that live in two places drift apart in exactly the
//! way that is invisible until a placeholder count is wrong. `rpc_bootstrap`
//! and `rpc_devices` both ask this file; this file asks neither of them, so the
//! dependency runs downward.
//!
//! A DATABASE MESSAGE NEVER REACHES A CALLER. A sqlx error names a table, a
//! column and a constraint, and it is a fact about this deployment's schema
//! rather than about the request. [`internal`] puts the detail in the log under
//! the step that produced it and hands the client a fixed sentence, so an
//! operator loses nothing and a peer learns only that the coordinator is
//! unhappy.

use connectrpc::{ConnectError, ErrorCode};
use sqlx::{Any, AssertSqlSafe, Transaction};

use crate::db::CoordDb;

/// The lookups this surface asks most, named once.
pub(crate) const AUTHORIZED_KEY: &str = "SELECT 1 FROM authorized_keys WHERE fingerprint = $1";
pub(crate) const WORKER_ROW: &str = "SELECT 1 FROM workers WHERE fp = $1";
pub(crate) const REVOKED_KEY: &str =
    "SELECT 1 FROM authorized_key_revocations WHERE fingerprint = $1";
pub(crate) const ACCOUNT_DEVICE_ROW: &str = "SELECT 1 FROM account_devices WHERE fingerprint = $1";

/// One bound parameter, so a caller can say what a column actually holds
/// instead of rendering an integer into a string and hoping SQLite forgives it.
pub(crate) enum Bind<'a> {
    /// A text column, or SQL `NULL`.
    Text(Option<&'a str>),
    /// A 32-byte Ed25519 public key.
    Bytes(&'a [u8]),
    /// An epoch-millisecond or row-count column.
    Int(i64),
}

/// The refusal for a statement that failed, and the step it failed at.
pub(crate) fn internal(step: &'static str, error: &sqlx::Error) -> ConnectError {
    fault(step, error)
}

/// The refusal for anything this coordinator got wrong, whatever the source
/// type of the detail. `step` names the place and the detail goes to the log
/// under it, so a table and a constraint name never reaches a browser.
pub(crate) fn fault(step: &'static str, detail: &impl std::fmt::Display) -> ConnectError {
    tracing::error!(step, error = %detail, "auth.statement_failed");
    ConnectError::new(
        ErrorCode::Internal,
        "the coordinator could not complete this request",
    )
}

pub(crate) async fn begin(database: &CoordDb) -> Result<Transaction<'_, Any>, ConnectError> {
    database
        .pool()
        .begin()
        .await
        .map_err(|error| internal("begin", &error))
}

pub(crate) async fn commit(transaction: Transaction<'_, Any>) -> Result<(), ConnectError> {
    transaction
        .commit()
        .await
        .map_err(|error| internal("commit", &error))
}

/// Run a statement with typed binds.
pub(crate) async fn run(
    transaction: &mut Transaction<'_, Any>,
    sql: &str,
    binds: &[Bind<'_>],
) -> Result<(), ConnectError> {
    let mut query = sqlx::query(AssertSqlSafe(sql));
    for bind in binds {
        query = match bind {
            Bind::Text(value) => query.bind(value),
            Bind::Bytes(value) => query.bind(*value),
            Bind::Int(value) => query.bind(*value),
        };
    }
    // The row count is discarded here on purpose: nothing in this crate reads
    // it, and a helper whose return type is whatever its caller happens to want
    // is a helper with two contracts.
    query
        .execute(&mut **transaction)
        .await
        .map(|_| ())
        .map_err(|error| internal("run", &error))
}

/// Whether a one-parameter lookup found a row.
pub(crate) async fn exists1(
    transaction: &mut Transaction<'_, Any>,
    sql: &str,
    value: &str,
) -> Result<bool, ConnectError> {
    sqlx::query_scalar::<_, i64>(AssertSqlSafe(sql))
        .bind(value)
        .fetch_optional(&mut **transaction)
        .await
        .map(|found| found.is_some())
        .map_err(|error| internal("exists1", &error))
}

/// Whether a two-parameter lookup found a row, for the account-scoped questions
/// one arity cannot answer.
pub(crate) async fn exists2(
    transaction: &mut Transaction<'_, Any>,
    sql: &str,
    values: (&str, &str),
) -> Result<bool, ConnectError> {
    sqlx::query_scalar::<_, i64>(AssertSqlSafe(sql))
        .bind(values.0)
        .bind(values.1)
        .fetch_optional(&mut **transaction)
        .await
        .map(|found| found.is_some())
        .map_err(|error| internal("exists2", &error))
}

/// The one text column a lookup named.
pub(crate) async fn column1(
    transaction: &mut Transaction<'_, Any>,
    sql: &str,
    value: &str,
) -> Result<Option<String>, ConnectError> {
    sqlx::query_as::<_, (String,)>(AssertSqlSafe(sql))
        .bind(value)
        .fetch_optional(&mut **transaction)
        .await
        .map(|found| found.map(|(text,)| text))
        .map_err(|error| internal("column1", &error))
}

/// The one text column a two-parameter lookup named.
pub(crate) async fn column2(
    transaction: &mut Transaction<'_, Any>,
    sql: &str,
    values: (&str, &str),
) -> Result<Option<String>, ConnectError> {
    sqlx::query_as::<_, (String,)>(AssertSqlSafe(sql))
        .bind(values.0)
        .bind(values.1)
        .fetch_optional(&mut **transaction)
        .await
        .map(|found| found.map(|(text,)| text))
        .map_err(|error| internal("column2", &error))
}

/// The stored public key bytes, compared byte-for-byte on a retry: the same
/// fingerprint with a different key is a second principal wearing its name.
pub(crate) async fn stored_public_key(
    transaction: &mut Transaction<'_, Any>,
    fingerprint: &str,
) -> Result<Option<Vec<u8>>, ConnectError> {
    sqlx::query_as::<_, (Vec<u8>,)>("SELECT public_key FROM authorized_keys WHERE fingerprint = $1")
        .bind(fingerprint)
        .fetch_optional(&mut **transaction)
        .await
        .map(|found| found.map(|(bytes,)| bytes))
        .map_err(|error| internal("stored_public_key", &error))
}

/// Add a key to `authorized_keys`, shared by a redemption and a rotation.
pub(crate) async fn insert_authorized_key(
    transaction: &mut Transaction<'_, Any>,
    fingerprint: &str,
    public_key: &[u8; 32],
    label: &str,
    now: i64,
) -> Result<(), ConnectError> {
    run(
        transaction,
        "INSERT INTO authorized_keys (fingerprint, public_key, label, added_at) \
         VALUES ($1, $2, $3, $4)",
        &[
            Bind::Text(Some(fingerprint)),
            Bind::Bytes(public_key),
            Bind::Text(Some(label)),
            Bind::Int(now),
        ],
    )
    .await
}

/// Associate a key with an account. A key with no `account_devices` row resolves
/// to `LegacySelfHosted`, which the Sync upgrade refuses.
pub(crate) async fn insert_account_device(
    transaction: &mut Transaction<'_, Any>,
    fingerprint: &str,
    account_id: &str,
    now: i64,
) -> Result<(), ConnectError> {
    run(
        transaction,
        "INSERT INTO account_devices (fingerprint, account_id, added_at_ms, last_seen_at_ms) \
         VALUES ($1, $2, $3, $4)",
        &[
            Bind::Text(Some(fingerprint)),
            Bind::Text(Some(account_id)),
            Bind::Int(now),
            Bind::Int(now),
        ],
    )
    .await
}

pub(crate) fn invalid_argument(message: &str) -> ConnectError {
    ConnectError::new(ErrorCode::InvalidArgument, message)
}
