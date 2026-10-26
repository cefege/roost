//! The SQL authority for scheduled-message rows.
//!
//! `store.rs` forwards the five `AgentStore` schedule methods here, mirroring
//! `store_durable.rs`. No lease fence: any replica may admit a schedule or
//! sweep one due; `advance_schedule`'s `due_ms` compare-and-set is what keeps
//! two replicas racing the same row from double-advancing it.

use roost_agent::error::AgentError;
use roost_agent::schedules::{ScheduleRow, next_due_after};
use sqlx::Row as _;

use super::now_ms;
use crate::db::CoordDb;

fn store_error(error: sqlx::Error) -> AgentError {
    AgentError::Store(error.to_string())
}

pub(super) async fn set_schedule(
    db: &CoordDb,
    conversation_id: &str,
    row: &ScheduleRow,
) -> Result<(), AgentError> {
    sqlx::query(
        "INSERT INTO agent_schedules (conversation_id,id,text,due_ms,every_ms,fired,created_ms) \
         VALUES ($1,$2,$3,$4,$5,$6,$7) \
         ON CONFLICT(conversation_id,id) DO UPDATE SET \
         text=excluded.text, due_ms=excluded.due_ms, every_ms=excluded.every_ms, fired=excluded.fired",
    )
    .bind(conversation_id)
    .bind(&row.id)
    .bind(&row.text)
    .bind(row.due_ms)
    .bind(row.every_ms)
    .bind(i64::from(row.fired))
    .bind(row.created_ms)
    .execute(db.pool())
    .await
    .map_err(store_error)?;
    Ok(())
}

pub(super) async fn schedules(
    db: &CoordDb,
    conversation_id: &str,
) -> Result<Vec<ScheduleRow>, AgentError> {
    let rows = sqlx::query(
        "SELECT id,text,due_ms,every_ms,fired,created_ms FROM agent_schedules \
         WHERE conversation_id=$1 ORDER BY due_ms",
    )
    .bind(conversation_id)
    .fetch_all(db.pool())
    .await
    .map_err(store_error)?;
    rows.iter().map(decode_row).collect()
}

pub(super) async fn due_schedules(
    db: &CoordDb,
    now_ms: i64,
) -> Result<Vec<(String, ScheduleRow)>, AgentError> {
    let rows = sqlx::query(
        "SELECT conversation_id,id,text,due_ms,every_ms,fired,created_ms FROM agent_schedules \
         WHERE due_ms <= $1 ORDER BY due_ms",
    )
    .bind(now_ms)
    .fetch_all(db.pool())
    .await
    .map_err(store_error)?;
    rows.iter()
        .map(|row| {
            let conversation_id: String = row.try_get("conversation_id").map_err(store_error)?;
            Ok((conversation_id, decode_row(row)?))
        })
        .collect()
}

pub(super) async fn advance_schedule(
    db: &CoordDb,
    conversation_id: &str,
    id: &str,
    from_due_ms: i64,
) -> Result<bool, AgentError> {
    let mut tx = db.begin_write().await.map_err(store_error)?;
    let row = sqlx::query(
        "SELECT every_ms,fired FROM agent_schedules \
         WHERE conversation_id=$1 AND id=$2 AND due_ms=$3",
    )
    .bind(conversation_id)
    .bind(id)
    .bind(from_due_ms)
    .fetch_optional(&mut *tx)
    .await
    .map_err(store_error)?;
    let Some(row) = row else {
        tx.commit().await.map_err(store_error)?;
        return Ok(false);
    };
    let every_ms: Option<i64> = row.try_get("every_ms").map_err(store_error)?;
    let fired: i64 = row.try_get("fired").map_err(store_error)?;
    match every_ms {
        None => {
            sqlx::query(
                "DELETE FROM agent_schedules WHERE conversation_id=$1 AND id=$2 AND due_ms=$3",
            )
            .bind(conversation_id)
            .bind(id)
            .bind(from_due_ms)
            .execute(&mut *tx)
            .await
            .map_err(store_error)?;
        }
        Some(step) => {
            let next_due = next_due_after(from_due_ms, step, now_ms());
            sqlx::query(
                "UPDATE agent_schedules SET due_ms=$4, fired=$5 \
                 WHERE conversation_id=$1 AND id=$2 AND due_ms=$3",
            )
            .bind(conversation_id)
            .bind(id)
            .bind(from_due_ms)
            .bind(next_due)
            .bind(fired + 1)
            .execute(&mut *tx)
            .await
            .map_err(store_error)?;
        }
    }
    tx.commit().await.map_err(store_error)?;
    Ok(true)
}

pub(super) async fn remove_schedule(
    db: &CoordDb,
    conversation_id: &str,
    id: &str,
) -> Result<bool, AgentError> {
    let outcome = sqlx::query("DELETE FROM agent_schedules WHERE conversation_id=$1 AND id=$2")
        .bind(conversation_id)
        .bind(id)
        .execute(db.pool())
        .await
        .map_err(store_error)?;
    Ok(outcome.rows_affected() > 0)
}

fn decode_row(row: &sqlx::any::AnyRow) -> Result<ScheduleRow, AgentError> {
    let fired: i64 = row.try_get("fired").map_err(store_error)?;
    Ok(ScheduleRow {
        id: row.try_get("id").map_err(store_error)?,
        text: row.try_get("text").map_err(store_error)?,
        due_ms: row.try_get("due_ms").map_err(store_error)?,
        every_ms: row.try_get("every_ms").map_err(store_error)?,
        fired: u32::try_from(fired).unwrap_or(0),
        created_ms: row.try_get("created_ms").map_err(store_error)?,
    })
}
