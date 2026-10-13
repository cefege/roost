//! Persistence of coordinator liveness and the internal address peers dial.
//!
//! Each replica refreshes its own row on the election cadence and removes rows
//! whose heartbeat is too old to route traffic safely.

use crate::cluster::ownership::{INSTANCE_STALE_MS, OwnershipState};
use crate::db::CoordDb;

/// Upsert this replica's heartbeat and remove stale address records.
pub async fn write(
    db: &CoordDb,
    instance_id: &str,
    internal_url: &str,
    now_ms: i64,
    state: &OwnershipState,
) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO coord_instances (instance_id,internal_url,heartbeat_ms) VALUES ($1,$2,$3) ON CONFLICT(instance_id) DO UPDATE SET internal_url=excluded.internal_url,heartbeat_ms=excluded.heartbeat_ms")
        .bind(instance_id).bind(internal_url).bind(now_ms).execute(db.pool()).await?;
    sqlx::query("DELETE FROM coord_instances WHERE heartbeat_ms < $1")
        .bind(now_ms.saturating_sub(INSTANCE_STALE_MS))
        .execute(db.pool())
        .await?;
    state.set_heartbeat(now_ms);
    Ok(())
}
