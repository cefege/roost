//! audit_log retention: the one high-volume, low-signal method ages out, and
//! everything else is kept forever.
//!
//! Owned by the coordinator. The reason this module exists, from v2
//! (`audit-retention.ts:1-11`): audit_log had no retention at all and reached
//! 7,026,358 rows / 1.0 GB before a one-off manual prune cut it to ~174k.
//! Without a sweep it regrows.

use std::time::Duration;

use crate::db::CoordDb;
use crate::serve::now_ms;

/// One day. The retention window is counted in these, the sweep runs once per
/// one, and `backup` schedules on the same value -- one definition so the
/// three cannot silently drift apart (`audit-retention.ts:17`).
pub const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// Rows per DELETE statement (`audit-retention.ts:19`).
pub const AUDIT_BATCH_SIZE: i64 = 10_000;

/// The ONLY methods this sweep may delete.
///
/// An explicit allowlist, never a predicate and never a wildcard: audit_log
/// also holds the low-volume, high-forensic-value rows -- PairApprove,
/// PairConfirm, AuthRedeemBrowser, WorkersDelete, WorkspacesDelete,
/// SessionsKill, SessionsSpawn -- and "when was this device authorised, and by
/// whom" is exactly the question someone asks a year later. `SessionsInput` is
/// here because it is the remaining top contributor: ~42k rows of "who typed
/// into which session", which is real audit data and so has to age out rather
/// than be skipped at write time. To extend: add a name here. Never add
/// anything authorization, pairing, deletion or lifecycle-related
/// (`audit-retention.ts:21-48`).
pub const AUDIT_SWEEP_METHODS: &[&str] = &["SessionsInput"];

/// The window, and the batch size, for one sweep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditSweepOptions {
    /// How many days of `SessionsInput` rows to keep.
    pub retention_days: u64,
    /// The wall clock, for a test to pin. Defaults to now.
    pub now_ms: Option<i64>,
    /// Rows per statement. Defaults to [`AUDIT_BATCH_SIZE`].
    pub batch_size: Option<i64>,
}

impl AuditSweepOptions {
    /// The configured window at the default batch size, on the real clock.
    #[must_use]
    pub fn daily(retention_days: u64) -> Self {
        Self {
            retention_days,
            now_ms: None,
            batch_size: None,
        }
    }
}

/// Delete swept-method rows older than the window. Returns the count.
///
/// `ts` is epoch MILLISECONDS (`migrations/0001_init.sql`), and `path` is
/// `/<service>/<Method>` with a service prefix that varies across proto
/// packages -- so the match is on the trailing segment, and NEVER on the
/// `method` column, which the interceptor fills with the literal HTTP verb
/// (`audit-retention.ts:98-104`).
pub async fn sweep_audit_log(
    database: &CoordDb,
    options: AuditSweepOptions,
) -> Result<u64, sqlx::Error> {
    let now = options.now_ms.unwrap_or_else(now_ms);
    let batch_size = options.batch_size.unwrap_or(AUDIT_BATCH_SIZE);
    let days = i64::try_from(options.retention_days).unwrap_or(i64::MAX);
    let cutoff = now.saturating_sub(days.saturating_mul(DAY_MS));

    // Bounded batches, not one unbounded DELETE: a first run against a large
    // backlog must not hold the write lock for its whole duration on a live
    // coordinator. The LIMIT rides on a subselect, and `audit_log_ts` turns
    // `ts < ?` into a bounded index range scan (`audit-retention.ts:105-118`).
    let path_filter = AUDIT_SWEEP_METHODS
        .iter()
        .map(|_| "path LIKE ?")
        .collect::<Vec<_>>()
        .join(" OR ");
    let statement = format!(
        "DELETE FROM audit_log WHERE id IN (SELECT id FROM audit_log \
         WHERE ts < ? AND ({path_filter}) ORDER BY ts LIMIT ?)"
    );
    let mut query = sqlx::query(&statement).bind(cutoff);
    for method in AUDIT_SWEEP_METHODS {
        query = query.bind(format!("%/{method}"));
    }
    let mut query = query.bind(batch_size);

    let mut deleted = 0_u64;
    loop {
        let outcome = query.execute(database.pool()).await?;
        let changed = outcome.rows_affected();
        deleted += changed;
        // A short batch means the cutoff range is exhausted.
        if i64::try_from(changed).unwrap_or(i64::MAX) < batch_size {
            break;
        }
        // The coordinator's audit inserts run on this connection, so a tight
        // loop over a multi-million-row backlog would block every RPC until it
        // finished -- the batching alone buys nothing without an explicit yield
        // (`audit-retention.ts:132-136`).
        tokio::task::yield_now().await;
    }

    // Deliberately no VACUUM. Reclaiming the freed pages needs an EXCLUSIVE
    // lock over the whole file and rewrites it end to end; on a live coord that
    // stalls every RPC for as long as it takes. The freed pages are reused by
    // subsequent inserts, so the file stops growing without one, and shrinking
    // it is a manual, out-of-hours operation (`audit-retention.ts:141-146`).
    Ok(deleted)
}

