//! The SQL authority for conversation rows, their entry journal, settings,
//! and advisor state. `store.rs` forwards these `AgentStore` methods here,
//! mirroring `store_durable.rs`, `store_execution.rs` and `store_schedules.rs`.

use roost_agent::error::AgentError;
use roost_agent::records::{AgentSettings, ConversationRecord, Entry};
use sqlx::Row as _;

use super::store_rows::decode_record;
use crate::db::CoordDb;

fn store_error(error: sqlx::Error) -> AgentError {
    AgentError::Store(error.to_string())
}
fn serialize_error(error: impl std::fmt::Display) -> AgentError {
    AgentError::Store(error.to_string())
}

pub(super) async fn conversations(db: &CoordDb) -> Result<Vec<ConversationRecord>, AgentError> {
    let rows = sqlx::query("SELECT * FROM agent_conversations ORDER BY updated_ms DESC, id")
        .fetch_all(db.pool())
        .await
        .map_err(store_error)?;
    rows.iter()
        .map(|row| decode_record(row, db.backend()))
        .collect()
}

pub(super) async fn conversation(
    db: &CoordDb,
    id: &str,
) -> Result<Option<ConversationRecord>, AgentError> {
    let row = sqlx::query("SELECT * FROM agent_conversations WHERE id = $1")
        .bind(id)
        .fetch_optional(db.pool())
        .await
        .map_err(store_error)?;
    row.as_ref()
        .map(|row| decode_record(row, db.backend()))
        .transpose()
}

pub(super) async fn save_conversation(
    db: &CoordDb,
    record: &ConversationRecord,
    expected_revision: u64,
) -> Result<u64, AgentError> {
    let (provider, model_id) = record
        .model
        .as_ref()
        .map(|model| (Some(model.provider.as_str()), Some(model.model_id.as_str())))
        .unwrap_or((None, None));
    let pre_plan = record
        .pre_plan_model
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(serialize_error)?;
    let next = expected_revision.saturating_add(1);
    let next_i64 = i64::try_from(next).map_err(serialize_error)?;
    let expected_i64 = i64::try_from(expected_revision).map_err(serialize_error)?;
    let statement = if expected_revision == 0 {
        sqlx::query(
            "INSERT INTO agent_conversations \
            (id,title,worker_fp,worker_label,worker_os,cwd,model_provider,model_id,thinking_level,mode,pre_plan_model,parent_id,agent,advisor,read_only,run_state,error,created_ms,updated_ms,revision) \
            VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20) \
            ON CONFLICT(id) DO NOTHING",
        )
    } else {
        sqlx::query(
            "UPDATE agent_conversations SET \
            title=$2,worker_fp=$3,worker_label=$4,worker_os=$5,cwd=$6,model_provider=$7,model_id=$8,thinking_level=$9,mode=$10,pre_plan_model=$11,parent_id=$12,agent=$13,advisor=$14,read_only=$15,run_state=$16,error=$17,created_ms=$18,updated_ms=$19,revision=$20 \
            WHERE id=$1 AND revision=$21",
        )
    };
    let written = statement
        .bind(&record.id)
        .bind(&record.title)
        .bind(&record.worker_fp)
        .bind(&record.worker_label)
        .bind(&record.worker_os)
        .bind(&record.cwd)
        .bind(provider)
        .bind(model_id)
        .bind(&record.thinking_level)
        .bind(record.mode.as_str())
        .bind(pre_plan)
        .bind(&record.parent_id)
        .bind(&record.agent)
        .bind(record.advisor)
        .bind(record.read_only)
        .bind(record.run_state.as_str())
        .bind(&record.error)
        .bind(record.created_ms)
        .bind(record.updated_ms)
        .bind(next_i64);
    let written = if expected_revision == 0 {
        written
    } else {
        written.bind(expected_i64)
    };
    let outcome = written.execute(db.pool()).await.map_err(store_error)?;
    if outcome.rows_affected() == 1 {
        return Ok(next);
    }
    if expected_revision != 0 && conversation(db, &record.id).await?.is_none() {
        return Err(AgentError::NotFound(record.id.clone()));
    }
    Err(AgentError::Conflict(record.id.clone()))
}

