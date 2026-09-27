//! The durable outbox's SQLite half: the file, the append, and the
//! exact-sequence acknowledgement that is the only thing permitted to remove a
//! row. Owned by [`super::event_store`]; `sqlx` appears here and in its four
//! children and nowhere else in the worker.
//!
//! The admission half is [`super::Store`]. This half holds no capacity
//! ARITHMETIC of its own: the rule is [`super::admission`], which both halves
//! call, because a store that admits and persists with two ideas of "full" is a
//! store whose two answers can disagree. What lives here is PERSISTENCE, and
//! it is split by what each part owns: the FILE ([`schema`]), ONE ROW
//! ([`rows`]), the CAPACITY a session claimed before its PTY existed
//! ([`claims`]), and the one that is about NUMBERING rather than capacity — the
//! client-sequence block, whose own failure mode is a duplicated sequence and
//! not a duplicated row ([`sequence`]).
//!
//! TWO INVARIANTS, and everything else here serves one of them.
//!
//! **A row leaves only on the EXACT coordinator acknowledgement.** A row the
//! coordinator has not confirmed is the whole reason this file exists: a worker
//! restarted mid-event must offer it again, and a store that tidied it away on a
//! timeout would turn at-least-once into at-most-once with nothing to show for it.
//!
//! **Replay is one row at a time, oldest first.** [`Journal::replay_head`]
//! returns ONE row, never a batch. The coordinator's `client_seq` ledger is a
//! single ordered stream, so two events in flight make the two acknowledgements
//! ambiguous — and an ambiguous acknowledgement is the one state a durable path
//! cannot recover from on its own.

pub mod claims;
pub mod rows;
pub mod schema;
pub mod sequence;

use std::path::Path;
use std::time::Duration;

use roost_protocol::wire::event::SessionEvent;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

use super::{MAX_PAYLOAD_BYTES, MAX_ROWS};
use sequence::SequenceWindow;

pub use rows::PendingRow;
/// The name is v2's, deliberately: an operator reading a support bundle sees
/// one filename, and a worker's durable record should not change shape because
/// the implementation did.
pub const DATABASE_FILE_NAME: &str = "session-event-outbox.sqlite";

/// How long a writer waits for the outbox's one connection before giving up.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// What the outbox file currently holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JournalStats {
    pub rows: usize,
    pub payload_bytes: usize,
    pub database_bytes: u64,
}

/// Why the outbox refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JournalError {
    #[error("the durable outbox at {path} could not be opened: {reason}")]
    Open { path: String, reason: String },
    #[error("the durable outbox refused its own configuration: {reason}")]
    Configuration { reason: String },
    #[error("the durable outbox holds a schema this build does not own: {reason}")]
    Schema { reason: String },
    #[error(
        "the durable outbox is full: {rows} rows and {bytes} payload bytes against caps of \
         {max_rows} and {max_bytes}"
    )]
    Full {
        rows: usize,
        bytes: usize,
        max_rows: usize,
        max_bytes: usize,
    },
    #[error("the durable outbox {label} failed: {reason}")]
    Query { label: &'static str, reason: String },
    #[error("the durable outbox holds a corrupt row: {reason}")]
    Corrupt { reason: String },
}

/// The durable outbox.
pub struct Journal {
    pool: SqlitePool,
    /// Held across the block-claim write, which is why this is the async mutex:
    /// a `std` guard across an await is a deadlock waiting for a second caller.
    window: tokio::sync::Mutex<SequenceWindow>,
    /// The sequence this process took over at, so a log line can explain a gap
    /// across a restart and the caller can align the link's barrier with it.
    handed_over_at: u64,
}

impl std::fmt::Debug for Journal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Journal")
            .field("handed_over_at", &self.handed_over_at)
            .finish_non_exhaustive()
    }
}

