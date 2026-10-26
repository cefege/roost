//! Selects Roost's configured side-task model chain for durable summaries.
//! The shared compaction task owns summarization and persistence; this hook
//! only pins the first available Roost model before the request is captured.

use std::sync::Arc;

use async_trait::async_trait;
use rpi_durable::Context;
use rpi_durable::harness::types::{
    BeforeCompact, BeforeCompactResult, CompactionHooks, HookApi, ModelRef,
};
use rpi_durable::session::{SessionError, SessionResult};

use crate::runtime::AgentRuntime;

pub(crate) fn hooks(runtime: AgentRuntime, conversation_id: String) -> Arc<dyn CompactionHooks> {
    Arc::new(RoostCompactionHooks {
        runtime,
        conversation_id,
    })
}

struct RoostCompactionHooks {
    runtime: AgentRuntime,
    conversation_id: String,
}

#[async_trait]
impl CompactionHooks for RoostCompactionHooks {
    async fn select_model(
        &self,
        _current: ModelRef,
        _context: &Context,
    ) -> SessionResult<Option<ModelRef>> {
        let record = self
            .runtime
            .record(&self.conversation_id)
            .await
            .map_err(|error| SessionError::message(error.to_string()))?;
        let candidates = crate::turn::side_task_models(&self.runtime, &record)
            .await
            .map_err(|error| SessionError::message(error.to_string()))?;
        Ok(candidates.into_iter().next().map(|model| ModelRef {
            provider: model.provider,
            model_id: model.id,
        }))
    }

    async fn before_compact(
        &self,
        _compaction: BeforeCompact,
        _api: &dyn HookApi,
        _context: &Context,
    ) -> Option<BeforeCompactResult> {
        None
    }
}
