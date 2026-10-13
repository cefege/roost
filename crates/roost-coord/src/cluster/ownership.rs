//! The single link owner for a coordinator cluster.
//!
//! A Postgres lease elects the replica that owns worker and browser sockets;
//! followers splice their public listener to its internal address. SQLite has
//! one process and therefore starts as owner without a lease election.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use roost_agent::admission::{LeaseRow, lease_expiry, next_lease_epoch};
use tokio::sync::watch;

use crate::cluster::notify::{CLUSTER_CHANNEL, ClusterNotifier};
use crate::db::{CoordDb, DbBackend};

/// The link-owner lease name shared by every replica.
pub const LINK_LEASE_NAME: &str = "cluster:links";
/// The lease is renewed frequently relative to its lifetime.
pub const LINK_LEASE_TTL_MS: u64 = 10_000;
/// Election and heartbeat cadence.
pub const CLUSTER_TICK: Duration = Duration::from_secs(2);
/// Stale instance heartbeat threshold.
pub const INSTANCE_STALE_MS: i64 = 60_000;
/// Probe freshness threshold for this instance.
pub const PROBE_HEARTBEAT_FRESH_MS: i64 = 10_000;

/// Live cluster status used by ownership, transport and probe routes.
#[derive(Debug, Default)]
pub struct OwnershipState {
    owner: AtomicBool,
    heartbeat_ms: AtomicI64,
    owner_internal_url: Mutex<Option<String>>,
}

impl OwnershipState {
    /// Whether this process currently holds the links lease.
    #[must_use]
    pub fn is_owner(&self) -> bool {
        self.owner.load(Ordering::Acquire)
    }

    /// Whether this process has a recent persisted heartbeat.
    #[must_use]
    pub fn heartbeat_fresh(&self, now_ms: i64) -> bool {
        let heartbeat_ms = self.heartbeat_ms.load(Ordering::Acquire);
        heartbeat_ms > 0 && now_ms.saturating_sub(heartbeat_ms) <= PROBE_HEARTBEAT_FRESH_MS
    }

    /// The current owner's HTTP internal URL, if the row is known.
    #[must_use]
    pub fn owner_internal_url(&self) -> Option<String> {
        self.owner_internal_url
            .lock()
            .ok()
            .and_then(|url| url.clone())
    }

    fn set_owner(&self, owner: bool) {
        self.owner.store(owner, Ordering::Release);
    }

    pub(crate) fn set_heartbeat(&self, heartbeat_ms: i64) {
        self.heartbeat_ms.store(heartbeat_ms, Ordering::Release);
    }

    /// Set a peer address for transport tests that do not run a database loop.
    #[doc(hidden)]
    pub fn _set_owner_url_for_tests(&self, url: Option<String>) {
        self.set_owner_url(url);
    }

    fn set_owner_url(&self, url: Option<String>) {
        if let Ok(mut current) = self.owner_internal_url.lock() {
            *current = url;
        }
    }
}

/// Runs heartbeat and lease maintenance until shutdown or ownership loss.
pub async fn run(
    db: CoordDb,
    instance_id: String,
    internal_url: String,
    notifier: Arc<ClusterNotifier>,
    state: Arc<OwnershipState>,
    agent: Arc<crate::agent::AgentService>,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let mut notices = notifier.subscribe();
    let mut interval = tokio::time::interval(CLUSTER_TICK);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    if db.backend() == DbBackend::Sqlite {
        state.set_owner(true);
        agent.join_cluster();
        agent.recover().await;
        tracing::info!(instance_id, "coordinator link ownership acquired");
    }

    let mut last_renewed = crate::serve::now_ms();
    loop {
        tokio::select! {
            _ = interval.tick() => {},
            _ = notices.recv() => {},
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    release_for_shutdown(&db, &instance_id, &state, &notifier).await?;
                    return Ok(());
                }
            }
        }
        let now = crate::serve::now_ms();
        match tick(&db, &instance_id, &internal_url, &state, now).await {
            Ok(Tick::Renewed) => last_renewed = now,
            Ok(Tick::Acquired) => {
                last_renewed = now;
                tracing::info!(instance_id, "coordinator link ownership acquired");
                agent.join_cluster();
                agent.recover().await;
            }
            Ok(Tick::Following) => {}
            Ok(Tick::Lost) => {
                state.set_owner(false);
                tracing::error!(instance_id, "coordinator link-owner lease lost");
                anyhow::bail!("coordinator link-owner lease lost for {instance_id}");
            }
            Err(error) => {
                tracing::warn!(instance_id, %error, "coordinator cluster tick failed");
                let deadline = last_renewed
                    .saturating_add(i64::try_from(LINK_LEASE_TTL_MS).unwrap_or(i64::MAX));
                if state.is_owner() && now >= deadline {
                    state.set_owner(false);
                    anyhow::bail!(
                        "coordinator link-owner lease could not be renewed before it expired: {error}"
                    );
                }
            }
        }
    }
}

