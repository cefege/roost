//! Claim and fence the replica that polls a device-code login.
//! Lease rows share the coordinator's durable coordination table.

use roost_agent::admission::{LeaseRow, lease_expiry, next_lease_epoch};

use crate::db::CoordDb;

use super::{LoginError, login_storage, now_ms};

const POLLER_LEASE_TTL_MS: u64 = 30_000;

pub(super) async fn claim_poller_lease(
    db: &CoordDb,
    instance_id: &str,
    login_id: &str,
) -> Result<Option<u64>, LoginError> {
    let lease_name = login_lease_name(login_id);
    let now = now_ms();
    let mut transaction = db.begin_write().await.map_err(login_storage)?;
    let stored: Option<(String, i64, i64)> =
        sqlx::query_as("SELECT owner_instance,epoch,expires_ms FROM coord_leases WHERE name=$1")
            .bind(&lease_name)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(login_storage)?;
    let current = stored.map(|(owner, epoch, expires_ms)| LeaseRow {
        owner,
        epoch: u64::try_from(epoch).unwrap_or(u64::MAX),
        expires_ms,
    });
    let Some(epoch) = next_lease_epoch(current.as_ref(), instance_id, now) else {
        transaction.rollback().await.map_err(login_storage)?;
        return Ok(None);
    };
    sqlx::query("INSERT INTO coord_leases(name,owner_instance,epoch,expires_ms) VALUES($1,$2,$3,$4) ON CONFLICT(name) DO UPDATE SET owner_instance=excluded.owner_instance,epoch=excluded.epoch,expires_ms=excluded.expires_ms")
        .bind(lease_name)
        .bind(instance_id)
        .bind(i64::try_from(epoch).unwrap_or(i64::MAX))
        .bind(lease_expiry(now, POLLER_LEASE_TTL_MS))
        .execute(&mut *transaction)
        .await
        .map_err(login_storage)?;
    transaction.commit().await.map_err(login_storage)?;
    Ok(Some(epoch))
}

pub(super) async fn renew_poller_lease(
    db: &CoordDb,
    instance_id: &str,
    login_id: &str,
    epoch: u64,
) -> Result<bool, LoginError> {
    let now = now_ms();
    let result = sqlx::query("UPDATE coord_leases SET expires_ms=$1 WHERE name=$2 AND owner_instance=$3 AND epoch=$4 AND expires_ms>$5")
        .bind(lease_expiry(now, POLLER_LEASE_TTL_MS))
        .bind(login_lease_name(login_id))
        .bind(instance_id)
        .bind(i64::try_from(epoch).unwrap_or(i64::MAX))
        .bind(now)
        .execute(db.pool())
        .await
        .map_err(login_storage)?;
    Ok(result.rows_affected() == 1)
}

pub(super) async fn release_poller_lease(
    db: &CoordDb,
    instance_id: &str,
    login_id: &str,
    epoch: u64,
) -> Result<(), LoginError> {
    sqlx::query("DELETE FROM coord_leases WHERE name=$1 AND owner_instance=$2 AND epoch=$3")
        .bind(login_lease_name(login_id))
        .bind(instance_id)
        .bind(i64::try_from(epoch).unwrap_or(i64::MAX))
        .execute(db.pool())
        .await
        .map_err(login_storage)?;
    Ok(())
}

pub(super) fn login_lease_name(login_id: &str) -> String {
    format!("login:{login_id}")
}
