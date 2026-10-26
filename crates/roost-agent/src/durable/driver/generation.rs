//! The native run's entry into the durable generation task.
//! Admission remains a host command boundary; model/tool execution and restart
//! state come exclusively from the reusable kernel's built-in task definitions.

use std::sync::PoisonError;

use rpi_ai::types::ModelThinkingLevel;
use rpi_durable::harness::{AgentChange, LIVE_DOC, LiveState, ModelRef, start_run};
use rpi_durable::session::{DocOwner, commit_fn};
use rpi_durable::types::{ScanOrder, TaskOutcome, TaskState, TaskStatus};
use rpi_durable::{BACKGROUND_CONTEXT, ROOT_CONVERSATION_ID, Storage, TaskQuery};
use tokio_util::sync::CancellationToken;

use super::{DurableDriver, session_error};
use crate::error::AgentError;
use crate::model_call::ModelOutput;
use crate::records::Entry;
use crate::roles::ResolvedModel;
use crate::runtime::RunEnd;
use crate::turn::{RunMemory, TurnOutcome};

impl DurableDriver {
    pub async fn generate(
        &self,
        memory: &mut RunMemory,
        cancel: &CancellationToken,
    ) -> Result<TurnOutcome, AgentError> {
        if cancel.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        self.refresh_extension().await?;
        let record = self.runtime.record(&self.conversation_id).await?;
        let chain = crate::turn::conversation_chain(&self.runtime, &record).await?;
        let candidate = chain.first().ok_or_else(|| {
            AgentError::FailedPrecondition("No model candidates are available".into())
        })?;
        let Some(claimed) = crate::turn::dispatch::claim_dispatch_inputs(
            &self.runtime,
            &self.conversation_id,
            cancel,
            &candidate.info.provider,
        )
        .await?
        else {
            return Ok(TurnOutcome::NoWork);
        };
        if claimed.new_submission {
            memory.new_submission();
        }
        let thinking = crate::turn::resolve_thinking(
            &self.runtime,
            &record,
            candidate,
            &claimed.entries,
            memory,
        )
        .await;
        {
            let mut metadata = self
                .projection
                .metadata
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            metadata.completed = false;
            metadata.stop_reason = None;
            metadata.input_through_seq = claimed.through_seq;
            metadata.continued = memory.continued;
            metadata.yield_reminders = memory.yield_reminders;
            metadata.goal_id = super::super::state::active_goal_id(&claimed.entries);
        }
        self.sync
            .synchronize(
                &claimed.entries,
                &candidate.info.provider,
                &BACKGROUND_CONTEXT,
            )
            .await?;
        let root = self
            .harness
            .conversation(ROOT_CONVERSATION_ID, &BACKGROUND_CONTEXT)
            .await
            .map_err(session_error)?
            .ok_or_else(|| AgentError::Store("Durable root absent".into()))?;
        let thinking = serde_json::from_value::<ModelThinkingLevel>(serde_json::json!(thinking))
            .map_err(|error| AgentError::InvalidArgument(error.to_string()))?;
        if candidate
            .thinking
            .as_deref()
            .unwrap_or(&record.thinking_level)
            == "auto"
        {
            self.projection
                .auto_levels
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(crate::journal::current_checkpoint(&claimed.entries))
                .or_insert(thinking);
        }
        root.configure(
            AgentChange {
                model: Some(Some(ModelRef {
                    provider: candidate.info.provider.clone(),
                    model_id: candidate.info.id.clone(),
                })),
                thinking_level: Some(Some(thinking)),
                cwd: Some(Some(record.cwd)),
                ..AgentChange::default()
            },
            &BACKGROUND_CONTEXT,
        )
        .await
        .map_err(session_error)?;
        root.commit(
            commit_fn(|tx| {
                Box::pin(async move {
                    let live = tx
                        .doc(
                            &LIVE_DOC,
                            DocOwner::Conversation(ROOT_CONVERSATION_ID),
                            None,
                        )
                        .await?;
                    let value = live.read()?;
                    let state: LiveState = serde_json::from_value(value).map_err(|error| {
                        rpi_durable::session::SessionError::message(error.to_string())
                    })?;
                    if state.run.is_none() {
                        start_run(tx, ROOT_CONVERSATION_ID, &live, Vec::new()).await?;
                    }
                    Ok(())
                })
            }),
            &BACKGROUND_CONTEXT,
        )
        .await
        .map_err(session_error)?;
        let settled = tokio::select! {
            () = cancel.cancelled() => {
                let latest = self.storage.scan_tasks(TaskQuery {
                    conversation_id: Some(ROOT_CONVERSATION_ID),
                    kind: Some(rpi_durable::harness::GENERATION_TASK_NAME.into()),
                    order: Some(ScanOrder::Descending), ..TaskQuery::default()
                }, 1, None, &BACKGROUND_CONTEXT).await
                    .map_err(|error| AgentError::Store(error.to_string()))?;
                if latest.items.first().is_some_and(|task| task.status() == TaskStatus::Sleeping) {
                    root.abort(&BACKGROUND_CONTEXT,
                        rpi_durable::harness::ConversationAbortOptions { background: true })
                        .await.map_err(session_error)?;
                }
                root.wait_for_idle(&BACKGROUND_CONTEXT).await
            },
            settled = root.wait_for_idle(&BACKGROUND_CONTEXT) => settled,
        };
        settled.map_err(session_error)?;
        {
            let metadata = self
                .projection
                .metadata
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            memory.continued = metadata.continued;
            memory.yield_reminders = metadata.yield_reminders;
        }
        let end = self
            .projection
            .end
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let latest = self
            .storage
            .scan_tasks(
                TaskQuery {
                    conversation_id: Some(ROOT_CONVERSATION_ID),
                    kind: Some(rpi_durable::harness::GENERATION_TASK_NAME.into()),
                    order: Some(ScanOrder::Descending),
                    ..TaskQuery::default()
                },
                1,
                None,
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|error| AgentError::Store(error.to_string()))?;
        if let Some(task) = latest.items.first() {
            match &task.state {
                TaskState::Terminal {
                    outcome: TaskOutcome::Failed { error, .. } | TaskOutcome::Faulted { error },
                } => return Err(AgentError::Model(error.message.clone())),
                TaskState::Terminal {
                    outcome: TaskOutcome::Aborted { .. },
                } => return Err(AgentError::Cancelled),
                TaskState::Terminal {
                    outcome: TaskOutcome::Orphaned { reason },
                } => return Err(AgentError::FailedPrecondition(reason.clone())),
                _ => {}
            }
        }
        let entries = self
            .runtime
            .inner
            .store
            .entries(&self.conversation_id)
            .await?;
        let response = entries
            .iter()
            .rev()
            .find_map(|(_, entry)| match entry {
                Entry::Assistant {
                    blocks,
                    usage,
                    model,
                    metadata,
                    ..
                } => Some((blocks, usage, model, metadata)),
                _ => None,
            })
            .ok_or_else(|| {
                AgentError::Model("Durable run ended without an assistant response".into())
            })?;
        let (blocks, usage, model, metadata) = response;
        let metadata = metadata
            .as_ref()
            .ok_or_else(|| AgentError::Store("Durable response metadata absent".into()))?;
        if !metadata.completed {
            return Err(match end {
                Some(RunEnd::Aborted) => AgentError::Cancelled,
                Some(RunEnd::Failed(message)) => AgentError::Model(message),
                _ => AgentError::Model("Durable model request did not complete".into()),
            });
        }
        let output = ModelOutput {
            blocks: blocks.clone(),
            usage: *usage,
            stop: metadata
                .stop_reason
                .clone()
                .ok_or_else(|| AgentError::Store("Durable stop reason absent".into()))?,
        };
        if !output.tool_calls().is_empty() {
            let yielded =
                recover_yield_receipt(self.storage.as_ref(), latest.items.first()).await?;
            return Ok(TurnOutcome::Tools {
                end: Some(yielded.map_or(RunEnd::Completed, RunEnd::Yielded)),
            });
        }
        let model = model
            .as_ref()
            .and_then(|reference| {
                self.runtime
                    .catalog()
                    .get(&reference.provider, &reference.model_id)
            })
            .cloned()
            .ok_or_else(|| {
                AgentError::Model("Durable response model absent from catalog".into())
            })?;
        Ok(TurnOutcome::Text {
            output: Box::new(output),
            model: Box::new(ResolvedModel {
                info: model,
                thinking: None,
            }),
        })
    }
}
async fn recover_yield_receipt(
    storage: &dyn Storage,
    generation: Option<&rpi_durable::TaskRecord>,
) -> Result<Option<String>, AgentError> {
    let Some(rpi_durable::TaskRecord {
        state:
            TaskState::Terminal {
                outcome: TaskOutcome::Completed { result },
            },
        ..
    }) = generation
    else {
        return Ok(None);
    };
    let Some(assistant_id) = result.get("entryId").and_then(serde_json::Value::as_u64) else {
        return Ok(None);
    };
    let mut cursor = None;
    loop {
        let page = storage
            .scan_tasks(
                TaskQuery {
                    conversation_id: Some(ROOT_CONVERSATION_ID),
                    kind: Some(rpi_durable::harness::TOOL_TASK_NAME.into()),
                    order: Some(ScanOrder::Descending),
                    ..TaskQuery::default()
                },
                128,
                cursor,
                &BACKGROUND_CONTEXT,
            )
            .await
            .map_err(|error| AgentError::Store(error.to_string()))?;
        for task in page.items {
            if task
                .input
                .get("assistant")
                .and_then(serde_json::Value::as_u64)
                != Some(assistant_id)
            {
                continue;
            }
            if let TaskState::Terminal {
                outcome: TaskOutcome::Completed { result },
            } = task.state
                && let Some(report) = result
                    .get("control")
                    .and_then(|control| control.get("handoff"))
                    .and_then(serde_json::Value::as_str)
            {
                return Ok(Some(report.to_owned()));
            }
        }
        cursor = page.next;
        if cursor.is_none() {
            return Ok(None);
        }
    }
}
