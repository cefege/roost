//! The `push_subscriptions` rows: the capped upsert, the removal, and the two
//! reads dispatch needs.
//!
//! Owned by the push domain. Every statement here binds its values; none
//! interpolates a caller's endpoint into SQL text.
//!
//! THE CAP IS ONE STATEMENT, AND IT MUST STAY ONE. v2's
//! `handlers-push.ts:88-109` does not count and then insert -- it puts the
//! count, the existence check and the write in the same `INSERT ... SELECT ...
//! WHERE`, so the cap and the row are one decision. A count-then-insert is two
//! decisions with a window between them, and a device that opens two tabs at
//! once takes that window every time. A test asserts the shape by racing two
//! subscribes; see `tests/push_subscription_rpc.rs`.

use sqlx::{Row, SqlitePool};

use crate::push::endpoint_policy::PushInputError;

/// How many distinct endpoints one account device may hold.
///
/// 4 (`handlers-push.ts:23`). A browser re-subscribes on a new profile and
/// leaves the old endpoint behind, so the number is a bound on accumulated
/// dead endpoints rather than on tabs; four is enough for a device that has
/// genuinely been re-provisioned twice.
pub const MAX_SUBSCRIPTIONS_PER_DEVICE: usize = 4;

/// One stored subscription, as dispatch reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSubscription {
    /// The dashboard the row is scoped to.
    pub dashboard_id: String,
    /// The browser device fingerprint the subscription belongs to.
    pub viewer_fp: String,
    /// The push service endpoint.
    pub endpoint: String,
    /// The base64url P-256 public key.
    pub p256dh: String,
    /// The base64url auth secret.
    pub auth: String,
    /// When the row was last written, in epoch milliseconds.
    pub created_at_ms: i64,
}

/// The upsert, cap and all, as one statement.
///
/// The `WHERE` is the whole policy: re-subscribing an endpoint this device
/// already holds is always admitted (that is the `EXISTS` arm, and it is what
/// makes a key rotation work at the cap), and a NEW endpoint is admitted only
/// while the device is below [`MAX_SUBSCRIPTIONS_PER_DEVICE`]. The `ON CONFLICT`
/// arm then refreshes the keys, so a second subscribe from the same browser
/// updates rather than duplicating.
const UPSERT_SUBSCRIPTION: &str = "\
    INSERT INTO push_subscriptions (
      dashboard_id, viewer_fp, endpoint, p256dh, auth, created_at_ms
    )
    SELECT ?1, ?2, ?3, ?4, ?5, ?6
    WHERE EXISTS (
      SELECT 1 FROM push_subscriptions
      WHERE viewer_fp = ?2 AND endpoint = ?3
    ) OR (
      SELECT COUNT(*) FROM push_subscriptions
      WHERE viewer_fp = ?2
    ) < ?7
    ON CONFLICT (dashboard_id, viewer_fp, endpoint) DO UPDATE SET
      p256dh = excluded.p256dh,
      auth = excluded.auth,
      created_at_ms = excluded.created_at_ms";

/// Store or refresh one subscription, refusing a device that is at the cap.
///
/// `ResourceExhausted` is the answer, not `InvalidArgument`: nothing about the
/// request was malformed, and a browser that sees 429 retries later, which is
/// the behaviour that lets a device recover after the operator raises the cap
/// or after the device drops a stale endpoint.
pub async fn store_subscription(
    pool: &SqlitePool,
    dashboard_id: &str,
    viewer_fp: &str,
    endpoint: &str,
    p256dh: &str,
    auth: &str,
    now_ms: i64,
) -> Result<(), PushInputError> {
    let written = sqlx::query(UPSERT_SUBSCRIPTION)
        .bind(dashboard_id)
        .bind(viewer_fp)
        .bind(endpoint)
        .bind(p256dh)
        .bind(auth)
        .bind(now_ms)
        .bind(i64::try_from(MAX_SUBSCRIPTIONS_PER_DEVICE).unwrap_or(i64::MAX))
        .execute(pool)
        .await
        .map_err(|error| PushInputError::Store(error.to_string()))?;
    if written.rows_affected() == 1 {
        Ok(())
    } else {
        Err(PushInputError::DeviceCapReached)
    }
}

