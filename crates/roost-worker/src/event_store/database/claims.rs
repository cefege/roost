//! The durable half's capacity claims: a session takes the room its `closed`
//! event will need BEFORE the PTY exists, and that claim outlives the process.
//! `Journal` calls it; nothing else does.
//!
//! PERSISTED, because the case the claim exists for is a machine that reboots
//! between the reservation and the `opened`: a claim held only in a heap is
//! silently gone, and the store then admits an `opened` it has no room to close.
//!
//! LEASED, because persistence turns the other failure mode from transient into
//! permanent — a claim held by a process that died is capacity nobody hands
//! back, and after enough crashed spawns the store refuses every write while
//! presenting as "the outbox is full", which is the one diagnosis nobody chases
//! back to a boot three weeks earlier. So every claim is stamped, and
//! [`Journal::reclaim_expired_claims`] takes the ones past
//! [`RESERVATION_LEASE`] while leaving the ones inside it alone: a rule that
//! cannot tell stale from fresh is indistinguishable from one that discards
//! everything, and what it would take is a concurrent spawn's claim.
//!
//! The cap arithmetic is NOT here. It is
//! [`crate::event_store::admission`], which the in-memory half also calls, so
//! the two cannot disagree about what "full" means.
//!
//! `ClaimRefusal` is a SIBLING of `ReserveError` and not a variant on it:
//! `ReserveError` derives `Copy` because it is the token-level vocabulary and
//! every match on it is allocation-free, and a store failure has to carry a
//! reason string. Adding a `String` variant there red-lines every consumer.

use sqlx::sqlite::SqlitePool;

use super::super::{
    DurableEventKind, RESERVATION_LEASE, Reservation, ReserveError, admission_fits,
    claim_is_well_formed,
};
use super::{Journal, JournalError};

/// Why a claim was not taken.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClaimRefusal {
    /// The cap or the bound said no. The in-memory half's own vocabulary, on
    /// purpose: one rule, two callers.
    #[error(transparent)]
    Refused(#[from] ReserveError),
    /// The file said no.
    #[error("the durable outbox refused the claim: {reason}")]
    Store { reason: String },
}

/// Wall-clock milliseconds, the clock the lease is measured in.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

/// The instant before which a claim is dead.
pub fn lease_cutoff(now: i64) -> i64 {
    now.saturating_sub(i64::try_from(RESERVATION_LEASE.as_millis()).unwrap_or(i64::MAX))
}

fn store(error: sqlx::Error) -> ClaimRefusal {
    ClaimRefusal::Store {
        reason: error.to_string(),
    }
}

/// What the file holds, for the admission arithmetic: stored rows plus every
/// LIVE claim's bytes. Expired claims are excluded rather than deleted here, so
/// the arithmetic cannot depend on when a sweep last ran.
pub async fn used_totals(pool: &SqlitePool, now: i64) -> Result<(usize, usize), JournalError> {
    let cutoff = lease_cutoff(now);
    let rows: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM session_events")
        .fetch_one(pool)
        .await
        .map_err(store_query("used rows"))?;
    let claims: (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COALESCE(SUM(payload_bytes), 0) FROM session_claims \
         WHERE claimed_at_ms >= ?",
    )
    .bind(cutoff)
    .fetch_one(pool)
    .await
    .map_err(store_query("used claims"))?;
    let stored_bytes: (i64,) =
        sqlx::query_as("SELECT COALESCE(SUM(payload_bytes), 0) FROM session_events")
            .fetch_one(pool)
            .await
            .map_err(store_query("used bytes"))?;
    Ok((
        to_count(rows.0)? + to_count(claims.0)?,
        to_count(stored_bytes.0)? + to_count(claims.1)?,
    ))
}

fn to_count(value: i64) -> Result<usize, JournalError> {
    usize::try_from(value).map_err(|_| corrupt("a count is not a count"))
}

fn corrupt(reason: &str) -> JournalError {
    JournalError::Corrupt {
        reason: reason.to_owned(),
    }
}

fn store_query(label: &'static str) -> impl Fn(sqlx::Error) -> JournalError {
    move |error| JournalError::Query {
        label,
        reason: error.to_string(),
    }
}

/// Reclaim every claim past the lease at `now`. [`Journal::open`]'s half; the
/// public form is [`Journal::reclaim_expired_claims`], which is what a test
/// drives so the rule is exercised rather than a second path to the table.
pub(crate) async fn reclaim_expired(pool: &SqlitePool, now: i64) -> Result<usize, JournalError> {
    let reclaimed = sqlx::query("DELETE FROM session_claims WHERE claimed_at_ms < ?")
        .bind(lease_cutoff(now))
        .execute(pool)
        .await
        .map_err(store_query("claim reclaim"))?;
    usize::try_from(reclaimed.rows_affected()).map_err(|_| corrupt("a count is not a count"))
}

