//! Durable harness conversations, transcript entries, and settings in `CoordDb`.
//!
//! The runtime calls this through `roost_agent::AgentStore`; rows stay private
//! to the coordinator and JSON payloads use the shared harness record types.
//! Admission, leases and fenced appends live in `store_execution.rs`.

use futures::future::BoxFuture;
use roost_agent::error::AgentError;
use roost_agent::records::{AgentSettings, ConversationRecord, Entry};
use roost_agent::traits::{AdmitOutcome, AgentStore, CommandAdmission, LeaseFence};
use sqlx::Row as _;
use tokio_util::sync::CancellationToken;

use crate::db::CoordDb;

#[path = "store_execution.rs"]
mod store_execution;
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
        expected_revision: u64,
    ) -> BoxFuture<'a, Result<u64, AgentError>> {
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
            let next = expected_revision.saturating_add(1);
            let next_i64 = i64::try_from(next).map_err(serialize_error)?;
            let expected_i64 = i64::try_from(expected_revision).map_err(serialize_error)?;
            let statement = if expected_revision == 0 {
                sqlx::query(
                    "INSERT INTO agent_conversations \
                    (id,title,worker_fp,worker_label,worker_os,cwd,model_provider,model_id,thinking_level,mode,pre_plan_model,parent_id,agent,advisor,run_state,error,created_ms,updated_ms,revision) \
                    VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19) \
                    ON CONFLICT(id) DO NOTHING",
                )
            } else {
                sqlx::query(
                    "UPDATE agent_conversations SET \
                    title=$2,worker_fp=$3,worker_label=$4,worker_os=$5,cwd=$6,model_provider=$7,model_id=$8,thinking_level=$9,mode=$10,pre_plan_model=$11,parent_id=$12,agent=$13,advisor=$14,run_state=$15,error=$16,created_ms=$17,updated_ms=$18,revision=$19 \
                    WHERE id=$1 AND revision=$20",
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
            let outcome = written.execute(self.db.pool()).await.map_err(store_error)?;
            if outcome.rows_affected() == 1 {
                return Ok(next);
            }
            if expected_revision != 0 && self.conversation(&record.id).await?.is_none() {
                return Err(AgentError::NotFound(record.id.clone()));
            }
            Err(AgentError::Conflict(record.id.clone()))
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
        cancellation: &'a [&'a CancellationToken],
        fence: Option<&'a LeaseFence>,
    ) -> BoxFuture<'a, Result<u64, AgentError>> {
        Box::pin(store_execution::append_entry(
            &self.db,
            id,
            entry,
            cancellation,
            fence,
        ))
    }

    fn admit_command<'a>(
        &'a self,
        id: &'a str,
        admission: CommandAdmission,
    ) -> BoxFuture<'a, Result<AdmitOutcome, AgentError>> {
        Box::pin(store_execution::admit_command(&self.db, id, admission))
    }

    fn complete_command<'a>(
        &'a self,
        id: &'a str,
        request_id: &'a str,
        result_json: &'a str,
    ) -> BoxFuture<'a, Result<(), AgentError>> {
        Box::pin(store_execution::complete_command(
            &self.db,
            id,
            request_id,
            result_json,
        ))
    }

    fn acquire_lease<'a>(
        &'a self,
        id: &'a str,
        owner: &'a str,
        ttl_ms: u64,
    ) -> BoxFuture<'a, Result<Option<u64>, AgentError>> {
        Box::pin(store_execution::acquire_lease(&self.db, id, owner, ttl_ms))
    }

    fn renew_lease<'a>(
        &'a self,
        id: &'a str,
        fence: &'a LeaseFence,
        ttl_ms: u64,
    ) -> BoxFuture<'a, Result<bool, AgentError>> {
        Box::pin(store_execution::renew_lease(&self.db, id, fence, ttl_ms))
    }

    fn release_lease<'a>(
        &'a self,
        id: &'a str,
        fence: &'a LeaseFence,
    ) -> BoxFuture<'a, Result<(), AgentError>> {
        Box::pin(store_execution::release_lease(&self.db, id, fence))
    }

    fn orphaned_runs(&self, now_ms: i64) -> BoxFuture<'_, Result<Vec<String>, AgentError>> {
        Box::pin(store_execution::orphaned_runs(&self.db, now_ms))
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
    fn advisor_state<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, AgentError>> {
        Box::pin(async move {
            sqlx::query_scalar(
                "SELECT state_json FROM agent_advisor_state WHERE conversation_id=$1",
            )
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
            .map_err(store_error)
        })
    }

    fn save_advisor_state<'a>(
        &'a self,
        id: &'a str,
        state_json: &'a str,
    ) -> BoxFuture<'a, Result<(), AgentError>> {
        Box::pin(async move {
            sqlx::query("INSERT INTO agent_advisor_state(conversation_id,state_json,updated_ms) VALUES($1,$2,$3) ON CONFLICT(conversation_id) DO UPDATE SET state_json=excluded.state_json,updated_ms=excluded.updated_ms")
                .bind(id)
                .bind(state_json)
                .bind(now_ms())
                .execute(self.db.pool())
                .await
                .map_err(store_error)?;
            Ok(())
        })
    }
}

pub(super) fn now_ms() -> i64 {
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