impl Journal {
    /// Open (or create) the outbox at `path`, and refuse anything it cannot own.
    ///
    /// A refusal here STOPS the worker. A durable store that half-opened would
    /// accept events it cannot replay, and the loss would surface as a session
    /// the coordinator never heard of rather than as a store that would not
    /// start.
    pub async fn open(path: &Path) -> Result<Self, JournalError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| JournalError::Open {
                path: parent.display().to_string(),
                reason: error.to_string(),
            })?;
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            // DELETE, not WAL: a row that has not been acknowledged must be in
            // the main file when the process dies, and an un-checkpointed WAL is
            // exactly the case this file exists for.
            .journal_mode(SqliteJournalMode::Delete)
            // FULL, not NORMAL: an acknowledged event must not come back after a
            // power cut, and must not vanish before it does.
            .synchronous(SqliteSynchronous::Full)
            .busy_timeout(BUSY_TIMEOUT);
        // One connection, so the pragmas sqlx applies per connection are applied
        // to THE connection. The outbox admits one event in flight by contract,
        // so a second connection would buy contention and nothing else.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .acquire_timeout(BUSY_TIMEOUT)
            .connect_with(options)
            .await
            .map_err(|error| JournalError::Open {
                path: path.display().to_string(),
                reason: error.to_string(),
            })?;
        schema::establish(&pool).await?;
        // Every open reclaims the claims past the lease and leaves the ones
        // inside it. A sweep, not a per-reserve check, so a long-lived worker
        // cannot quietly eat its own live claims one lease at a time.
        let reclaimed = claims::reclaim_expired(&pool, claims::now_ms()).await?;
        if reclaimed > 0 {
            tracing::warn!(
                reclaimed,
                "the durable outbox reclaimed capacity claims abandoned by a process that did not \
                 release them"
            );
        }
        let handed_over_at = schema::persisted_high_water(&pool).await?;
        Ok(Journal {
            pool,
            window: tokio::sync::Mutex::new(SequenceWindow {
                issued: handed_over_at,
                reserved_through: 0,
            }),
            handed_over_at,
        })
    }

    /// Where this process took the sequence over.
    pub fn handed_over_at(&self) -> u64 {
        self.handed_over_at
    }

    /// Write one event the caller has ALREADY claimed capacity for, retiring
    /// the claim in the SAME transaction.
    ///
    /// This is the primitive the session layer publishes through. Two
    /// transactions — a write then a retire — would leave a row on disk with a
    /// live claim behind it, and that double-counts capacity for an event that
    /// is already durable. It is not the crash path that manufactures the leak;
    /// it is the ORDINARY one, which is why it has to be one transaction.
    pub async fn emit(
        &self,
        claim: super::Reservation,
        event: &SessionEvent,
    ) -> Result<PendingRow, JournalError> {
        self.append_within(event, Some(claim)).await
    }

    /// Write one event, and return the row that will be replayed for it.
    ///
    /// NO CLAIM. For the paths that genuinely hold none — the snapshot is one,
    /// and the barrier owns it — and stated here so the next reader knows which
    /// primitive they are looking at: a caller holding a claim wants
    /// [`Journal::emit`].
    pub async fn append(&self, event: &SessionEvent) -> Result<PendingRow, JournalError> {
        self.append_within(event, None).await
    }

    /// The shared body of [`Journal::emit`] and [`Journal::append`].
    async fn append_within(
        &self,
        event: &SessionEvent,
        claim: Option<super::Reservation>,
    ) -> Result<PendingRow, JournalError> {
        let value = serde_json::to_value(event).map_err(|error| JournalError::Query {
            label: "serialization",
            reason: error.to_string(),
        })?;
        let event_json = serde_json::to_string(&value).map_err(|error| JournalError::Query {
            label: "serialization",
            reason: error.to_string(),
        })?;
        let kind = rows::wire_kind(&value)?;
        let payload_bytes = event_json.len();
        let client_seq = self.claim_sequence().await?;

        let mut transaction = self.pool.begin().await.map_err(query("begin"))?;
        if let Some(claim) = claim {
            Self::retire_claim(&mut transaction, claim).await?;
        }
        let (held_rows, held_bytes) = held_totals(&mut transaction).await?;
        // Inclusive caps: a store holding exactly the cap admits nothing more,
        // and the next event is usually a close.
        if held_rows >= MAX_ROWS || held_bytes.saturating_add(payload_bytes) > MAX_PAYLOAD_BYTES {
            return Err(JournalError::Full {
                rows: held_rows,
                bytes: held_bytes,
                max_rows: MAX_ROWS,
                max_bytes: MAX_PAYLOAD_BYTES,
            });
        }
        sqlx::query(
            "INSERT INTO session_events (client_seq, kind, event_json, payload_bytes) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(rows::sequence_value(client_seq)?)
        .bind(&kind)
        .bind(&event_json)
        .bind(rows::sequence_value(
            u64::try_from(payload_bytes)
                .map_err(|_| corrupt("a payload byte count is past what the column can hold"))?,
        )?)
        .execute(&mut *transaction)
        .await
        .map_err(query("append"))?;
        // Retention is re-read rather than assumed. An insert that did not land
        // would otherwise be reported as a durable event, and a durable event
        // that is not there is a hole nobody can detect downstream.
        let retained: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM session_events WHERE client_seq = ?")
                .bind(rows::sequence_value(client_seq)?)
                .fetch_one(&mut *transaction)
                .await
                .map_err(query("append verification"))?;
        if retained != 1 {
            return Err(corrupt("the appended row was not retained"));
        }
        transaction.commit().await.map_err(query("commit"))?;
        tracing::info!(
            client_seq,
            kind = %kind,
            payload_bytes,
            "the durable outbox accepted a session event"
        );
        Ok(PendingRow {
            client_seq,
            kind,
            event: event.clone(),
            payload_bytes,
        })
    }

    /// The ONE row that may go next, oldest first, or none.
    ///
    /// One row and not a batch is the point: the coordinator acknowledges each
    /// durable event by its exact sequence, so a second row in flight would make
    /// two acknowledgements ambiguous.
    pub async fn replay_head(&self) -> Result<Option<PendingRow>, JournalError> {
        let row = sqlx::query(
            "SELECT client_seq, kind, event_json, payload_bytes FROM session_events \
             ORDER BY client_seq LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(query("replay head"))?;
        row.map(rows::decode).transpose()
    }

    /// Every row still waiting, oldest first. For a count and a diagnostic; the
    /// replay itself goes through [`Journal::replay_head`].
    pub async fn pending(&self) -> Result<Vec<PendingRow>, JournalError> {
        let rows = sqlx::query(
            "SELECT client_seq, kind, event_json, payload_bytes FROM session_events \
             ORDER BY client_seq",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(query("pending read"))?;
        rows.into_iter().map(rows::decode).collect()
    }

    /// Remove exactly this sequence. `false` when no such row is waiting.
    ///
    /// `false` is a normal answer here — a reconnect replays, so the coordinator
    /// re-acknowledges a row that has already gone — and it is the CALLER that
    /// has to notice, because the barrier's in-flight row must never vanish
    /// before its own acknowledgement arrives.
    pub async fn acknowledge(&self, client_seq: u64) -> Result<bool, JournalError> {
        if client_seq == 0 || client_seq > sequence::MAX_SEQUENCE {
            return Ok(false);
        }
        let removed = sqlx::query("DELETE FROM session_events WHERE client_seq = ?")
            .bind(rows::sequence_value(client_seq)?)
            .execute(&self.pool)
            .await
            .map_err(query("acknowledge"))?;
        let removed = removed.rows_affected() == 1;
        if removed {
            tracing::info!(client_seq, "the coordinator acknowledged a durable row");
        }
        Ok(removed)
    }

    /// What the file holds, for `roost doctor` and for the admission half.
    pub async fn stats(&self) -> Result<JournalStats, JournalError> {
        let totals: (i64, i64) =
            sqlx::query_as("SELECT COUNT(*), COALESCE(SUM(payload_bytes), 0) FROM session_events")
                .fetch_one(&self.pool)
                .await
                .map_err(query("stats"))?;
        let (pages, page_size): (i64, i64) =
            sqlx::query_as("SELECT * FROM pragma_page_count(), pragma_page_size()")
                .fetch_one(&self.pool)
                .await
                .map_err(query("stats"))?;
        Ok(JournalStats {
            rows: count(totals.0, "a row count is not a count")?,
            payload_bytes: count(totals.1, "a payload total is not a count")?,
            database_bytes: u64::try_from(pages.saturating_mul(page_size).max(0))
                .map_err(|_| corrupt("a page count is not a count"))?,
        })
    }

    /// Close the file. Un-acknowledged rows stay on disk; that is the point.
    pub async fn close(&self) -> Result<(), JournalError> {
        self.pool.close().await;
        Ok(())
    }
}

/// What the file holds, counted inside the append's own transaction so the cap is
/// read against the rows this write is about to join.
async fn held_totals(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<(usize, usize), JournalError> {
    let (rows, bytes): (i64, i64) =
        sqlx::query_as("SELECT COUNT(*), COALESCE(SUM(payload_bytes), 0) FROM session_events")
            .fetch_one(&mut **transaction)
            .await
            .map_err(query("bounds read"))?;
    Ok((
        count(rows, "a row count is not a count")?,
        count(bytes, "a payload total is not a count")?,
    ))
}

fn count(value: i64, reason: &'static str) -> Result<usize, JournalError> {
    usize::try_from(value).map_err(|_| corrupt(reason))
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
