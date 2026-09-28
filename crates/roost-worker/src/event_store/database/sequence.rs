//! The one sequence space that both the durable events and the snapshot draw
//! from: the block this process claims, and the watermark that decides what a
//! restart may hand out next. [`super::Journal`] calls it; nothing else does.
//!
//! It is a FOURTH concern, and none of the three beside it owns it.
//! [`super::schema`] verifies the file, [`super::rows`] is one row in and out,
//! and [`super::claims`] is a claim on CAPACITY — how many rows may be admitted
//! at all. None of those is a question about NUMBERING, and the failure modes
//! are not interchangeable either: a leaked capacity claim costs a row, while a
//! leaked sequence block costs the coordinator its ability to tell a replayed
//! event from a new one.
//!
//! A BLOCK IS CLAIMED AND WRITTEN AS A UNIT, and the write happens BEFORE any
//! value in the block is handed out. A crash therefore costs the unused tail of
//! one block rather than renumbering everything after it: the consequence is a
//! GAP in the sequence, never a repeat. A repeat is the one defect a durable
//! path cannot recover from on its own, because the coordinator acknowledges by
//! `client_seq` and has no second name for an event.

use super::super::SEQUENCE_BLOCK_SIZE;
use super::{Journal, JournalError, rows};

/// The highest sequence an INTEGER column can carry.
pub(super) const MAX_SEQUENCE: u64 = i64::MAX as u64;

/// The window of client sequences this process has claimed.
///
/// A block is claimed and written as a unit, so a crash costs the unused tail of
/// one block rather than renumbering everything after it. The consequence is a
/// GAP in the sequence, never a repeat.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct SequenceWindow {
    pub(super) issued: u64,
    pub(super) reserved_through: u64,
}

impl Journal {
    /// One sequence, under a window lock the caller already holds.
    pub(super) async fn claim_sequence_in(
        &self,
        window: &mut SequenceWindow,
    ) -> Result<u64, JournalError> {
        if window.issued >= window.reserved_through {
            self.reserve_block(window).await?;
        }
        window.issued += 1;
        Ok(window.issued)
    }

    /// The snapshot's sequence, drawn only while no committed row is numbered
    /// above `offered_through` (v2 `startSnapshotBarrier`: `oldestDurable()`,
    /// then `nextClientSeq()`). `None` when such a row exists: it must reach the
    /// coordinator before a snapshot numbered after it. Appends hold the same
    /// lock through their commit, so none can land between the check and the
    /// draw.
    pub async fn snapshot_sequence(
        &self,
        offered_through: u64,
    ) -> Result<Option<u64>, JournalError> {
        let mut window = self.window.lock().await;
        let newer: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM session_events WHERE client_seq > ?")
                .bind(rows::sequence_value(offered_through.min(MAX_SEQUENCE))?)
                .fetch_one(&self.pool)
                .await
                .map_err(query("snapshot sequence"))?;
        if newer > 0 {
            return Ok(None);
        }
        self.claim_sequence_in(&mut window).await.map(Some)
    }

    /// Persist the next block before any of it is handed out.
    ///
    /// The write happens BEFORE the first value is returned, which is what makes
    /// a crash cost a gap rather than a repeat.
    pub(super) async fn reserve_block(
        &self,
        window: &mut SequenceWindow,
    ) -> Result<(), JournalError> {
        let mut transaction = self.pool.begin().await.map_err(query("begin"))?;
        let reserved: i64 =
            sqlx::query_scalar("SELECT reserved_through FROM sequence_state WHERE singleton = 1")
                .fetch_one(&mut *transaction)
                .await
                .map_err(query("sequence read"))?;
        let reserved =
            u64::try_from(reserved).map_err(|_| corrupt("the sequence watermark is negative"))?;
        let floor = window.issued.max(reserved);
        let end = floor
            .checked_add(SEQUENCE_BLOCK_SIZE)
            .filter(|end| *end <= MAX_SEQUENCE)
            .ok_or_else(|| corrupt("the client sequence space is exhausted"))?;
        sqlx::query("UPDATE sequence_state SET reserved_through = ? WHERE singleton = 1")
            .bind(rows::sequence_value(end)?)
            .execute(&mut *transaction)
            .await
            .map_err(query("sequence reservation"))?;
        transaction.commit().await.map_err(query("commit"))?;
        window.issued = floor;
        window.reserved_through = end;
        Ok(())
    }
}

fn corrupt(reason: &str) -> JournalError {
    JournalError::Corrupt {
        reason: reason.to_owned(),
    }
}

fn query(label: &'static str) -> impl Fn(sqlx::Error) -> JournalError {
    move |error| JournalError::Query {
        label,
        reason: error.to_string(),
    }
}
