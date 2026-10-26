//! Roost's existing agent tools as registrations in the durable Harness.
//!
//! The coordinator builds the extension for one conversation; durable owns
//! task execution and result persistence while each callback reuses `run_call`.

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use rpi_ai::types::{TextContent, Tool, ToolCall, ToolResultContent};
use rpi_durable::chord::context::Context;
use rpi_durable::chord::json::JsonObject;
use rpi_durable::harness::{
    Extension, HookHandlers, HookRegistration, ToolControl, ToolExecute, ToolExecuteError,
    ToolExecutionApi, ToolExecutionMode, ToolExecutionResult, ToolRegistration, ToolReplay,
};
use serde_json::{Map, Value};

use super::cancellation::InvocationCancellation;
use crate::error::AgentError;
use crate::runtime::{AgentRuntime, RunEnd};
use crate::tool_round::{CallScope, run_call};
use crate::toolset::toolset_for;

/// Build the conversation's current model-facing tools for durable execution.
pub(crate) async fn extension(
    runtime: AgentRuntime,
    conversation_id: String,
    end: Arc<Mutex<Option<RunEnd>>>,
) -> Result<Arc<Extension>, AgentError> {
    let record = runtime.record(&conversation_id).await?;
    let depth = crate::turn::depth_of(&runtime, &record).await;
    let find_available = crate::find::available(&runtime, &conversation_id).await;
    let toolset = toolset_for(&record, depth, find_available);
    let mut tools: Vec<ToolRegistration> = toolset
        .specs
        .into_iter()
        .map(|spec| {
            let execution_mode = if crate::toolset::is_read_only(&spec.name) {
                None
            } else {
                Some(ToolExecutionMode::Sequential)
            };
            let execute = Arc::new(RoostTool {
                runtime: runtime.clone(),
                conversation_id: conversation_id.clone(),
                name: spec.name.clone(),
                end: Arc::clone(&end),
            });
            ToolRegistration {
                tool: Tool {
                    name: spec.name,
                    description: spec.description,
                    parameters: spec.parameters,
                    constrained_sampling: None,
                },
                replay: ToolReplay::Unsafe,
                execution_mode,
                prepare_arguments: None,
                deferred: false,
                output_limits: None,
                structured_output_schema: None,
                callers: None,
                execute,
            }
        })
        .collect();
    let codemode = rpi_durable::harness::codemode::create_codemode_extension();
    tools.extend(codemode.tools);
    let hooks = vec![HookRegistration {
        task: rpi_durable::harness::TOOL_TASK_NAME.into(),
        handlers: HookHandlers::Tool(Arc::new(RoostRecoveryHooks {
            runtime: runtime.clone(),
            conversation_id: conversation_id.clone(),
        })),
    }];
    let mut sections = crate::durable::prompts::sections(runtime, conversation_id);
    sections.extend(codemode.sections);
    Ok(Arc::new(Extension {
        name: "roost-agent".into(),
        tools,
        sections,
        hooks,
        ..Extension::default()
    }))
}

struct RoostTool {
    runtime: AgentRuntime,
    conversation_id: String,
    name: String,
    end: Arc<Mutex<Option<RunEnd>>>,
}

