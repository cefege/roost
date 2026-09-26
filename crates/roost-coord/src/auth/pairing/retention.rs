//! Pair-request retention: the sweep that expires live requests and reclaims
//! decided ones, and the lifetime the coordinator holds it for.
//!
//! Owned by the pairing slice. Ported from
//! `apps/coord/src/auth/pair-request-retention.ts`.
//!
//! WHY A SWEEP EXISTS AT ALL. A pair request is a standing credential for ten
//! minutes: anybody who learns its id and requester token can be confirmed into
//! a device by an operator who approves it. Past its deadline it is not a
//! credential, but it is still a row naming a key, a label, an address and a
//! city -- so the sweep does two things, in this order:
//!
//! 1. **Expire** every live request whose deadline has passed, clearing its
//!    verification-code digest in the same statement. A request that expires
//!    with its code digest still attached is a request whose code an attacker
//!    with a read-only database path could keep trying to match.
//! 2. **Reclaim** decided rows a day later. A day, not a minute, because
//!    "when was this device authorised, and by whom" is a question somebody asks
//!    long after the ceremony -- and `maintenance::audit_retention` makes the
//!    same call about the same records, on purpose.
//!
//! BOTH ENDS ARE BOUNDED IN BATCHES. A first run against a large backlog must
//! not hold the write lock for its whole duration on a live coordinator, so
//! each pass names at most [`PAIR_REQUEST_BATCH_SIZE`] rows and the loop ends
//! when a pass comes back short.

use std::sync::Arc;
use std::time::Duration;

use super::PairingResult;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::rows::{self, LiveSelector};
use super::status::TerminalRequest;
use super::sqlx_error;
use crate::db::CoordDb;
use crate::events::bus_messages::PairRequestDelta;
use crate::maintenance::audit_retention::DAY_MS;
use crate::services::CoordServices;

/// How often the sweep runs.
pub const PAIR_REQUEST_SWEEP_INTERVAL_MS: u64 = 60_000;

/// How long a decided request is kept before it is deleted.
pub const PAIR_REQUEST_TOMBSTONE_MS: i64 = DAY_MS;

/// Rows one pass may touch, on either end of the sweep.
pub const PAIR_REQUEST_BATCH_SIZE: i64 = 1_000;

/// What one sweep reclaimed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepOutcome {
    /// The requests terminalized as `expired`, by ceremony handle. Every one
    /// owes the bus a `removed` frame: they were pending a moment ago, and a
    /// viewer's list that still shows them is a viewer inviting an operator to
    /// approve a request that can no longer complete.
    pub expired: Vec<String>,
    /// Decided rows deleted, for the tombstone window.
    pub deleted: i64,
}

/// Expire overdue live requests, then reclaim decided ones older than a day.
///
/// `now_ms` is a parameter so a test can drive the ten-minute lifetime and the
/// one-day tombstone without waiting for either.
pub async fn sweep_pair_requests(database: &CoordDb, now_ms: i64) -> PairingResult<SweepOutcome> {
    let mut outcome = SweepOutcome::default();
    loop {
        // The expire side first, in bounded batches. This is the same
        // statement the approve and confirm paths use, so a request that
        // expired through the sweep and one that expired through an approval
        // are indistinguishable in the database.
        let batch = rows::terminalize(
            database.pool(),
            LiveSelector::ExpiredByBatch {
                bound: now_ms,
                limit: PAIR_REQUEST_BATCH_SIZE,
            },
            TerminalRequest::Expired,
            now_ms,
        )
        .await?;
        if batch.is_empty() {
            break;
        }
        outcome.expired.extend(batch);
    }
    // The tombstone side second: a decided row cannot become live again, so
    // there is nothing to expire, only age to reclaim.
    let cutoff = now_ms - PAIR_REQUEST_TOMBSTONE_MS;
    loop {
        let deleted = delete_tombstone_batch(database, cutoff).await?;
        outcome.deleted += deleted;
        if deleted < PAIR_REQUEST_BATCH_SIZE {
            break;
        }
    }
    Ok(outcome)
}

