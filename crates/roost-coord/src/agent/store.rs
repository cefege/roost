//! Durable harness conversations, transcript entries, and settings in `CoordDb`.
//!
//! The runtime calls this through `roost_agent::AgentStore`; rows stay private
//! to the coordinator and JSON payloads use the shared harness record types.
//! Admission, leases and fenced appends live in `store_execution.rs`.

use futures::future::BoxFuture;
use roost_agent::error::AgentError;
use roost_agent::records::{AgentSettings, ConversationRecord, Entry};
use roost_agent::traits::{AdmitOutcome, AgentStore, CommandAdmission, LeaseFence};
use roost_protocol::wire::agent_chat::ModelRef;
use sqlx::Row as _;
use tokio_util::sync::CancellationToken;

use crate::db::CoordDb;

#[path = "store_core.rs"]
mod store_core;
#[path = "store_durable.rs"]
mod store_durable;
#[path = "store_execution.rs"]
mod store_execution;
#[path = "store_rows.rs"]
mod store_rows;
#[path = "store_schedules.rs"]
mod store_schedules;

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
        Box::pin(store_core::conversations(&self.db))
    }

    fn conversation<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<ConversationRecord>, AgentError>> {
        Box::pin(store_core::conversation(&self.db, id))
    }

    fn save_conversation<'a>(
        &'a self,
        record: &'a ConversationRecord,
        expected_revision: u64,
    ) -> BoxFuture<'a, Result<u64, AgentError>> {
        Box::pin(store_core::save_conversation(
            &self.db,
            record,
            expected_revision,
        ))
    }

    fn delete_conversation<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), AgentError>> {
        Box::pin(store_core::delete_conversation(&self.db, id))
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

    fn durable_batches<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<roost_agent::durable::DurableBatch>, AgentError>> {
        Box::pin(store_durable::durable_batches(&self.db, id))
    }

    fn mint_durable_id<'a>(
        &'a self,
        id: &'a str,
        fence: &'a LeaseFence,
    ) -> BoxFuture<'a, Result<u64, AgentError>> {
        Box::pin(store_durable::mint_durable_id(&self.db, id, fence))
    }

    fn append_durable_batch<'a>(
        &'a self,
        id: &'a str,
        expected_sequence: u64,
        writes: &'a [roost_agent::durable::StorageWrite],
        entries: &'a [Entry],
        fence: &'a LeaseFence,
    ) -> BoxFuture<'a, Result<roost_agent::durable::DurableCommit, AgentError>> {
        Box::pin(store_durable::append_durable_batch(
            &self.db,
            id,
            expected_sequence,
            writes,
            entries,
            fence,
        ))
    }

    fn orphaned_runs(&self, now_ms: i64) -> BoxFuture<'_, Result<Vec<String>, AgentError>> {
        Box::pin(store_execution::orphaned_runs(&self.db, now_ms))
    }

    fn entries<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Vec<(u64, Entry)>, AgentError>> {
        Box::pin(store_core::entries(&self.db, id))
    }

    fn settings(&self) -> BoxFuture<'_, Result<AgentSettings, AgentError>> {
        Box::pin(store_core::settings(&self.db))
    }

    fn save_settings<'a>(
        &'a self,
        settings: &'a AgentSettings,
    ) -> BoxFuture<'a, Result<(), AgentError>> {
        Box::pin(store_core::save_settings(&self.db, settings))
    }

    fn advisor_state<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, AgentError>> {
        Box::pin(store_core::advisor_state(&self.db, id))
    }

    fn save_advisor_state<'a>(
        &'a self,
        id: &'a str,
        state_json: &'a str,
    ) -> BoxFuture<'a, Result<(), AgentError>> {
        Box::pin(store_core::save_advisor_state(&self.db, id, state_json))
    }

    fn set_schedule<'a>(
        &'a self,
        conversation_id: &'a str,
        row: &'a roost_agent::schedules::ScheduleRow,
    ) -> BoxFuture<'a, Result<(), AgentError>> {
        Box::pin(store_schedules::set_schedule(
            &self.db,
            conversation_id,
            row,
        ))
    }

    fn schedules<'a>(
        &'a self,
        conversation_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<roost_agent::schedules::ScheduleRow>, AgentError>> {
        Box::pin(store_schedules::schedules(&self.db, conversation_id))
    }

    fn due_schedules(
        &self,
        now_ms: i64,
    ) -> BoxFuture<'_, Result<Vec<(String, roost_agent::schedules::ScheduleRow)>, AgentError>> {
        Box::pin(store_schedules::due_schedules(&self.db, now_ms))
    }

    fn advance_schedule<'a>(
        &'a self,
        conversation_id: &'a str,
        id: &'a str,
        from_due_ms: i64,
    ) -> BoxFuture<'a, Result<bool, AgentError>> {
        Box::pin(store_schedules::advance_schedule(
            &self.db,
            conversation_id,
            id,
            from_due_ms,
        ))
    }

    fn remove_schedule<'a>(
        &'a self,
        conversation_id: &'a str,
        id: &'a str,
    ) -> BoxFuture<'a, Result<bool, AgentError>> {
        Box::pin(store_schedules::remove_schedule(
            &self.db,
            conversation_id,
            id,
        ))
    }
}

/// Models conversations were explicitly switched to, most recently active first.
pub async fn recent_models(db: &CoordDb, limit: i64) -> Result<Vec<ModelRef>, AgentError> {
    let rows = sqlx::query(
        "SELECT model_provider, model_id FROM agent_conversations \
         WHERE model_provider IS NOT NULL AND model_id IS NOT NULL \
         AND model_provider <> '' AND model_id <> '' \
         GROUP BY model_provider, model_id ORDER BY MAX(updated_ms) DESC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(db.pool())
    .await
    .map_err(store_error)?;
    rows.iter()
        .map(|row| {
            Ok(ModelRef {
                provider: row
                    .try_get::<String, _>("model_provider")
                    .map_err(store_error)?,
                model_id: row.try_get::<String, _>("model_id").map_err(store_error)?,
            })
        })
        .collect()
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
