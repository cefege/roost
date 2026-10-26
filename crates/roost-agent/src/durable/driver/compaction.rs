//! Manual compaction through the same durable task used by generation.
//! Native slash commands claim an owner lease, import the selected history and
//! await the task receipt; model choice remains a Roost extension policy.

use rpi_ai::types::ModelThinkingLevel;
use rpi_durable::harness::{AgentChange, CompactionResult, ModelRef};
use rpi_durable::types::{TaskOutcome, TaskState};
use rpi_durable::{BACKGROUND_CONTEXT, ROOT_CONVERSATION_ID};

use super::{DurableDriver, session_error};
use crate::error::AgentError;

impl DurableDriver {
    pub async fn compact(&self, instructions: Option<String>) -> Result<bool, AgentError> {
        self.refresh_extension().await?;
        let record = self.runtime.record(&self.conversation_id).await?;
        let model = crate::turn::conversation_model(&self.runtime, &record).await?;
        let entries = self
            .runtime
            .inner
            .store
            .entries(&self.conversation_id)
            .await?;
        self.sync
            .synchronize(&entries, &model.info.provider, &BACKGROUND_CONTEXT)
            .await?;
        let root = self
            .harness
            .conversation(ROOT_CONVERSATION_ID, &BACKGROUND_CONTEXT)
            .await
            .map_err(session_error)?
            .ok_or_else(|| AgentError::Store("Durable root absent".into()))?;
        root.configure(
            AgentChange {
                model: Some(Some(ModelRef {
                    provider: model.info.provider,
                    model_id: model.info.id,
                })),
                thinking_level: Some(Some(ModelThinkingLevel::Off)),
                cwd: Some(Some(record.cwd)),
                ..AgentChange::default()
            },
            &BACKGROUND_CONTEXT,
        )
        .await
        .map_err(session_error)?;
        let task_id = root
            .compact(instructions, &BACKGROUND_CONTEXT)
            .await
            .map_err(session_error)?;
        let settled = self
            .harness
            .wait_for_task(task_id, &BACKGROUND_CONTEXT)
            .await
            .map_err(session_error)?;
        match settled.record.state {
            TaskState::Terminal {
                outcome: TaskOutcome::Completed { result },
            } => {
                let result: CompactionResult = serde_json::from_value(result)
                    .map_err(|error| AgentError::Store(error.to_string()))?;
                let compacted = result.entry_id.is_some() || result.submission_id.is_some();
                if compacted {
                    self.runtime.advisor_reset(&self.conversation_id);
                    tracing::info!(conversation_id = self.conversation_id, %task_id,
                        "native history compacted through Pi Durable");
                }
                Ok(compacted)
            }
            TaskState::Terminal {
                outcome: TaskOutcome::Aborted { .. },
            } => Err(AgentError::Cancelled),
            TaskState::Terminal {
                outcome: TaskOutcome::Failed { error, .. } | TaskOutcome::Faulted { error },
            } => Err(AgentError::Model(error.message)),
            TaskState::Terminal {
                outcome: TaskOutcome::Orphaned { reason },
            } => Err(AgentError::FailedPrecondition(reason)),
            _ => Err(AgentError::Store(
                "Compaction settled without a terminal receipt".into(),
            )),
        }
    }
}
