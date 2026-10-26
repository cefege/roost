//! The SQL authority for one conversation's durable storage batches and IDs.
//!
//! `store.rs` forwards the three durable `AgentStore` methods here; every
//! mutation shares one write transaction with the lease check and journal.

use roost_agent::durable::{DurableBatch, DurableCommit, StorageWrite};
use roost_agent::error::AgentError;
use roost_agent::records::Entry;
use roost_agent::traits::LeaseFence;
use sqlx::Row as _;

use super::store_execution;
use crate::db::CoordDb;

const MAX_SAFE_ID: u64 = 9_007_199_254_740_991;

pub(super) async fn durable_batches(
    db: &CoordDb,
    id: &str,
) -> Result<Vec<DurableBatch>, AgentError> {
    let rows = sqlx::query(
        "SELECT b.sequence,b.writes_json,b.entry_sequences_json FROM agent_conversations c \
         LEFT JOIN agent_durable_batches b ON b.conversation_id=c.id \
         WHERE c.id=$1 ORDER BY b.sequence",
    )
    .bind(id)
    .fetch_all(db.pool())
    .await
    .map_err(store_error)?;
    if rows.is_empty() {
        return Err(AgentError::NotFound(id.to_owned()));
    }
    let mut batches = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(sequence) = row
            .try_get::<Option<i64>, _>("sequence")
            .map_err(store_error)?
        else {
            continue;
        };
        let writes_json = row
            .try_get::<String, _>("writes_json")
            .map_err(store_error)?;
        let entry_sequences_json = row
            .try_get::<String, _>("entry_sequences_json")
            .map_err(store_error)?;
        batches.push(DurableBatch {
            sequence: u64::try_from(sequence).map_err(serialize_error)?,
            writes: serde_json::from_str(&writes_json).map_err(serialize_error)?,
            entry_sequences: serde_json::from_str(&entry_sequences_json)
                .map_err(serialize_error)?,
        });
    }
    Ok(batches)
}

pub(super) async fn mint_durable_id(
    db: &CoordDb,
    id: &str,
    fence: &LeaseFence,
) -> Result<u64, AgentError> {
    let mut tx = db.begin_write().await.map_err(store_error)?;
    store_execution::require_conversation(&mut tx, id).await?;
    if !store_execution::holds_lease(db.backend(), &mut tx, id, fence).await? {
        return Err(AgentError::Fenced(id.to_owned()));
    }
    let current: Option<i64> =
        sqlx::query_scalar("SELECT next_id FROM agent_durable_ids WHERE conversation_id=$1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(store_error)?;
    let next = current
        .unwrap_or(1)
        .checked_add(1)
        .filter(|id| (2..=MAX_SAFE_ID as i64).contains(id))
        .ok_or_else(|| {
            AgentError::Store(format!("durable ID space exhausted for conversation {id}"))
        })?;
    sqlx::query(
        "INSERT INTO agent_durable_ids (conversation_id,next_id) VALUES ($1,$2) \
         ON CONFLICT(conversation_id) DO UPDATE SET next_id=excluded.next_id",
    )
    .bind(id)
    .bind(next)
    .execute(&mut *tx)
    .await
    .map_err(store_error)?;
    tx.commit().await.map_err(store_error)?;
    u64::try_from(next).map_err(serialize_error)
}

pub(super) async fn append_durable_batch(
    db: &CoordDb,
    id: &str,
    expected_sequence: u64,
    writes: &[StorageWrite],
    entries: &[Entry],
    fence: &LeaseFence,
) -> Result<DurableCommit, AgentError> {
    let writes_json = serde_json::to_string(writes).map_err(serialize_error)?;
    let mut tx = db.begin_write().await.map_err(store_error)?;
    store_execution::require_conversation(&mut tx, id).await?;
    if !store_execution::holds_lease(db.backend(), &mut tx, id, fence).await? {
        return Err(AgentError::Fenced(id.to_owned()));
    }

    let current: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(sequence),0) FROM agent_durable_batches WHERE conversation_id=$1",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .map_err(store_error)?;
    let current = u64::try_from(current).map_err(serialize_error)?;
    if current != expected_sequence {
        return Err(AgentError::Conflict(id.to_owned()));
    }
    let sequence = current
        .checked_add(1)
        .ok_or_else(|| AgentError::Store(format!("durable sequence exhausted for {id}")))?;
    let mut entry_sequences = Vec::with_capacity(entries.len());
    let mut journal_sequence = store_execution::last_seq(&mut tx, id).await?;
    for entry in entries {
        journal_sequence = journal_sequence
            .checked_add(1)
            .ok_or_else(|| AgentError::Store(format!("journal sequence exhausted for {id}")))?;
        let entry_json = serde_json::to_string(entry).map_err(serialize_error)?;
        store_execution::insert_entry(&mut tx, id, journal_sequence, &entry_json).await?;
        entry_sequences.push(journal_sequence);
    }
    let entry_sequences_json = serde_json::to_string(&entry_sequences).map_err(serialize_error)?;

    if let Some(max_id) = writes.iter().filter_map(StorageWrite::write_id).max() {
        if max_id == 0 || max_id > MAX_SAFE_ID {
            return Err(AgentError::InvalidArgument(format!(
                "durable ID {max_id} is outside the JavaScript safe integer range"
            )));
        }
        let max_id = store_execution::to_i64(max_id)?;
        let stored: Option<i64> =
            sqlx::query_scalar("SELECT next_id FROM agent_durable_ids WHERE conversation_id=$1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(store_error)?;
        if stored.is_none_or(|stored| stored < max_id) {
            sqlx::query(
                "INSERT INTO agent_durable_ids (conversation_id,next_id) VALUES ($1,$2) \
                 ON CONFLICT(conversation_id) DO UPDATE SET next_id=excluded.next_id",
            )
            .bind(id)
            .bind(max_id)
            .execute(&mut *tx)
            .await
            .map_err(store_error)?;
        }
    }
    sqlx::query(
        "INSERT INTO agent_durable_batches (conversation_id,sequence,writes_json,entry_sequences_json,created_ms) VALUES ($1,$2,$3,$4,$5)",
    )
    .bind(id)
    .bind(store_execution::to_i64(sequence)?)
    .bind(writes_json)
    .bind(entry_sequences_json)
    .bind(super::now_ms())
    .execute(&mut *tx)
    .await
    .map_err(store_error)?;
    tx.commit().await.map_err(store_error)?;
    Ok(DurableCommit {
        sequence,
        entry_sequences,
    })
}

fn store_error(error: sqlx::Error) -> AgentError {
    AgentError::Store(error.to_string())
}

fn serialize_error(error: impl std::fmt::Display) -> AgentError {
    AgentError::Store(error.to_string())
}