pub(super) async fn delete_conversation(db: &CoordDb, id: &str) -> Result<(), AgentError> {
    let mut tx = db.begin_write().await.map_err(store_error)?;
    sqlx::query("WITH RECURSIVE descendants(id) AS (SELECT id FROM agent_conversations WHERE id=$1 UNION SELECT c.id FROM agent_conversations c JOIN descendants d ON c.parent_id=d.id) DELETE FROM agent_entries WHERE conversation_id IN (SELECT id FROM descendants)")
        .bind(id).execute(&mut *tx).await.map_err(store_error)?;
    sqlx::query("WITH RECURSIVE descendants(id) AS (SELECT id FROM agent_conversations WHERE id=$1 UNION SELECT c.id FROM agent_conversations c JOIN descendants d ON c.parent_id=d.id) DELETE FROM agent_conversations WHERE id IN (SELECT id FROM descendants)")
        .bind(id).execute(&mut *tx).await.map_err(store_error)?;
    tx.commit().await.map_err(store_error)
}

pub(super) async fn entries(db: &CoordDb, id: &str) -> Result<Vec<(u64, Entry)>, AgentError> {
    let rows = sqlx::query(
        "SELECT seq,entry_json FROM agent_entries WHERE conversation_id=$1 ORDER BY seq",
    )
    .bind(id)
    .fetch_all(db.pool())
    .await
    .map_err(store_error)?;
    rows.iter()
        .map(|row| {
            let seq: i64 = row.try_get("seq").map_err(store_error)?;
            let json: String = row.try_get("entry_json").map_err(store_error)?;
            let entry = serde_json::from_str(&json).map_err(serialize_error)?;
            Ok((
                u64::try_from(seq).map_err(|error| AgentError::Store(error.to_string()))?,
                entry,
            ))
        })
        .collect()
}

pub(super) async fn settings(db: &CoordDb) -> Result<AgentSettings, AgentError> {
    let value: Option<String> =
        sqlx::query_scalar("SELECT value_json FROM agent_settings WHERE key='settings'")
            .fetch_optional(db.pool())
            .await
            .map_err(store_error)?;
    value.map_or_else(
        || Ok(AgentSettings::default()),
        |json| serde_json::from_str(&json).map_err(serialize_error),
    )
}

pub(super) async fn save_settings(
    db: &CoordDb,
    settings: &AgentSettings,
) -> Result<(), AgentError> {
    let json = serde_json::to_string(settings).map_err(serialize_error)?;
    sqlx::query("INSERT INTO agent_settings(key,value_json) VALUES('settings',$1) ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json")
        .bind(json).execute(db.pool()).await.map_err(store_error)?;
    Ok(())
}

pub(super) async fn advisor_state(db: &CoordDb, id: &str) -> Result<Option<String>, AgentError> {
    // The empty roster slot owns the single advisor; named slots stay inactive.
    sqlx::query_scalar(
        "SELECT state_json FROM agent_advisor_state WHERE conversation_id=$1 AND roster_slug=''",
    )
    .bind(id)
    .fetch_optional(db.pool())
    .await
    .map_err(store_error)
}

pub(super) async fn save_advisor_state(
    db: &CoordDb,
    id: &str,
    state_json: &str,
) -> Result<(), AgentError> {
    sqlx::query("INSERT INTO agent_advisor_state(conversation_id,roster_slug,state_json,updated_ms) VALUES($1,'',$2,$3) ON CONFLICT(conversation_id,roster_slug) DO UPDATE SET state_json=excluded.state_json,updated_ms=excluded.updated_ms")
        .bind(id)
        .bind(state_json)
        .bind(super::now_ms())
        .execute(db.pool())
        .await
        .map_err(store_error)?;
    Ok(())
}