/// One bounded pass of the reclaim side. A short pass ends the loop.
async fn delete_tombstone_batch(database: &CoordDb, cutoff: i64) -> PairingResult<i64> {
    let deleted = sqlx::query(
        "DELETE FROM pair_requests WHERE id IN ( \
             SELECT id FROM pair_requests \
              WHERE status NOT IN ('pending', 'verification_required') \
                AND decided_at_ms IS NOT NULL AND decided_at_ms <= ? \
              ORDER BY decided_at_ms LIMIT ?) ",
    )
    .bind(cutoff)
    .bind(PAIR_REQUEST_BATCH_SIZE)
    .execute(database.pool())
    .await
    .map_err(|error| sqlx_error("pairing.retention", error))?
    .rows_affected();
    Ok(i64::try_from(deleted).unwrap_or(i64::MAX))
}

/// Run the retention loop for as long as `shutdown` stays open.
///
/// One sweep before the first sleep, so a coordinator that was down over a
/// request's deadline reclaims it at boot rather than a minute later: a live
/// request whose expiry has already passed is a credential until something
/// notices, and "a minute later" is the whole window.
///
/// `shutdown` is the coordinator's own shutdown signal, and this is the one
/// function that reads it: pass `None` only from a test that wants a sweep and
/// no way to stop it. The task ends when the signal fires or its sender drops,
/// and the returned [`RetentionSweep`] is what `serve` awaits on the way out --
/// which is what makes this a lifetime rather than a leak with a name.
#[must_use]
pub fn spawn_pair_request_retention(
    services: Arc<CoordServices>,
    shutdown: impl Into<Option<watch::Receiver<bool>>>,
) -> RetentionSweep {
    let mut shutdown = shutdown.into();
    let sweep_services = Arc::clone(&services);
    let handle = tokio::spawn(async move {
        loop {
            if let Err(error) = run_one_sweep(&sweep_services).await {
                // A sweep that throws is retried on the next tick rather than
                // ending the loop: retention that stops silently is retention
                // that never runs again, and nothing else would notice.
                tracing::error!(error = %error, "pair request retention sweep failed");
            }
            if let Some(receiver) = shutdown.as_mut() {
                if *receiver.borrow_and_update() {
                    tracing::info!("pair request retention sweep stopped");
                    return;
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(PAIR_REQUEST_SWEEP_INTERVAL_MS)) => {}
                    changed = receiver.changed() => {
                        if changed.is_err() || *receiver.borrow_and_update() {
                            tracing::info!("pair request retention sweep stopped");
                            return;
                        }
                    }
                }
            } else {
                tokio::time::sleep(Duration::from_millis(PAIR_REQUEST_SWEEP_INTERVAL_MS)).await;
            }
        }
    });
    RetentionSweep { handle }
}

/// One sweep, and the line it emits for each end that reclaimed something.
async fn run_one_sweep(services: &CoordServices) -> PairingResult<()> {
    let outcome = sweep_pair_requests(&services.db, crate::rpc::service::now_ms()).await?;
    for ephemeral_id in &outcome.expired {
        services.buses.pair_bus.publish(PairRequestDelta::Removed {
            ephemeral_id: ephemeral_id.clone(),
        });
    }
    if !outcome.expired.is_empty() {
        tracing::info!(
            expired = outcome.expired.len(),
            "pair requests expired by retention"
        );
    }
    if outcome.deleted > 0 {
        tracing::info!(
            deleted = outcome.deleted,
            "pair request tombstones reclaimed"
        );
    }
    Ok(())
}

/// The running sweep, and the one way to stop it.
#[derive(Debug)]
pub struct RetentionSweep {
    /// The task. Awaited by [`RetentionSweep::stop`] and never aborted: an
    /// abort mid-statement would leave SQLite to roll the sweep back on a
    /// future nobody was waiting for.
    handle: JoinHandle<()>,
}

impl RetentionSweep {
    /// Stop the sweep and wait for the tick it is on to finish.
    ///
    /// `timeout` because a sweep is a bounded number of statements and the only
    /// way one outlives its shutdown is a database that is already wedged --
    /// in which case the caller is about to find that out the hard way, and
    /// this should not be the reason.
    pub async fn stop(self) {
        const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
        if tokio::time::timeout(SHUTDOWN_GRACE, self.handle)
            .await
            .is_err()
        {
            tracing::warn!("pair request retention sweep did not stop within its grace");
        }
    }
}
