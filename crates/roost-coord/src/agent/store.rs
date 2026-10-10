//! Durable harness conversations, transcript entries, and settings in `CoordDb`.
//!
//! The runtime calls this through `roost_agent::AgentStore`; rows stay private
//! to the coordinator and JSON payloads use the shared harness record types.

use futures::future::BoxFuture;
use roost_agent::error::AgentError;
use roost_agent::records::{AgentSettings, ConversationRecord, Entry};
use roost_agent::traits::AgentStore;
use sqlx::Row as _;

use crate::db::CoordDb;

#[path = "store_rows.rs"]
mod store_rows;
use store_rows::decode_record;

#[derive(Debug, Clone)]
pub struct CoordAgentStore {
    db: CoordDb,
}

impl CoordAgentStore {
    #[must_use]
    pub fn new(db: CoordDb) -> Self {
        Self { db }
    }
}

impl AgentStore for CoordAgentStore {
    fn conversations(&self) -> BoxFuture<'_, Result<Vec<ConversationRecord>, AgentError>> {
        Box::pin(async move {
            let rows =
                sqlx::query("SELECT * FROM agent_conversations ORDER BY updated_ms DESC, id")
                    .fetch_all(self.db.pool())
                    .await
                    .map_err(store_error)?;
            rows.iter()
                .map(|row| decode_record(row, self.db.backend()))
                .collect()
        })
    }

    fn conversation<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<ConversationRecord>, AgentError>> {
        Box::pin(async move {
            let row = sqlx::query("SELECT * FROM agent_conversations WHERE id = $1")
                .bind(id)
                .fetch_optional(self.db.pool())
                .await
                .map_err(store_error)?;
            row.as_ref()
                .map(|row| decode_record(row, self.db.backend()))
                .transpose()
        })
    }

    fn save_conversation<'a>(
        &'a self,
        record: &'a ConversationRecord,
    ) -> BoxFuture<'a, Result<(), AgentError>> {
        Box::pin(async move {
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
            sqlx::query("INSERT INTO agent_conversations \
                (id,title,worker_fp,worker_label,worker_os,cwd,model_provider,model_id,thinking_level,mode,pre_plan_model,parent_id,agent,advisor,run_state,error,created_ms,updated_ms) \
                VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18) \
                ON CONFLICT(id) DO UPDATE SET title=excluded.title,worker_fp=excluded.worker_fp,worker_label=excluded.worker_label,worker_os=excluded.worker_os,cwd=excluded.cwd,model_provider=excluded.model_provider,model_id=excluded.model_id,thinking_level=excluded.thinking_level,mode=excluded.mode,pre_plan_model=excluded.pre_plan_model,parent_id=excluded.parent_id,agent=excluded.agent,advisor=excluded.advisor,run_state=excluded.run_state,error=excluded.error,created_ms=excluded.created_ms,updated_ms=excluded.updated_ms")
                .bind(&record.id).bind(&record.title).bind(&record.worker_fp).bind(&record.worker_label)
                .bind(&record.worker_os).bind(&record.cwd).bind(provider).bind(model_id)
                .bind(&record.thinking_level).bind(record.mode.as_str()).bind(pre_plan)
                .bind(&record.parent_id).bind(&record.agent).bind(record.advisor)
                .bind(record.run_state.as_str()).bind(&record.error).bind(record.created_ms).bind(record.updated_ms)
                .execute(self.db.pool()).await.map_err(store_error)?;
            Ok(())
        })
    }

    fn delete_conversation<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), AgentError>> {
        Box::pin(async move {
            let mut tx = self.db.begin_write().await.map_err(store_error)?;
            sqlx::query("WITH RECURSIVE descendants(id) AS (SELECT id FROM agent_conversations WHERE id=$1 UNION SELECT c.id FROM agent_conversations c JOIN descendants d ON c.parent_id=d.id) DELETE FROM agent_entries WHERE conversation_id IN (SELECT id FROM descendants)")
                .bind(id).execute(&mut *tx).await.map_err(store_error)?;
            sqlx::query("WITH RECURSIVE descendants(id) AS (SELECT id FROM agent_conversations WHERE id=$1 UNION SELECT c.id FROM agent_conversations c JOIN descendants d ON c.parent_id=d.id) DELETE FROM agent_conversations WHERE id IN (SELECT id FROM descendants)")
                .bind(id).execute(&mut *tx).await.map_err(store_error)?;
            tx.commit().await.map_err(store_error)
        })
    }

    fn append_entry<'a>(
        &'a self,
        id: &'a str,
        entry: &'a Entry,
    ) -> BoxFuture<'a, Result<u64, AgentError>> {
        Box::pin(async move {
            let json = serde_json::to_string(entry).map_err(serialize_error)?;
            let mut tx = self.db.begin_write().await.map_err(store_error)?;
            let conversation_count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM agent_conversations WHERE id=$1")
                    .bind(id)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(store_error)?;
            if conversation_count == 0 {
                return Err(AgentError::NotFound(id.to_owned()));
            }
            let previous: i64 = sqlx::query_scalar(
                "SELECT COALESCE(MAX(seq), 0) FROM agent_entries WHERE conversation_id=$1",
            )
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(store_error)?;
            let seq = previous
                .checked_add(1)
                .ok_or_else(|| AgentError::Store("entry sequence overflow".to_owned()))?;
            sqlx::query("INSERT INTO agent_entries (conversation_id,seq,entry_json,created_ms) VALUES ($1,$2,$3,$4)")
                .bind(id).bind(seq).bind(json).bind(now_ms()).execute(&mut *tx).await.map_err(store_error)?;
            tx.commit().await.map_err(store_error)?;
            u64::try_from(seq).map_err(|error| AgentError::Store(error.to_string()))
        })
    }

    fn entries<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Vec<(u64, Entry)>, AgentError>> {
        Box::pin(async move {
            let rows = sqlx::query(
                "SELECT seq,entry_json FROM agent_entries WHERE conversation_id=$1 ORDER BY seq",
            )
            .bind(id)
            .fetch_all(self.db.pool())
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
        })
    }

    fn settings(&self) -> BoxFuture<'_, Result<AgentSettings, AgentError>> {
        Box::pin(async move {
            let value: Option<String> =
                sqlx::query_scalar("SELECT value_json FROM agent_settings WHERE key='settings'")
                    .fetch_optional(self.db.pool())
                    .await
                    .map_err(store_error)?;
            value.map_or_else(
                || Ok(AgentSettings::default()),
                |json| serde_json::from_str(&json).map_err(serialize_error),
            )
        })
    }

    fn save_settings<'a>(
        &'a self,
        settings: &'a AgentSettings,
    ) -> BoxFuture<'a, Result<(), AgentError>> {
        Box::pin(async move {
            let json = serde_json::to_string(settings).map_err(serialize_error)?;
            sqlx::query("INSERT INTO agent_settings(key,value_json) VALUES('settings',$1) ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json")
                .bind(json).execute(self.db.pool()).await.map_err(store_error)?;
            Ok(())
        })
    }
}

fn now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(i64::MAX)
}

fn store_error(error: sqlx::Error) -> AgentError {
    AgentError::Store(error.to_string())
}
fn serialize_error(error: impl std::fmt::Display) -> AgentError {
    AgentError::Store(error.to_string())
}