impl Journal {
    /// Take a persisted claim on capacity, and return the token that owns it.
    ///
    /// The token is returned only after the row is committed, so a caller that
    /// holds one may open a PTY on the strength of it. That ordering is the
    /// whole reason the claim is durable rather than a counter in a heap.
    pub async fn reserve(
        &self,
        kind: DurableEventKind,
        payload_bytes: usize,
    ) -> Result<Reservation, ClaimRefusal> {
        claim_is_well_formed(kind, payload_bytes)?;
        let now = now_ms();
        let (used_rows, used_bytes) =
            used_totals(&self.pool, now)
                .await
                .map_err(|error| ClaimRefusal::Store {
                    reason: error.to_string(),
                })?;
        admission_fits(used_rows, used_bytes, payload_bytes)?;
        let id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO session_claims (id, kind, payload_bytes, claimed_at_ms, snapshot_blocking) \
             VALUES ((SELECT COALESCE(MAX(id), 0) + 1 FROM session_claims), ?, ?, ?, 1) RETURNING id",
        )
        .bind(kind_name(kind))
        .bind(i64::try_from(payload_bytes).map_err(|_| ClaimRefusal::Refused(
            ReserveError::PayloadNotPositive { payload: payload_bytes },
        ))?)
        .bind(now)
        .fetch_one(&self.pool)
        .await
        .map_err(store)?;
        tracing::debug!(
            id,
            ?kind,
            payload_bytes,
            "the durable outbox took a claim on capacity"
        );
        Ok(Reservation {
            id: u64::try_from(id).map_err(|_| ClaimRefusal::Store {
                reason: "a claim id is past what a token can hold".to_owned(),
            })?,
            kind,
            payload_bytes,
            snapshot_blocking: true,
        })
    }

    /// Stop a claim from blocking a snapshot, WITHOUT giving it up.
    ///
    /// The session is committed: it is open and the coordinator knows, so its
    /// close is not speculative. The claim survives because the close still has
    /// to fit, and releasing it here would let a busy store strand a live
    /// session with nowhere to record its end.
    pub async fn hold(&self, claim: Reservation) -> Result<Reservation, ClaimRefusal> {
        let changed = sqlx::query(
            "UPDATE session_claims SET snapshot_blocking = 0 WHERE id = ? AND snapshot_blocking = 1",
        )
        .bind(claim_id(claim)?)
        .execute(&self.pool)
        .await
        .map_err(store)?;
        if changed.rows_affected() != 1 {
            return Err(ClaimRefusal::Refused(ReserveError::AlreadyHeld {
                id: claim.id,
            }));
        }
        Ok(Reservation {
            snapshot_blocking: false,
            ..claim
        })
    }

    /// Give a claim back without writing anything.
    pub async fn release(&self, claim: Reservation) -> Result<(), ClaimRefusal> {
        let removed = sqlx::query("DELETE FROM session_claims WHERE id = ?")
            .bind(claim_id(claim)?)
            .execute(&self.pool)
            .await
            .map_err(store)?;
        if removed.rows_affected() != 1 {
            return Err(ClaimRefusal::Refused(ReserveError::ReservationNotLive {
                id: claim.id,
            }));
        }
        Ok(())
    }

    /// Reclaim every claim older than [`RESERVATION_LEASE`] at `now`.
    ///
    /// The clock is a PARAMETER, and that is the whole reason this is a real
    /// rule rather than a plausible one: aging a claim and leaving a fresh one
    /// alone is the property, and a test that cannot move the clock cannot tell
    /// the two apart — a rule that cannot is indistinguishable from one that
    /// discards everything. [`Journal::open`] is the production caller.
    pub async fn reclaim_expired_claims(&self, now: i64) -> Result<usize, JournalError> {
        let reclaimed = sqlx::query("DELETE FROM session_claims WHERE claimed_at_ms < ?")
            .bind(lease_cutoff(now))
            .execute(&self.pool)
            .await
            .map_err(store_query("claim reclaim"))?;
        usize::try_from(reclaimed.rows_affected()).map_err(|_| corrupt("a count is not a count"))
    }

    /// Claims still live, and how many of them still block a snapshot.
    pub async fn claims(&self) -> Result<(usize, usize), JournalError> {
        let (live, blocking): (i64, i64) = sqlx::query_as(
            "SELECT COUNT(*), COALESCE(SUM(snapshot_blocking), 0) FROM session_claims \
             WHERE claimed_at_ms >= ?",
        )
        .bind(lease_cutoff(now_ms()))
        .fetch_one(&self.pool)
        .await
        .map_err(store_query("claim count"))?;
        Ok((to_count(live)?, to_count(blocking)?))
    }

    /// Retire a claim inside a transaction that is already writing its row.
    ///
    /// Split out so `emit` and `append` share ONE definition of "a claim is
    /// spent", which is the whole difference between them: `emit` is this plus
    /// the insert, in the same transaction.
    pub(crate) async fn retire_claim(
        transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        claim: Reservation,
    ) -> Result<(), JournalError> {
        let removed = sqlx::query("DELETE FROM session_claims WHERE id = ?")
            .bind(claim_id(claim).map_err(|error| JournalError::Query {
                label: "emit",
                reason: error.to_string(),
            })?)
            .execute(&mut **transaction)
            .await
            .map_err(store_query("claim retire"))?;
        if removed.rows_affected() != 1 {
            return Err(corrupt(
                "the claim was not there to retire under its own event",
            ));
        }
        Ok(())
    }
}

fn claim_id(claim: Reservation) -> Result<i64, ClaimRefusal> {
    i64::try_from(claim.id)
        .map_err(|_| ClaimRefusal::Refused(ReserveError::ReservationNotLive { id: claim.id }))
}

/// The stored spelling of a claim's kind, so a reopened file reads as the same
/// vocabulary it was written in.
fn kind_name(kind: DurableEventKind) -> &'static str {
    match kind {
        DurableEventKind::Opened => "opened",
        DurableEventKind::State => "state",
        DurableEventKind::Exited => "exited",
        DurableEventKind::Closed => "closed",
    }
}