/// Remove one endpoint for one device.
///
/// Silently succeeds when there was nothing to remove: an unsubscribe for an
/// endpoint this device never held is the state the caller asked for.
pub async fn remove_subscription(
    pool: &SqlitePool,
    viewer_fp: &str,
    endpoint: &str,
) -> Result<(), PushInputError> {
    sqlx::query("DELETE FROM push_subscriptions WHERE viewer_fp = ?1 AND endpoint = ?2")
        .bind(viewer_fp)
        .bind(endpoint)
        .execute(pool)
        .await
        .map_err(|error| PushInputError::Store(error.to_string()))?;
    Ok(())
}

/// Delete rows whose device no longer exists, then read the deliverable set.
///
/// The delete and the read are two statements, in this order, and the order is
/// the point: `account_devices` is the live browser-device registry, so the
/// foreign key already prunes a normally-revoked device. This delete repairs
/// the rows a legacy cleanup path left behind, where only the
/// `account_devices` association was removed. Running it first means the read
/// below cannot hand this dispatch a subscription for a device that is gone.
///
/// The read joins through `account_devices` to `accounts` and requires
/// `status = 'active'`: a disabled account KEEPS its rows -- so an operator who
/// re-enables it does not have to re-pair every browser -- but receives nothing
/// while it is disabled.
pub async fn take_deliverable_subscriptions(
    pool: &SqlitePool,
) -> Result<Vec<StoredSubscription>, PushInputError> {
    prune_orphaned_subscriptions(pool).await?;
    let rows = sqlx::query(
        "SELECT subscription.dashboard_id AS dashboard_id, \
                subscription.viewer_fp AS viewer_fp, \
                subscription.endpoint AS endpoint, \
                subscription.p256dh AS p256dh, \
                subscription.auth AS auth, \
                subscription.created_at_ms AS created_at_ms \
         FROM push_subscriptions AS subscription \
         INNER JOIN account_devices AS device \
           ON device.fingerprint = subscription.viewer_fp \
         INNER JOIN accounts AS account ON account.id = device.account_id \
         WHERE account.status = 'active'",
    )
    .fetch_all(pool)
    .await
    .map_err(|error| PushInputError::Store(error.to_string()))?;
    rows.into_iter()
        .map(|row| {
            Ok(StoredSubscription {
                dashboard_id: row.try_get("dashboard_id").map_err(store_error)?,
                viewer_fp: row.try_get("viewer_fp").map_err(store_error)?,
                endpoint: row.try_get("endpoint").map_err(store_error)?,
                p256dh: row.try_get("p256dh").map_err(store_error)?,
                auth: row.try_get("auth").map_err(store_error)?,
                created_at_ms: row.try_get("created_at_ms").map_err(store_error)?,
            })
        })
        .collect()
}

/// A row read that failed, in the vocabulary the callers already handle.
fn store_error(error: sqlx::Error) -> PushInputError {
    PushInputError::Store(error.to_string())
}

/// Delete every subscription whose fingerprint is no longer an account device.
async fn prune_orphaned_subscriptions(pool: &SqlitePool) -> Result<(), PushInputError> {
    sqlx::query(
        "DELETE FROM push_subscriptions \
         WHERE NOT EXISTS ( \
           SELECT 1 FROM account_devices \
           WHERE account_devices.fingerprint = push_subscriptions.viewer_fp \
         )",
    )
    .execute(pool)
    .await
    .map_err(|error| PushInputError::Store(error.to_string()))?;
    Ok(())
}

/// Count the endpoints one device currently holds. Test and diagnostic use.
pub async fn subscription_count(pool: &SqlitePool, viewer_fp: &str) -> Result<i64, PushInputError> {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM push_subscriptions WHERE viewer_fp = ?1")
        .bind(viewer_fp)
        .fetch_one(pool)
        .await
        .map_err(|error| PushInputError::Store(error.to_string()))
}