#[async_trait]
impl ToolExecute for RoostTool {
    async fn execute(
        &self,
        args: JsonObject,
        api: &dyn ToolExecutionApi,
        context: &Context,
    ) -> Result<ToolExecutionResult, ToolExecuteError> {
        let args =
            serde_json::to_string(&args).map_err(|error| Box::new(error) as ToolExecuteError)?;
        let core_cancellation = context
            .abort_signal()
            .map(|signal| signal.token())
            .unwrap_or_default();
        let cancellation =
            InvocationCancellation::new(&self.runtime, &self.conversation_id, core_cancellation);
        let record = self
            .runtime
            .record(&self.conversation_id)
            .await
            .map_err(|error| Box::new(error) as ToolExecuteError)?;
        let entries = self
            .runtime
            .inner
            .store
            .entries(&self.conversation_id)
            .await
            .map_err(|error| Box::new(error) as ToolExecuteError)?;
        let checkpoint_id = crate::journal::current_checkpoint(&entries);
        let entries = crate::journal::effective_entries(&entries);
        let depth = crate::turn::depth_of(&self.runtime, &record).await;
        let find_available = crate::find::available(&self.runtime, &self.conversation_id).await;
        let toolset = toolset_for(&record, depth, find_available);
        let goal_id = entries.iter().rev().find_map(|(_, entry)| match entry {
            crate::records::Entry::Goal { state } => Some(state.id.clone()),
            _ => None,
        });
        let output_api = api.shared();
        let execution_id = format!("durable-tool-{}", api.task_id());
        let scope = CallScope {
            runtime: &self.runtime,
            record: &record,
            toolset: &toolset,
            depth,
            goal_id,
            checkpoint_id,
            execution_id: &execution_id,
            cancel: cancellation.token(),
            durable_output: Arc::new(move |bytes| output_api.output(bytes, None)),
        };
        let result = run_call(&scope, api.call_id(), &self.name, &args).await;
        let control = result.end.as_ref().map(|run_end| {
            let mut stored_end = self.end.lock().unwrap_or_else(PoisonError::into_inner);
            if stored_end.is_none() {
                *stored_end = Some(run_end.clone());
            }
            ToolControl {
                terminate: Some(true),
                handoff: match run_end {
                    RunEnd::Yielded(report) => Some(report.clone()),
                    _ => None,
                },
                add_tools: None,
            }
        });
        Ok(ToolExecutionResult {
            content: Some(vec![ToolResultContent::Text(TextContent {
                text: result.outcome.content,
                text_signature: None,
            })]),
            is_error: Some(result.outcome.is_error),
            details: serde_json::from_str::<Value>(&result.outcome.details_json).ok(),
            control,
            ..ToolExecutionResult::default()
        })
    }
}
struct RoostRecoveryHooks {
    runtime: AgentRuntime,
    conversation_id: String,
}

#[async_trait]
impl rpi_durable::harness::types::ToolHooks for RoostRecoveryHooks {
    async fn before_tool(
        &self,
        _call: &ToolCall,
        _api: &dyn rpi_durable::harness::HookApi,
        _context: &Context,
    ) -> Option<rpi_durable::harness::types::BeforeToolResult> {
        None
    }

    async fn after_tool(
        &self,
        _call: &ToolCall,
        _result: ToolExecutionResult,
        _api: &dyn rpi_durable::harness::HookApi,
        _context: &Context,
    ) -> Option<ToolExecutionResult> {
        None
    }

    async fn on_interrupted(
        &self,
        call: &ToolCall,
        mut result: ToolExecutionResult,
        _api: &dyn rpi_durable::harness::HookApi,
        _context: &Context,
    ) -> Option<ToolExecutionResult> {
        let interrupted = result.diagnostics.as_ref().is_some_and(|diagnostics| {
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.as_deref() == Some("interrupted"))
        });
        if call.name != crate::toolset::TOOL_TASK || result.is_error != Some(true) || !interrupted {
            return None;
        }
        let (children, lookup_error) = match self.runtime.inner.store.conversations().await {
            Ok(records) => (
                records
                    .into_iter()
                    .filter(|record| record.is_child_of(&self.conversation_id))
                    .map(|record| record.id)
                    .collect::<Vec<_>>(),
                None,
            ),
            Err(error) => {
                tracing::warn!(
                    conversation_id = self.conversation_id,
                    %error,
                    "could not list child conversations after task interruption"
                );
                (Vec::new(), Some(error.to_string()))
            }
        };
        let child_report = match (&lookup_error, children.as_slice()) {
            (Some(error), _) => {
                format!("Existing child conversations could not be inspected: {error}.")
            }
            (None, []) => "No existing child conversations were found.".to_owned(),
            (None, children) => format!(
                "Existing child conversations (do not respawn): {}",
                children.join(", ")
            ),
        };
        let guidance = format!(
            "The task was interrupted before its result was durably recorded. Its side effects \
             are unknown; do not respawn it blindly. Inspect its existing conversation state, \
             then continue the same task from its recorded progress.\n{child_report}"
        );
        result
            .content
            .get_or_insert_with(Vec::new)
            .push(ToolResultContent::Text(TextContent {
                text: guidance,
                text_signature: None,
            }));
        let mut details = match result.details.take() {
            Some(Value::Object(details)) => details,
            Some(previous) => {
                let mut details = Map::new();
                details.insert("previous_details".to_owned(), previous);
                details
            }
            None => Map::new(),
        };
        details.insert("interrupted".to_owned(), Value::Bool(true));
        details.insert("outcome_unknown".to_owned(), Value::Bool(true));
        details.insert(
            "children".to_owned(),
            Value::Array(children.into_iter().map(Value::String).collect()),
        );
        if let Some(error) = lookup_error {
            details.insert("children_lookup_error".to_owned(), Value::String(error));
        }
        result.details = Some(Value::Object(details));
        result.is_error = Some(true);
        Some(result)
    }
}