/// Remove the pre-hardening backlog of anonymous successful SPA/static reads.
/// Returns the count.
///
/// Startup-only: the recurring task keeps the narrow forensic sweep
/// (`audit-retention.ts:56-58, 170-171`). Sweeping these on a schedule would
/// mean paying an INSERT per RPC to delete the row days later.
pub async fn cleanup_anonymous_static_audit_log(
    database: &CoordDb,
    batch_size: Option<i64>,
) -> Result<u64, sqlx::Error> {
    let limit = batch_size.unwrap_or(AUDIT_BATCH_SIZE);
    let mut query = sqlx::query(
        "DELETE FROM audit_log WHERE id IN (\
           SELECT id FROM audit_log \
           WHERE caller_fp IS NULL \
             AND method IN ('GET', 'HEAD') \
             AND status >= 200 AND status < 400 \
             AND path <> '/api/db-export' \
             AND path NOT LIKE '/api/%' \
             AND path <> '/internal' AND path NOT LIKE '/internal/%' \
             AND path <> '/ws' AND path NOT LIKE '/ws/%' \
             AND path NOT LIKE '/roost.%' \
           ORDER BY id LIMIT ?)",
    )
    .bind(limit);

    let mut deleted = 0_u64;
    loop {
        let outcome = query.execute(database.pool()).await?;
        let changed = outcome.rows_affected();
        deleted += changed;
        if i64::try_from(changed).unwrap_or(i64::MAX) < limit {
            break;
        }
        tokio::task::yield_now().await;
    }
    Ok(deleted)
}

/// Run the retention loop: the static backlog once at boot, then the forensic
/// sweep once a day.
///
/// The task is detached; see `backup::spawn_scheduled_backups` for why there is
/// nothing to unref.
pub fn spawn_audit_retention(database: CoordDb, retention_days: u64) {
    tokio::spawn(async move {
        match cleanup_anonymous_static_audit_log(&database, None).await {
            Ok(0) => {}
            Ok(deleted) => tracing::info!(deleted, "anonymous static audit rows cleaned"),
            Err(error) => {
                tracing::error!(error = %error, "anonymous static audit cleanup failed");
            }
        }
        loop {
            match sweep_audit_log(&database, AuditSweepOptions::daily(retention_days)).await {
                // Silence is the steady state; only a real deletion is a line.
                Ok(0) => {}
                Ok(deleted) => {
                    tracing::info!(deleted, retention_days, "audit_log pruned");
                }
                Err(error) => tracing::error!(error = %error, "audit_log prune failed"),
            }
            tokio::time::sleep(Duration::from_millis(DAY_MS as u64)).await;
        }
    });
}
