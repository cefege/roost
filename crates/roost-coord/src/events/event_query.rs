//! The three durable reads over the single global event stream, and the private
//! kind they all exclude.
//!
//! Ported from `apps/coord/src/events/event-query.ts`. Sync recovery reaches
//! these three functions and nothing else, and the windows they use are recovery
//! invariants rather than conveniences:
//!
//! | Read | Window | Limit | Why the window is that shape |
//! | --- | --- | ---: | --- |
//! | [`get_events_since`] | `id > since_id`, ascending | 1,000 | a reconnect backfill from the client's last applied id |
//! | [`get_event_max_id`] | `max(id)` over public rows | -- | the recovery cutoff, captured **after** the live subscription |
//! | [`get_events_through`] | `cursor < id <= cutoff` | 256 | one stable recovery interval, paged without ever moving the ceiling |
//!
//! The cutoff is captured after the live subscription, and the interval is
//! inclusive of the cutoff and exclusive of the cursor. Together they mean a
//! reconnect reads every event exactly once across the seam: the live lane
//! carries what was published after the cutoff and the backfill carries the
//! closed interval below it. An off-by-one in either comparison is a duplicated
//! or a missing event, and a duplicated `closed` is a terminal that vanishes.
//!
//! ALL THREE FILTER THE PRIVATE KIND. `agent_reference` is durable and its owning
//! worker recovers it, but no browser sees it through either lane. The filter is
//! the shared predicate's decision spelled as SQL
//! (`apps/coord/src/events/session-event-visibility.ts:7`), and the constant is
//! the same one the publisher and the row mapper read.
//!
//! THERE IS NO RETENTION ON THIS TABLE, and that is a deliberate gap in v2 rather
//! than an oversight (`docs/phase3-coord-contract.md` §3.8). This port does not
//! quietly close it.

use roost_protocol::wire::SessionEvent;
use sqlx::{Executor, Row};

use crate::events::visibility::PRIVATE_SESSION_EVENT_KIND;

/// The most events one backfill page returns.
pub const GET_EVENTS_SINCE_LIMIT: usize = 1_000;

/// The most events one recovery interval page returns.
pub const GET_EVENTS_THROUGH_LIMIT: usize = 256;

/// One durable event with the id it was committed under.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredEvent {
    /// The `events.id`, which is the recovery cursor's unit.
    pub id: u64,
    /// The decoded event.
    pub event: SessionEvent,
}

/// Why a durable read failed.
#[derive(Debug, thiserror::Error)]
pub enum EventQueryError {
    /// The statement failed.
    #[error("sqlite: {0}")]
    Sqlite(#[from] sqlx::Error),
    /// A stored payload is not a decodable event.
    ///
    /// v2's `JSON.parse` threw here and took the whole backfill with it
    /// (`event-query.ts:20`), which is the same failure with less information.
    /// Skipping the row instead would be worse: an event missing from a recovery
    /// interval is a client whose state silently diverges, and nothing would say
    /// which row it was.
    #[error("event {id} could not be decoded: {reason}")]
    Undecodable {
        /// The row that could not be read.
        id: u64,
        /// What the decoder said.
        reason: String,
    },
    /// A row's `id` is not a value a cursor can be.
    #[error("event row id {id} is not a usable cursor")]
    UnusableRowId {
        /// The offending value.
        id: i64,
    },
    /// A cursor is past the range the durable id column can hold, which means the
    /// caller built it wrong rather than that the database is damaged.
    #[error("event cursor {id} exceeds the durable id range")]
    CursorOutOfRange {
        /// The offending cursor.
        id: u64,
    },
}

/// Read back the public events after `since_id`, oldest first.
pub async fn get_events_since<'executor, E>(
    executor: E,
    since_id: u64,
    limit: Option<usize>,
) -> Result<Vec<StoredEvent>, EventQueryError>
where
    E: Executor<'executor, Database = sqlx::Sqlite>,
{
    let query = sqlx::query(
        "SELECT id, payload_json FROM events \
          WHERE id > ? AND kind != ? ORDER BY id ASC LIMIT ?",
    )
    .bind(cursor(since_id)?)
    .bind(PRIVATE_SESSION_EVENT_KIND)
    .bind(page_limit(limit, GET_EVENTS_SINCE_LIMIT));
    read_page(query, executor).await
}

/// The newest public event's id, or zero when no public row exists.
///
/// Zero is the right answer for an empty log and for a log whose newest rows are
/// all private: the cutoff's job is to name the last event a browser may already
/// have received, and a private event is not one.
pub async fn get_event_max_id<'executor, E>(executor: E) -> Result<u64, EventQueryError>
where
    E: Executor<'executor, Database = sqlx::Sqlite>,
{
    let row = sqlx::query("SELECT MAX(id) FROM events WHERE kind != ?")
        .bind(PRIVATE_SESSION_EVENT_KIND)
        .fetch_one(executor)
        .await?;
    match row.get::<Option<i64>, _>(0) {
        Some(id) => u64::try_from(id).map_err(|_| EventQueryError::UnusableRowId { id }),
        None => Ok(0),
    }
}

/// Read one stable recovery interval: `cursor < id <= cutoff`, oldest first.
pub async fn get_events_through<'executor, E>(
    executor: E,
    cursor_id: u64,
    cutoff: u64,
    limit: Option<usize>,
) -> Result<Vec<StoredEvent>, EventQueryError>
where
    E: Executor<'executor, Database = sqlx::Sqlite>,
{
    let query = sqlx::query(
        "SELECT id, payload_json FROM events \
          WHERE id > ? AND id <= ? AND kind != ? ORDER BY id ASC LIMIT ?",
    )
    .bind(cursor(cursor_id)?)
    .bind(cursor(cutoff)?)
    .bind(PRIVATE_SESSION_EVENT_KIND)
    .bind(page_limit(limit, GET_EVENTS_THROUGH_LIMIT));
    read_page(query, executor).await
}

async fn read_page<'executor, E>(
    query: sqlx::query::Query<'executor, sqlx::Sqlite, sqlx::sqlite::SqliteArguments>,
    executor: E,
) -> Result<Vec<StoredEvent>, EventQueryError>
where
    E: Executor<'executor, Database = sqlx::Sqlite>,
{
    let rows = query.fetch_all(executor).await?;
    let mut page = Vec::with_capacity(rows.len());
    for row in rows {
        let row_id: i64 = row.get(0);
        let payload_json: String = row.get(1);
        let id =
            u64::try_from(row_id).map_err(|_| EventQueryError::UnusableRowId { id: row_id })?;
        let event: SessionEvent =
            serde_json::from_str(&payload_json).map_err(|error| EventQueryError::Undecodable {
                id,
                reason: error.to_string(),
            })?;
        page.push(StoredEvent { id, event });
    }
    Ok(page)
}

/// The `events.id` column is a SQLite `INTEGER`, so a cursor that cannot be an
/// `i64` is refused at the boundary rather than wrapped into a negative id that
/// would silently match nothing.
fn cursor(id: u64) -> Result<i64, EventQueryError> {
    i64::try_from(id).map_err(|_| EventQueryError::CursorOutOfRange { id })
}

fn page_limit(limit: Option<usize>, default: usize) -> i64 {
    // A `usize` past `i64::MAX` is not representable in SQLite, and a limit that
    // large means "no limit" to every caller that would ask for it.
    i64::try_from(limit.unwrap_or(default)).unwrap_or(i64::MAX)
}