/// What one cluster tick did.
enum Tick {
    Renewed,
    Acquired,
    Following,
    Lost,
}

async fn tick(
    db: &CoordDb,
    instance_id: &str,
    internal_url: &str,
    state: &OwnershipState,
    now: i64,
) -> anyhow::Result<Tick> {
    crate::cluster::heartbeat::write(db, instance_id, internal_url, now, state).await?;
    state.set_owner_url(read_owner_url(db, now).await?);
    if db.backend() == DbBackend::Sqlite {
        return Ok(Tick::Renewed);
    }
    if state.is_owner() {
        return Ok(if renew(db, instance_id, now).await? {
            Tick::Renewed
        } else {
            Tick::Lost
        });
    }
    if acquire(db, instance_id, now).await? {
        state.set_owner(true);
        return Ok(Tick::Acquired);
    }
    Ok(Tick::Following)
}

async fn acquire(db: &CoordDb, instance_id: &str, now: i64) -> anyhow::Result<bool> {
    let mut tx = db.begin_write().await?;
    let row = sqlx::query_as::<_, (String, i64, i64)>(
        "SELECT owner_instance,epoch,expires_ms FROM coord_leases WHERE name=$1",
    )
    .bind(LINK_LEASE_NAME)
    .fetch_optional(&mut *tx)
    .await?;
    let current = row.map(|(owner, epoch, expires_ms)| LeaseRow {
        owner,
        epoch: u64::try_from(epoch).unwrap_or(0),
        expires_ms,
    });
    let Some(epoch) = next_lease_epoch(current.as_ref(), instance_id, now) else {
        tx.rollback().await?;
        return Ok(false);
    };
    sqlx::query("INSERT INTO coord_leases (name,owner_instance,epoch,expires_ms) VALUES ($1,$2,$3,$4) ON CONFLICT(name) DO UPDATE SET owner_instance=excluded.owner_instance,epoch=excluded.epoch,expires_ms=excluded.expires_ms")
        .bind(LINK_LEASE_NAME).bind(instance_id).bind(i64::try_from(epoch).unwrap_or(i64::MAX)).bind(lease_expiry(now, LINK_LEASE_TTL_MS)).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(true)
}

async fn renew(db: &CoordDb, instance_id: &str, now: i64) -> anyhow::Result<bool> {
    let result = sqlx::query("UPDATE coord_leases SET expires_ms=$1 WHERE name=$2 AND owner_instance=$3 AND expires_ms>$4")
        .bind(lease_expiry(now, LINK_LEASE_TTL_MS)).bind(LINK_LEASE_NAME).bind(instance_id).bind(now).execute(db.pool()).await?;
    Ok(result.rows_affected() == 1)
}

pub async fn release_for_shutdown(
    db: &CoordDb,
    instance_id: &str,
    state: &OwnershipState,
    notifier: &ClusterNotifier,
) -> anyhow::Result<()> {
    if state.is_owner() {
        if db.backend() == DbBackend::Postgres {
            sqlx::query("UPDATE coord_leases SET expires_ms=0 WHERE name=$1 AND owner_instance=$2")
                .bind(LINK_LEASE_NAME)
                .bind(instance_id)
                .execute(db.pool())
                .await?;
            notifier.notify(CLUSTER_CHANNEL, "released").await;
        }
        tracing::info!(instance_id, "coordinator link ownership released");
        state.set_owner(false);
    }
    Ok(())
}

async fn read_owner_url(db: &CoordDb, now: i64) -> anyhow::Result<Option<String>> {
    let live_owner: Option<String> = sqlx::query_scalar("SELECT i.internal_url FROM coord_leases l JOIN coord_instances i ON i.instance_id=l.owner_instance WHERE l.name=$1 AND l.expires_ms>$2 AND i.heartbeat_ms >= $3")
        .bind(LINK_LEASE_NAME).bind(now).bind(now.saturating_sub(INSTANCE_STALE_MS))
        .fetch_optional(db.pool()).await?;
    Ok(live_owner.filter(|url| !url.is_empty()))
}

/// Allow tests to mark ownership without driving the election timer.
#[cfg(test)]
mod tests {
    use super::OwnershipState;

    #[test]
    fn ownership_state_exposes_owner_address_and_heartbeat_freshness() {
        let state = OwnershipState::default();
        state.set_owner(true);
        state.set_owner_url(Some("http://127.0.0.1:4113".into()));
        state.set_heartbeat(10_000);
        assert!(state.is_owner());
        assert!(state.heartbeat_fresh(20_000));
        assert_eq!(
            state.owner_internal_url().as_deref(),
            Some("http://127.0.0.1:4113")
        );
    }
}
