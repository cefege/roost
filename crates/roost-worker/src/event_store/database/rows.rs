//! One durable-outbox row, in and out. Called by [`super::Journal`] and by
//! nothing else.
//!
//! Every check in [`decode`] is a claim the outbox makes to the coordinator. A
//! row whose recorded byte count disagrees with its own text, or whose recorded
//! kind is not the kind of the event it holds, would replay as a fact that never
//! happened — and the coordinator, which sees one opaque `client_seq`, has no
//! way to tell.

use roost_protocol::wire::event::SessionEvent;
use serde_json::Value;
use sqlx::sqlite::SqliteRow;
use sqlx::Row;

use super::{corrupt, JournalError};

/// One row of the outbox, decoded and checked.
///
/// `event` is the decoded union rather than the stored text because this is what
/// goes on the wire: a replay after a restart must be built from the row, not
/// from the producer that no longer exists.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingRow {
    /// The sequence the coordinator will acknowledge this row under. Stable
    /// across every retry, which is what makes a duplicate recognisable.
    pub client_seq: u64,
    /// The event's own wire discriminant, kept beside the event so a log line
    /// says what is waiting without decoding it.
    pub kind: String,
    pub event: SessionEvent,
    pub payload_bytes: usize,
}

/// The event's own wire discriminant, which is what a row is indexed by.
pub fn wire_kind(value: &Value) -> Result<String, JournalError> {
    value
        .get("kind")
        .and_then(Value::as_str)
        .filter(|kind| !kind.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| corrupt("a session event carries no wire discriminant"))
}

/// One stored row, decoded and checked against itself.
pub fn decode(row: SqliteRow) -> Result<PendingRow, JournalError> {
    let client_seq = u64::try_from(row.get::<i64, _>("client_seq"))
        .map_err(|_| corrupt("a client sequence is negative"))?;
    if client_seq == 0 {
        return Err(corrupt("a client sequence is zero"));
    }
    let kind: String = row.get("kind");
    let event_json: String = row.get("event_json");
    let recorded_bytes = u64::try_from(row.get::<i64, _>("payload_bytes"))
        .map_err(|_| corrupt("a payload byte count is negative"))?;
    if recorded_bytes == 0
        || usize::try_from(recorded_bytes).ok() != Some(event_json.len())
    {
        return Err(corrupt(
            "a row's recorded byte count is not the length of its own text",
        ));
    }
    let value: Value =
        serde_json::from_str(&event_json).map_err(|error| corrupt(&error.to_string()))?;
    if wire_kind(&value).ok().as_deref() != Some(kind.as_str()) {
        return Err(corrupt(
            "a row's recorded kind is not the kind of the event it holds",
        ));
    }
    let event = SessionEvent::parse(value)
        .map_err(|error| corrupt(&format!("a stored event no longer decodes: {error}")))?;
    Ok(PendingRow {
        client_seq,
        kind,
        event,
        payload_bytes: usize::try_from(recorded_bytes)
            .map_err(|_| corrupt("a payload byte count is not a count"))?,
    })
}

/// A sequence as the INTEGER column spells it.
pub fn sequence_value(sequence: u64) -> Result<i64, JournalError> {
    i64::try_from(sequence)
        .map_err(|_| corrupt("a client sequence is past what the column can hold"))
}
