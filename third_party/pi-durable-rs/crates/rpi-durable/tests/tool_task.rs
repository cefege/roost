//! Tool task integration tests (port of `packages/durable/test/harness-tools`
//! cases @ `abe508e1b`): validation failure, an executed result entry, and
//! diagnostics.

mod generation_support;

use std::sync::Arc;

use async_trait::async_trait;
use rpi_ai::types::{Message, Tool as AiTool, ToolCall, ToolResultContent};
use rpi_durable::chord::context::Context;
use rpi_durable::entries::{ASSISTANT_ENTRY, TOOL_RESULT_ENTRY};
use rpi_durable::harness::registry::TOOL_TASK_NAME;
use rpi_durable::harness::types::{DiagnosticSeverity, ToolExecuteError};
use rpi_durable::harness::{
    Extension, ToolDiagnostic, ToolExecute, ToolExecutionApi, ToolExecutionResult,
    ToolRegistration, ToolReplay,
};
use rpi_durable::ids::{ConversationId, TaskId};
use rpi_durable::session::{EntryDraft, TaskCreate, TaskOwnership, commit_fn};
use rpi_durable::types::{EntryRecord, ScanOrder, Storage};

use generation_support::{Opened, context, open, open_with_storage, test_models};

/// A tool whose `execute` reports output and one diagnostic.
struct ReportingTool;

#[async_trait]
impl ToolExecute for ReportingTool {
    async fn execute(
        &self,
        _args: rpi_durable::chord::json::JsonObject,
        api: &dyn ToolExecutionApi,
        _context: &Context,
    ) -> Result<ToolExecutionResult, ToolExecuteError> {
        api.output(b"hello output", None).expect("output");
        api.diagnostic(ToolDiagnostic {
            severity: DiagnosticSeverity::Warn,
            message: "note".to_string(),
            code: Some("note".to_string()),
        });
        Ok(ToolExecutionResult::default())
    }
}

fn tool_extension(name: &str, parameters: serde_json::Value) -> Extension {
    Extension {
        name: format!("ext-{name}"),
        tools: vec![ToolRegistration {
            tool: AiTool {
                name: name.to_string(),
                description: format!("{name} tool"),
                parameters,
                constrained_sampling: None,
            },
            replay: ToolReplay::Unsafe,
            execution_mode: None,
            deferred: false,
            prepare_arguments: None,
            output_limits: None,
            structured_output_schema: None,
            callers: None,
            execute: Arc::new(ReportingTool),
        }],
        ..Extension::default()
    }
}

fn assistant_with_call(call: &ToolCall) -> Message {
    let content = serde_json::json!({
        "type": "toolCall",
        "id": call.id,
        "name": call.name,
        "arguments": call.arguments,
    });
    let value = serde_json::json!({
        "role": "assistant",
        "content": [content],
        "api": "openai",
        "provider": "test",
        "model": "test-model",
        "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
            "cost": { "input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.0 } },
        "stopReason": "toolUse",
        "timestamp": 1
    });
    Message::Assistant(serde_json::from_value(value).expect("assistant"))
}

/// Append an assistant call and create its conversation-owned `pi.tool` task.
async fn create_tool_task(
    opened: &Opened,
    conversation_id: ConversationId,
    call: ToolCall,
) -> TaskId {
    create_tool_task_with_checkpoint(
        opened,
        conversation_id,
        call,
        serde_json::json!({ "phase": "call" }),
    )
    .await
}

async fn create_tool_task_with_checkpoint(
    opened: &Opened,
    conversation_id: ConversationId,
    call: ToolCall,
    checkpoint: serde_json::Value,
) -> TaskId {
    let change = commit_fn(move |tx| {
        let call = call.clone();
        let checkpoint = checkpoint.clone();
        Box::pin(async move {
            let mut draft = EntryDraft::new(ASSISTANT_ENTRY);
            draft.model = Some(vec![assistant_with_call(&call)]);
            let entry = tx.append_entry(conversation_id, draft).await?;
            tx.create_task(TaskCreate {
                kind: TOOL_TASK_NAME.to_string(),
                version: 1,
                input: serde_json::json!({ "assistant": entry.id, "callId": call.id }),
                checkpoint,
                ownership: TaskOwnership::Conversation(conversation_id),
                conversation_id: Some(conversation_id),
                background: false,
            })
            .await
        })
    });
    opened
        .harness
        .session()
        .commit(change, &context())
        .await
        .expect("create tool task")
}

/// The one `pi.tool-result` entry's `(isError, text, data, durationMs)`.
fn tool_result(entries: &[EntryRecord]) -> (bool, String, serde_json::Value, Option<u64>) {
    let entry = entries
        .iter()
        .find(|entry| entry.kind == TOOL_RESULT_ENTRY)
        .expect("tool result entry");
    for message in entry.model.iter().flatten() {
        if let Message::ToolResult(result) = message {
            let text = result
                .content
                .iter()
                .filter_map(|content| match content {
                    ToolResultContent::Text(text) => Some(text.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            return (
                result.is_error,
                text,
                entry.data.clone().unwrap_or(serde_json::Value::Null),
                result.duration_ms,
            );
        }
    }
    panic!("tool result message missing");
}

/// upstream: "an executed tool appends its result entry with output and diagnostics".
#[tokio::test]
async fn execute_appends_result_with_output_and_diagnostics() {
    let opened: Opened = open(
        vec![tool_extension(
            "echo",
            serde_json::json!({ "type": "object", "properties": {} }),
        )],
        None,
        test_models(Vec::new()),
    )
    .await;
    let conversation = opened
        .harness
        .root(&context(), Default::default())
        .await
        .expect("root");
    let call = ToolCall {
        id: "c1".to_string(),
        name: "echo".to_string(),
        arguments: serde_json::Map::new(),
        thought_signature: None,
        namespace: None,
    };
    let task_id = create_tool_task(&opened, conversation.id(), call).await;
    let settled = opened
        .harness
        .wait_for_task(task_id, &context())
        .await
        .expect("wait");
    assert!(matches!(
        settled.record.state,
        rpi_durable::types::TaskState::Terminal { .. }
    ));
    let entries = conversation_entries(&opened, conversation.id()).await;
    let (is_error, text, data, duration_ms) = tool_result(&entries);
    assert!(!is_error);
    assert!(text.contains("hello output"), "got {text}");
    assert!(text.contains("[warn] note"), "got {text}");
    assert!(
        duration_ms.is_some(),
        "the tool task records the execution duration"
    );
    assert_eq!(
        data.get("diagnostics")
            .and_then(|diagnostics| diagnostics.get(0))
            .and_then(|diagnostic| diagnostic.get("code")),
        Some(&serde_json::json!("note"))
    );
    assert!(entries.iter().any(|entry| entry.kind == ASSISTANT_ENTRY));
}

/// upstream: "invalid arguments complete with an invalid_arguments result".
#[tokio::test]
async fn validation_failure_result() {
    let opened = open(
        vec![tool_extension(
            "strict",
            serde_json::json!({
                "type": "object",
                "properties": { "value": { "type": "string" } },
                "required": ["value"]
            }),
        )],
        None,
        test_models(Vec::new()),
    )
    .await;
    let conversation = opened
        .harness
        .root(&context(), Default::default())
        .await
        .expect("root");
    let call = ToolCall {
        id: "c2".to_string(),
        name: "strict".to_string(),
        arguments: serde_json::Map::new(),
        thought_signature: None,
        namespace: None,
    };
    let task_id = create_tool_task(&opened, conversation.id(), call).await;
    let settled = opened
        .harness
        .wait_for_task(task_id, &context())
        .await
        .expect("wait");
    // A tool result with `isError` still completes.
    assert!(matches!(
        settled.record.state,
        rpi_durable::types::TaskState::Terminal { .. }
    ));
    let entries = conversation_entries(&opened, conversation.id()).await;
    let (is_error, text, data, _) = tool_result(&entries);
    assert!(is_error);
    assert!(text.contains("Validation failed"), "got {text}");
    assert_eq!(
        data.get("diagnostics")
            .and_then(|diagnostics| diagnostics.get(0))
            .and_then(|diagnostic| diagnostic.get("code")),
        Some(&serde_json::json!("invalid_arguments"))
    );
}

struct FailingSideEffect(Arc<std::sync::atomic::AtomicUsize>);

#[async_trait]
impl ToolExecute for FailingSideEffect {
    async fn execute(
        &self,
        _args: rpi_durable::chord::json::JsonObject,
        api: &dyn ToolExecutionApi,
        _context: &Context,
    ) -> Result<ToolExecutionResult, ToolExecuteError> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        api.output(b"side effect started", None)
            .expect("report output");
        Err(Box::new(std::io::Error::other(
            "interrupted after the side effect",
        )))
    }
}

struct ReplayingNestedCall;

#[async_trait]
impl ToolExecute for ReplayingNestedCall {
    async fn execute(
        &self,
        _args: rpi_durable::chord::json::JsonObject,
        api: &dyn ToolExecutionApi,
        context: &Context,
    ) -> Result<ToolExecutionResult, ToolExecuteError> {
        let options = rpi_durable::harness::types::NestedToolOptions {
            key: Some("unsafe-effect".to_string()),
            progress: Some(false),
        };
        let first = api
            .execute_tool(
                "side_effect",
                serde_json::Map::new(),
                options.clone(),
                context,
            )
            .await
            .map_err(|error| {
                Box::new(std::io::Error::other(error.to_string())) as ToolExecuteError
            })?;
        let replay = api
            .execute_tool("side_effect", serde_json::Map::new(), options, context)
            .await
            .map_err(|error| {
                Box::new(std::io::Error::other(error.to_string())) as ToolExecuteError
            })?;
        if first != replay
            || !replay.is_error
            || replay.structured_output != Some(serde_json::json!("side effect started"))
        {
            return Err(Box::new(std::io::Error::other(
                "nested call did not replay its recorded error output",
            )));
        }
        Ok(ToolExecutionResult::default())
    }
}

#[tokio::test]
async fn keyed_nested_unsafe_error_reuses_the_durable_child_and_progress_false_keeps_status_only_slot()
 {
    use rpi_durable::harness::types::ToolCaller;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let effects = Arc::new(AtomicUsize::new(0));
    let mut parent_extension = tool_extension("compose", serde_json::json!({ "type": "object" }));
    let mut parent = parent_extension.tools.remove(0);
    parent.callers = Some(vec![ToolCaller::Model]);
    parent.execute = Arc::new(ReplayingNestedCall);
    let mut effect_extension =
        tool_extension("side_effect", serde_json::json!({ "type": "object" }));
    let mut effect = effect_extension.tools.remove(0);
    effect.callers = Some(vec![ToolCaller::Tool]);
    effect.execute = Arc::new(FailingSideEffect(Arc::clone(&effects)));
    let extension = Extension {
        name: "nested".to_string(),
        tools: vec![parent, effect],
        ..Extension::default()
    };
    let opened = open(vec![extension], None, test_models(Vec::new())).await;
    let conversation = opened
        .harness
        .root(&context(), Default::default())
        .await
        .expect("root");
    let call = ToolCall {
        id: "compose-call".to_string(),
        name: "compose".to_string(),
        arguments: serde_json::Map::new(),
        thought_signature: None,
        namespace: None,
    };
    let task_id = create_tool_task(&opened, conversation.id(), call).await;
    opened
        .harness
        .wait_for_task(task_id, &context())
        .await
        .expect("wait");
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let live = opened
        .harness
        .session()
        .snapshot(
            &rpi_durable::harness::LIVE_DOC,
            rpi_durable::session::DocOwner::Conversation(conversation.id()),
            &context(),
        )
        .await
        .expect("live snapshot")
        .unwrap_or_default();
    let slots = live
        .get("nestedTools")
        .and_then(serde_json::Value::as_array)
        .expect("status slot");
    assert_eq!(slots.len(), 1);
    assert_eq!(slots[0].get("status"), Some(&serde_json::json!("done")));
    assert!(slots[0].get("output").is_none());
    assert!(slots[0].get("details").is_none());
    assert!(slots[0].get("diagnostics").is_none());
    let entries = conversation_entries(&opened, conversation.id()).await;
    let result = entries
        .iter()
        .filter_map(|entry| entry.model.as_ref())
        .flatten()
        .find_map(|message| match message {
            Message::ToolResult(result) if result.tool_call_id == "compose-call" => Some(result),
            _ => None,
        })
        .expect("parent result");
    let nested = result.nested_calls.as_ref().expect("nested call record");
    assert!(nested.complete);
    assert_eq!(nested.calls.len(), 1);
    assert_eq!(nested.calls[0].status, "error");
    assert_eq!(nested.calls[0].arguments, Some(serde_json::json!({})));
}

struct NestedOutputCheck;

#[async_trait]
impl ToolExecute for NestedOutputCheck {
    async fn execute(
        &self,
        _args: rpi_durable::chord::json::JsonObject,
        api: &dyn ToolExecutionApi,
        context: &Context,
    ) -> Result<ToolExecutionResult, ToolExecuteError> {
        let result = api
            .execute_tool(
                "reader",
                serde_json::Map::new(),
                rpi_durable::harness::types::NestedToolOptions::default(),
                context,
            )
            .await
            .map_err(|error| {
                Box::new(std::io::Error::other(error.to_string())) as ToolExecuteError
            })?;
        if result.structured_output != Some(serde_json::json!("hello output")) {
            return Err(Box::new(std::io::Error::other(format!(
                "schema-less nested output was {:?}",
                result.structured_output
            ))));
        }
        Ok(ToolExecutionResult::default())
    }
}

#[tokio::test]
async fn schema_less_nested_tool_returns_its_bounded_text_output() {
    use rpi_durable::harness::types::ToolCaller;

    let mut parent_extension =
        tool_extension("compose_output", serde_json::json!({ "type": "object" }));
    let mut parent = parent_extension.tools.remove(0);
    parent.callers = Some(vec![ToolCaller::Model]);
    parent.execute = Arc::new(NestedOutputCheck);
    let mut reader_extension = tool_extension("reader", serde_json::json!({ "type": "object" }));
    let mut reader = reader_extension.tools.remove(0);
    reader.callers = Some(vec![ToolCaller::Tool]);
    let extension = Extension {
        name: "nested-output".to_string(),
        tools: vec![parent, reader],
        ..Extension::default()
    };
    let opened = open(vec![extension], None, test_models(Vec::new())).await;
    let conversation = opened
        .harness
        .root(&context(), Default::default())
        .await
        .expect("root");
    let task_id = create_tool_task(
        &opened,
        conversation.id(),
        ToolCall {
            id: "compose-output-call".to_string(),
            name: "compose_output".to_string(),
            arguments: serde_json::Map::new(),
            thought_signature: None,
            namespace: None,
        },
    )
    .await;
    let settled = opened
        .harness
        .wait_for_task(task_id, &context())
        .await
        .expect("wait");
    assert!(matches!(
        settled.record.state,
        rpi_durable::types::TaskState::Terminal {
            outcome: rpi_durable::types::TaskOutcome::Completed { .. },
            ..
        }
    ));
}

struct StructuredCounter(Arc<std::sync::atomic::AtomicUsize>);

#[async_trait]
impl ToolExecute for StructuredCounter {
    async fn execute(
        &self,
        _args: rpi_durable::chord::json::JsonObject,
        _api: &dyn ToolExecutionApi,
        _context: &Context,
    ) -> Result<ToolExecutionResult, ToolExecuteError> {
        let count = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        Ok(ToolExecutionResult {
            structured_output: Some(serde_json::json!({ "count": count })),
            ..ToolExecutionResult::default()
        })
    }
}

#[tokio::test]
async fn codemode_runs_javascript_and_composes_a_nested_tool() {
    use rpi_durable::harness::types::ToolCaller;

    let effects = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut counter_extension = tool_extension("counter", serde_json::json!({ "type": "object" }));
    let mut counter = counter_extension.tools.remove(0);
    counter.callers = Some(vec![ToolCaller::Tool]);
    counter.structured_output_schema = Some(serde_json::json!({
        "type": "object",
        "properties": { "count": { "type": "integer" } },
        "required": ["count"],
        "additionalProperties": false
    }));
    counter.execute = Arc::new(StructuredCounter(Arc::clone(&effects)));
    let mut extension = rpi_durable::harness::codemode::create_codemode_extension();
    extension.name = "codemode-test".to_string();
    extension.tools.push(counter);
    let opened = open(vec![extension], None, test_models(Vec::new())).await;
    let conversation = opened
        .harness
        .root(&context(), Default::default())
        .await
        .expect("root");
    let call = ToolCall {
        id: "codemode-call".to_string(),
        name: "codemode".to_string(),
        arguments: serde_json::json!({ "code": "return await tools.counter({});" })
            .as_object()
            .cloned()
            .unwrap_or_default(),
        thought_signature: None,
        namespace: None,
    };
    let task_id = create_tool_task(&opened, conversation.id(), call).await;
    opened
        .harness
        .wait_for_task(task_id, &context())
        .await
        .expect("wait");
    assert_eq!(effects.load(std::sync::atomic::Ordering::SeqCst), 1);
    let entries = conversation_entries(&opened, conversation.id()).await;
    let (is_error, text, _, _) = tool_result(&entries);
    assert!(!is_error, "codemode failed: {text}");
    assert!(text.contains("\"count\":1"), "got {text}");
}
struct InterruptedExecution(Arc<std::sync::atomic::AtomicUsize>);

#[async_trait]
impl ToolExecute for InterruptedExecution {
    async fn execute(
        &self,
        _args: rpi_durable::chord::json::JsonObject,
        _api: &dyn ToolExecutionApi,
        _context: &Context,
    ) -> Result<ToolExecutionResult, ToolExecuteError> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(ToolExecutionResult::default())
    }
}

struct RecoveryHookCounts {
    interrupted: Arc<std::sync::atomic::AtomicUsize>,
    completed: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait]
impl rpi_durable::harness::types::ToolHooks for RecoveryHookCounts {
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
        self.completed
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        None
    }

    async fn on_interrupted(
        &self,
        _call: &ToolCall,
        mut result: ToolExecutionResult,
        _api: &dyn rpi_durable::harness::HookApi,
        _context: &Context,
    ) -> Option<ToolExecutionResult> {
        self.interrupted
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        result.details = Some(serde_json::json!({ "recovery_hook": true }));
        Some(result)
    }
}

#[tokio::test]
async fn unsafe_interruption_uses_recovery_hook_without_replaying_tool_or_after_tool() {
    let executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let interrupted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let completed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut extension = tool_extension("echo", serde_json::json!({ "type": "object" }));
    extension.tools[0].execute = Arc::new(InterruptedExecution(Arc::clone(&executions)));
    extension
        .hooks
        .push(rpi_durable::harness::HookRegistration {
            task: TOOL_TASK_NAME.to_string(),
            handlers: rpi_durable::harness::HookHandlers::Tool(Arc::new(RecoveryHookCounts {
                interrupted: Arc::clone(&interrupted),
                completed: Arc::clone(&completed),
            })),
        });
    let opened = open(vec![extension], None, test_models(Vec::new())).await;
    let conversation = opened
        .harness
        .root(&context(), Default::default())
        .await
        .expect("root");
    let call = ToolCall {
        id: "interrupted-call".to_string(),
        name: "echo".to_string(),
        arguments: serde_json::Map::new(),
        thought_signature: None,
        namespace: None,
    };
    let task_id = create_tool_task_with_checkpoint(
        &opened,
        conversation.id(),
        call,
        serde_json::json!({
            "phase": "execute",
            "arguments": {},
            "replay": "unsafe"
        }),
    )
    .await;
    opened
        .harness
        .wait_for_task(task_id, &context())
        .await
        .expect("wait");
    assert_eq!(executions.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(interrupted.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(completed.load(std::sync::atomic::Ordering::SeqCst), 0);
    let entries = conversation_entries(&opened, conversation.id()).await;
    let (is_error, text, _, _) = tool_result(&entries);
    assert!(is_error);
    assert!(text.contains("was interrupted"), "got {text}");
    let result = entries
        .iter()
        .find(|entry| entry.kind == TOOL_RESULT_ENTRY)
        .and_then(|entry| entry.model.as_ref())
        .into_iter()
        .flatten()
        .find_map(|message| match message {
            Message::ToolResult(result) => Some(result),
            _ => None,
        })
        .expect("tool result message");
    assert_eq!(
        result
            .details
            .as_ref()
            .and_then(|details| details.get("recovery_hook")),
        Some(&serde_json::json!(true))
    );
}
async fn conversation_entries(
    opened: &Opened,
    conversation_id: ConversationId,
) -> Vec<EntryRecord> {
    let storage: &Arc<dyn Storage> = &opened.storage;
    storage
        .scan_entries(
            rpi_durable::types::EntryQuery {
                conversation_id,
                min_entry_id: None,
                max_entry_id: None,
                order: Some(ScanOrder::Ascending),
            },
            200,
            None,
            &context(),
        )
        .await
        .expect("scan")
        .items
}
struct SignalComposition {
    signal: Arc<tokio::sync::Notify>,
    waits: bool,
}

#[async_trait]
impl ToolExecute for SignalComposition {
    async fn execute(
        &self,
        _args: rpi_durable::chord::json::JsonObject,
        _api: &dyn ToolExecutionApi,
        _context: &Context,
    ) -> Result<ToolExecutionResult, ToolExecuteError> {
        if self.waits {
            self.signal.notified().await;
            Ok(ToolExecutionResult {
                structured_output: Some(serde_json::json!("waited")),
                ..ToolExecutionResult::default()
            })
        } else {
            self.signal.notify_one();
            Ok(ToolExecutionResult {
                structured_output: Some(serde_json::json!("signaled")),
                ..ToolExecutionResult::default()
            })
        }
    }
}

#[tokio::test]
async fn codemode_promise_all_runs_nested_tools_concurrently() {
    use rpi_durable::harness::types::ToolCaller;

    let signal = Arc::new(tokio::sync::Notify::new());
    let mut wait_extension = tool_extension("wait_signal", serde_json::json!({ "type": "object" }));
    let mut wait_tool = wait_extension.tools.remove(0);
    wait_tool.callers = Some(vec![ToolCaller::Tool]);
    wait_tool.structured_output_schema = Some(serde_json::json!({ "type": "string" }));
    wait_tool.execute = Arc::new(SignalComposition {
        signal: Arc::clone(&signal),
        waits: true,
    });

    let mut signal_extension =
        tool_extension("send_signal", serde_json::json!({ "type": "object" }));
    let mut signal_tool = signal_extension.tools.remove(0);
    signal_tool.callers = Some(vec![ToolCaller::Tool]);
    signal_tool.structured_output_schema = Some(serde_json::json!({ "type": "string" }));
    signal_tool.execute = Arc::new(SignalComposition {
        signal,
        waits: false,
    });

    let mut extension = rpi_durable::harness::codemode::create_codemode_extension();
    extension.name = "codemode-concurrent-test".to_string();
    extension.tools.extend([wait_tool, signal_tool]);
    let opened = open(vec![extension], None, test_models(Vec::new())).await;
    let conversation = opened
        .harness
        .root(&context(), Default::default())
        .await
        .expect("root");
    let call = ToolCall {
        id: "codemode-concurrent-call".to_string(),
        name: "codemode".to_string(),
        arguments: serde_json::json!({
            "code": "return await Promise.all([tools.wait_signal({}), tools.send_signal({})]);"
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
        thought_signature: None,
        namespace: None,
    };
    let task_id = create_tool_task(&opened, conversation.id(), call).await;
    opened
        .harness
        .wait_for_task(task_id, &context())
        .await
        .expect("wait");
    let entries = conversation_entries(&opened, conversation.id()).await;
    let (is_error, text, _, _) = tool_result(&entries);
    assert!(!is_error, "codemode failed: {text}");
    assert!(text.contains("[\"waited\",\"signaled\"]"), "got {text}");
}

struct PanickingNestedTool(Arc<std::sync::atomic::AtomicUsize>);

#[async_trait]
impl ToolExecute for PanickingNestedTool {
    async fn execute(
        &self,
        _args: rpi_durable::chord::json::JsonObject,
        _api: &dyn ToolExecutionApi,
        _context: &Context,
    ) -> Result<ToolExecutionResult, ToolExecuteError> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::panic::panic_any("callback exploded");
    }
}

#[tokio::test]
async fn codemode_nested_tool_panic_is_a_terminal_error_result() {
    use rpi_durable::harness::types::ToolCaller;

    let callback_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut extension = rpi_durable::harness::codemode::create_codemode_extension();
    extension.name = "codemode-panic-test".to_string();
    let mut panic_extension = tool_extension("panic_tool", serde_json::json!({ "type": "object" }));
    let mut panic_tool = panic_extension.tools.remove(0);
    panic_tool.callers = Some(vec![ToolCaller::Tool]);
    panic_tool.execute = Arc::new(PanickingNestedTool(Arc::clone(&callback_count)));
    extension.tools.push(panic_tool);

    let opened = open(vec![extension], None, test_models(Vec::new())).await;
    let conversation = opened
        .harness
        .root(&context(), Default::default())
        .await
        .expect("root");
    let task_id = create_tool_task(&opened, conversation.id(), ToolCall {
        id: "codemode-panic-call".to_string(),
        name: "codemode".to_string(),
        arguments: serde_json::json!({
            "code": "try { await tools.panic_tool({}); } catch (error) { return error.message; }"
        }).as_object().cloned().unwrap_or_default(),
        thought_signature: None,
        namespace: None,
    }).await;
    opened
        .harness
        .wait_for_task(task_id, &context())
        .await
        .expect("wait");

    let entries = conversation_entries(&opened, conversation.id()).await;
    let (is_error, text, _, _) = tool_result(&entries);
    assert!(!is_error, "the parent code-mode task failed: {text}");
    assert!(text.contains("callback exploded"), "got {text}");
    assert_eq!(callback_count.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn codemode_reports_script_errors_and_preserves_typed_image_output() {
    let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
    let mut extension = rpi_durable::harness::codemode::create_codemode_extension();
    extension.name = "codemode-output-test".to_string();
    let opened = open(vec![extension], None, test_models(Vec::new())).await;
    let conversation = opened
        .harness
        .root(&context(), Default::default())
        .await
        .expect("root");
    let call = ToolCall {
        id: "codemode-output-call".to_string(),
        name: "codemode".to_string(),
        arguments: serde_json::json!({
            "code": format!(
                "store('saved', 4); store('deleted', 9); store('deleted', undefined); text(load('saved')); text(load('deleted')); text(String(Date.now() === new Date().getTime())); console.log('visible console', load('saved')); image('data:image/png;base64,{png}'); image({{ image_url: 'data:image/png;base64,{png}' }}); image({{ type: 'image', data: '{png}', mimeType: 'image/gif' }}); exit('ignored return');"
            )
        }).as_object().cloned().unwrap_or_default(),
        thought_signature: None,
        namespace: None,
    };
    let task_id = create_tool_task(&opened, conversation.id(), call).await;
    opened
        .harness
        .wait_for_task(task_id, &context())
        .await
        .expect("wait");
    let entries = conversation_entries(&opened, conversation.id()).await;
    let result = entries
        .iter()
        .find(|entry| entry.kind == TOOL_RESULT_ENTRY)
        .and_then(|entry| entry.model.as_ref())
        .into_iter()
        .flatten()
        .find_map(|message| match message {
            Message::ToolResult(result) => Some(result),
            _ => None,
        })
        .expect("tool result");
    assert!(!result.is_error);
    assert!(result.content.iter().any(|content| matches!(
        content,
        ToolResultContent::Text(text) if text.text.contains("4") && text.text.contains("undefined") && text.text.contains("true")
    )));
    assert!(result.content.iter().any(|content| matches!(
        content,
        ToolResultContent::Image(image) if image.data == png && image.mime_type == "image/png"
    )));
    assert!(result.content.iter().any(|content| matches!(
        content,
        ToolResultContent::Text(text) if text.text.contains("<console_output>") && text.text.contains("visible console 4")
    )));
    assert!(!result.content.iter().any(|content| matches!(
        content,
        ToolResultContent::Text(text) if text.text.contains("ignored return")
    )));

    let reload_call = ToolCall {
        id: "codemode-store-reload".to_string(),
        name: "codemode".to_string(),
        arguments: serde_json::json!({
            "code": "text(load('saved')); text(load('deleted')); text(load('unsaved'));"
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
        thought_signature: None,
        namespace: None,
    };
    let reload_task = create_tool_task(&opened, conversation.id(), reload_call).await;
    opened
        .harness
        .wait_for_task(reload_task, &context())
        .await
        .expect("wait store reload");
    let reload_entries = conversation_entries(&opened, conversation.id()).await;
    let reloaded = reload_entries
        .iter()
        .filter(|entry| entry.kind == TOOL_RESULT_ENTRY)
        .flat_map(|entry| entry.model.as_ref().into_iter().flatten())
        .find_map(|message| match message {
            Message::ToolResult(result) if result.tool_call_id == "codemode-store-reload" => Some(
                result
                    .content
                    .iter()
                    .filter_map(|content| match content {
                        ToolResultContent::Text(text) => Some(text.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            _ => None,
        })
        .expect("store reload result");
    assert!(
        reloaded.contains("4") && reloaded.matches("undefined").count() >= 2,
        "got {reloaded}"
    );

    let error_call = ToolCall {
        id: "codemode-error-call".to_string(),
        name: "codemode".to_string(),
        arguments: serde_json::json!({ "code": "store('unsaved', 5); text('partial output'); throw new Error('visible cause');" }).as_object().cloned().unwrap_or_default(),
        thought_signature: None,
        namespace: None,
    };
    let error_task = create_tool_task(&opened, conversation.id(), error_call).await;
    opened
        .harness
        .wait_for_task(error_task, &context())
        .await
        .expect("wait error script");
    let entries = conversation_entries(&opened, conversation.id()).await;
    let error_result = entries
        .iter()
        .filter(|entry| entry.kind == TOOL_RESULT_ENTRY)
        .flat_map(|entry| entry.model.as_ref().into_iter().flatten())
        .find_map(|message| match message {
            Message::ToolResult(result) if result.tool_call_id == "codemode-error-call" => {
                Some(result)
            }
            _ => None,
        })
        .expect("script error result");
    assert!(error_result.is_error);
    let error_text = error_result
        .content
        .iter()
        .filter_map(|content| match content {
            ToolResultContent::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(error_text.contains("visible cause"), "got {error_text}");
    assert!(
        error_text.contains("partial output"),
        "partial output was lost: {error_text}"
    );
    let failed_write_call = ToolCall {
        id: "codemode-failed-store-write".to_string(),
        name: "codemode".to_string(),
        arguments: serde_json::json!({ "code": "text(load('unsaved'));" })
            .as_object()
            .cloned()
            .unwrap_or_default(),
        thought_signature: None,
        namespace: None,
    };
    let failed_write_task = create_tool_task(&opened, conversation.id(), failed_write_call).await;
    opened
        .harness
        .wait_for_task(failed_write_task, &context())
        .await
        .expect("wait failed store write");
    let failed_write_entries = conversation_entries(&opened, conversation.id()).await;
    let failed_write_text = failed_write_entries
        .iter()
        .filter(|entry| entry.kind == TOOL_RESULT_ENTRY)
        .flat_map(|entry| entry.model.as_ref().into_iter().flatten())
        .find_map(|message| match message {
            Message::ToolResult(result) if result.tool_call_id == "codemode-failed-store-write" => {
                Some(
                    result
                        .content
                        .iter()
                        .filter_map(|content| match content {
                            ToolResultContent::Text(text) => Some(text.text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
            }
            _ => None,
        })
        .expect("failed store write result");
    assert!(
        failed_write_text.contains("undefined"),
        "failed script wrote persistent state: {failed_write_text}"
    );
}
struct NestedChainTool {
    next: &'static str,
}

#[async_trait]
impl ToolExecute for NestedChainTool {
    async fn execute(
        &self,
        _args: rpi_durable::chord::json::JsonObject,
        api: &dyn ToolExecutionApi,
        context: &Context,
    ) -> Result<ToolExecutionResult, ToolExecuteError> {
        let result = api
            .execute_tool(
                self.next,
                serde_json::Map::new(),
                rpi_durable::harness::types::NestedToolOptions::default(),
                context,
            )
            .await
            .map_err(|error| {
                Box::new(std::io::Error::other(error.to_string())) as ToolExecuteError
            })?;
        if self.next == "leaf" {
            return Ok(ToolExecutionResult {
                structured_output: result.structured_output,
                is_error: Some(result.is_error),
                details: result.details,
                diagnostics: Some(result.diagnostics),
                usage: result.usage,
                ..ToolExecutionResult::default()
            });
        }
        Ok(ToolExecutionResult {
            structured_output: Some(serde_json::json!({
                "leafError": result.is_error,
                "leafResult": result.structured_output,
            })),
            ..ToolExecutionResult::default()
        })
    }
}

struct StructuredNestedFailure;

#[async_trait]
impl ToolExecute for StructuredNestedFailure {
    async fn execute(
        &self,
        _args: rpi_durable::chord::json::JsonObject,
        _api: &dyn ToolExecutionApi,
        _context: &Context,
    ) -> Result<ToolExecutionResult, ToolExecuteError> {
        Ok(ToolExecutionResult {
            structured_output: Some(serde_json::json!({ "reason": "structured leaf failure" })),
            is_error: Some(true),
            diagnostics: Some(vec![ToolDiagnostic {
                severity: DiagnosticSeverity::Error,
                code: Some("leaf_failure".to_string()),
                message: "structured leaf failure".to_string(),
            }]),
            usage: Some(rpi_ai::types::Usage {
                input: 4,
                output: 1,
                cache_read: 0,
                cache_write: 0,
                cache_write1h: None,
                reasoning: None,
                total_tokens: 5,
                cost: rpi_ai::types::UsageCost {
                    input: 0.1,
                    output: 0.2,
                    cache_read: 0.0,
                    cache_write: 0.0,
                    total: 0.3,
                },
            }),
            ..ToolExecutionResult::default()
        })
    }
}

#[tokio::test]
async fn nested_chain_keeps_terminal_receipts_after_descendant_retirement() {
    use rpi_durable::harness::types::ToolCaller;

    let mut parent_extension =
        tool_extension("chain_parent", serde_json::json!({ "type": "object" }));
    let mut parent = parent_extension.tools.remove(0);
    parent.callers = Some(vec![ToolCaller::Model]);
    parent.execute = Arc::new(NestedChainTool { next: "middle" });

    let mut middle_extension = tool_extension("middle", serde_json::json!({ "type": "object" }));
    let mut middle = middle_extension.tools.remove(0);
    middle.callers = Some(vec![ToolCaller::Tool]);
    middle.execute = Arc::new(NestedChainTool { next: "leaf" });

    let mut leaf_extension = tool_extension("leaf", serde_json::json!({ "type": "object" }));
    let mut leaf = leaf_extension.tools.remove(0);
    leaf.callers = Some(vec![ToolCaller::Tool]);
    leaf.execute = Arc::new(StructuredNestedFailure);

    let extension = Extension {
        name: "nested-chain-test".to_string(),
        tools: vec![parent, middle, leaf],
        ..Extension::default()
    };
    let opened = open(vec![extension], None, test_models(Vec::new())).await;
    let conversation = opened
        .harness
        .root(&context(), Default::default())
        .await
        .expect("root");
    let call = ToolCall {
        id: "nested-chain-call".to_string(),
        name: "chain_parent".to_string(),
        arguments: serde_json::Map::new(),
        thought_signature: None,
        namespace: None,
    };
    let task_id = create_tool_task(&opened, conversation.id(), call).await;
    opened
        .harness
        .wait_for_task(task_id, &context())
        .await
        .expect("wait");

    let entries = conversation_entries(&opened, conversation.id()).await;
    let parent_result = entries
        .iter()
        .filter_map(|entry| entry.model.as_ref())
        .flatten()
        .find_map(|message| match message {
            Message::ToolResult(result) if result.tool_call_id == "nested-chain-call" => {
                Some(result)
            }
            _ => None,
        })
        .expect("parent tool result");
    let nested = parent_result
        .nested_calls
        .as_ref()
        .expect("nested call summary");
    assert!(nested.complete);
    assert_eq!(nested.calls.len(), 2);
    assert_eq!(nested.calls[0].id, "nested-chain-call/1");
    assert_eq!(nested.calls[0].status, "ok");
    assert_eq!(nested.calls[1].status, "error");
    assert_eq!(
        nested.calls[1].error.as_deref(),
        Some("structured leaf failure")
    );

    let live = opened
        .harness
        .session()
        .snapshot(
            &rpi_durable::harness::LIVE_DOC,
            rpi_durable::session::DocOwner::Conversation(conversation.id()),
            &context(),
        )
        .await
        .expect("live snapshot")
        .unwrap_or_default();
    let slots = live
        .get("nestedTools")
        .and_then(serde_json::Value::as_array)
        .expect("nested slots");
    assert_eq!(slots.len(), 2);
    assert_eq!(slots[1]["result"]["isError"], serde_json::json!(true));
    assert_eq!(
        slots[1]["result"]["structuredOutput"]["reason"],
        serde_json::json!("structured leaf failure")
    );
    assert_eq!(
        slots[1]["result"]["usage"]["totalTokens"],
        serde_json::json!(5)
    );
}

#[tokio::test]
async fn codemode_keeps_three_level_nested_structured_error_receipts() {
    use rpi_durable::harness::types::ToolCaller;

    let mut parent_extension =
        tool_extension("chain_parent", serde_json::json!({ "type": "object" }));
    let mut parent = parent_extension.tools.remove(0);
    parent.callers = Some(vec![ToolCaller::Tool]);
    parent.structured_output_schema = Some(serde_json::json!({ "type": "object" }));
    parent.execute = Arc::new(NestedChainTool { next: "middle" });

    let mut middle_extension = tool_extension("middle", serde_json::json!({ "type": "object" }));
    let mut middle = middle_extension.tools.remove(0);
    middle.callers = Some(vec![ToolCaller::Tool]);
    middle.structured_output_schema = Some(serde_json::json!({ "type": "object" }));
    middle.execute = Arc::new(NestedChainTool { next: "leaf" });

    let mut leaf_extension = tool_extension("leaf", serde_json::json!({ "type": "object" }));
    let mut leaf = leaf_extension.tools.remove(0);
    leaf.callers = Some(vec![ToolCaller::Tool]);
    leaf.structured_output_schema = Some(serde_json::json!({ "type": "object" }));
    leaf.execute = Arc::new(StructuredNestedFailure);

    let mut codemode_extension = rpi_durable::harness::codemode::create_codemode_extension();
    codemode_extension.name = "codemode-nested-chain-test".to_string();
    let chain_extension = Extension {
        name: "nested-chain-test".to_string(),
        tools: vec![parent, middle, leaf],
        ..Extension::default()
    };
    let opened = open(
        vec![codemode_extension, chain_extension],
        None,
        test_models(Vec::new()),
    )
    .await;
    let conversation = opened
        .harness
        .root(&context(), Default::default())
        .await
        .expect("root");
    let task_id = create_tool_task(
        &opened,
        conversation.id(),
        ToolCall {
            id: "codemode-nested-chain-call".to_string(),
            name: "codemode".to_string(),
            arguments: serde_json::json!({
                "code": "return await tools.chain_parent({});"
            })
            .as_object()
            .cloned()
            .unwrap_or_default(),
            thought_signature: None,
            namespace: None,
        },
    )
    .await;
    opened
        .harness
        .wait_for_task(task_id, &context())
        .await
        .expect("wait");

    let entries = conversation_entries(&opened, conversation.id()).await;
    let result = entries
        .iter()
        .filter(|entry| entry.kind == TOOL_RESULT_ENTRY)
        .flat_map(|entry| entry.model.as_ref().into_iter().flatten())
        .find_map(|message| match message {
            Message::ToolResult(result) if result.tool_call_id == "codemode-nested-chain-call" => {
                Some(result)
            }
            _ => None,
        })
        .expect("code-mode tool result");
    assert!(!result.is_error);
    let text = result
        .content
        .iter()
        .filter_map(|content| match content {
            ToolResultContent::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("leafError") && text.contains("structured leaf failure"),
        "got {text}"
    );
    let nested = result.nested_calls.as_ref().expect("nested call summary");
    assert!(nested.complete);
    assert!(
        nested.calls.iter().any(|call| {
            call.status == "error" && call.error.as_deref() == Some("structured leaf failure")
        }),
        "missing descendant error receipt: {nested:?}"
    );
    let live = opened
        .harness
        .session()
        .snapshot(
            &rpi_durable::harness::LIVE_DOC,
            rpi_durable::session::DocOwner::Conversation(conversation.id()),
            &context(),
        )
        .await
        .expect("live snapshot")
        .unwrap_or_default();
    let slots = live
        .get("nestedTools")
        .and_then(serde_json::Value::as_array)
        .expect("nested slots");
    assert!(
        slots.iter().any(|slot| {
            slot["result"]["structuredOutput"]["reason"]
                == serde_json::json!("structured leaf failure")
                && slot["result"]["usage"]["totalTokens"] == serde_json::json!(5)
        }),
        "leaf structured receipt or usage was lost: {slots:?}"
    );
}
struct DurableCrashEffect(String);

#[async_trait]
impl ToolExecute for DurableCrashEffect {
    async fn execute(
        &self,
        _args: rpi_durable::chord::json::JsonObject,
        _api: &dyn ToolExecutionApi,
        _context: &Context,
    ) -> Result<ToolExecutionResult, ToolExecuteError> {
        let previous = match std::fs::read_to_string(&self.0) {
            Ok(value) => value.parse::<usize>().map_err(|error| {
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
                    as ToolExecuteError
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => return Err(Box::new(error)),
        };
        let count = previous + 1;
        std::fs::write(&self.0, count.to_string())
            .map_err(|error| Box::new(error) as ToolExecuteError)?;
        Ok(ToolExecutionResult {
            structured_output: Some(serde_json::json!({ "count": count })),
            ..ToolExecutionResult::default()
        })
    }
}

struct DurableCrashPause {
    mode: String,
    marker: String,
}

#[async_trait]
impl ToolExecute for DurableCrashPause {
    async fn execute(
        &self,
        _args: rpi_durable::chord::json::JsonObject,
        _api: &dyn ToolExecutionApi,
        _context: &Context,
    ) -> Result<ToolExecutionResult, ToolExecuteError> {
        if self.mode == "seed" {
            std::fs::write(&self.marker, "paused")
                .map_err(|error| Box::new(error) as ToolExecuteError)?;
            std::future::pending::<()>().await;
        }
        Ok(ToolExecutionResult {
            structured_output: Some(serde_json::json!("resumed")),
            ..ToolExecutionResult::default()
        })
    }
}

fn crash_replay_extension(mode: &str, effect_path: &str, pause_path: &str) -> Extension {
    use rpi_durable::harness::types::ToolCaller;

    let mut extension = rpi_durable::harness::codemode::create_codemode_extension();
    extension.name = "codemode-crash-test".to_string();

    let mut effect_extension =
        tool_extension("unsafe_effect", serde_json::json!({ "type": "object" }));
    let mut effect = effect_extension.tools.remove(0);
    effect.callers = Some(vec![ToolCaller::Tool]);
    effect.replay = ToolReplay::Unsafe;
    effect.structured_output_schema = Some(serde_json::json!({
        "type": "object",
        "properties": { "count": { "type": "integer" } },
        "required": ["count"],
        "additionalProperties": false
    }));
    effect.execute = Arc::new(DurableCrashEffect(effect_path.to_string()));

    let mut pause_extension = tool_extension("safe_pause", serde_json::json!({ "type": "object" }));
    let mut pause = pause_extension.tools.remove(0);
    pause.callers = Some(vec![ToolCaller::Tool]);
    pause.replay = ToolReplay::Safe;
    pause.structured_output_schema = Some(serde_json::json!({ "type": "string" }));
    pause.execute = Arc::new(DurableCrashPause {
        mode: mode.to_string(),
        marker: pause_path.to_string(),
    });
    extension.tools.extend([effect, pause]);
    extension
}

#[tokio::test]
async fn codemode_process_crash_replays_an_unsafe_effect_once() {
    const CHILD_MODE: &str = "ROOST_CODEMODE_CRASH_MODE";
    if let Ok(mode) = std::env::var(CHILD_MODE) {
        run_codemode_crash_child(&mode).await;
        return;
    }

    let directory = std::env::temp_dir().join(format!(
        "rpi-codemode-crash-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    std::fs::create_dir(&directory).expect("create crash test directory");
    let database_path = directory.join("durable.sqlite");
    let effect_path = directory.join("effect-count");
    let pause_path = directory.join("safe-child-paused");
    let identity_path = directory.join("task-identity");
    let result_path = directory.join("result");
    let executable = std::env::current_exe().expect("current test executable");
    let launch = |mode: &str| {
        std::process::Command::new(&executable)
            .arg("codemode_process_crash_replays_an_unsafe_effect_once")
            .env(CHILD_MODE, mode)
            .env("ROOST_CODEMODE_CRASH_DB", &database_path)
            .env("ROOST_CODEMODE_CRASH_EFFECT", &effect_path)
            .env("ROOST_CODEMODE_CRASH_PAUSE", &pause_path)
            .env("ROOST_CODEMODE_CRASH_IDENTITY", &identity_path)
            .env("ROOST_CODEMODE_CRASH_RESULT", &result_path)
            .spawn()
            .expect("spawn crash test child")
    };

    let mut first_process = launch("seed");
    let marker_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    while !pause_path.exists() {
        assert!(
            tokio::time::Instant::now() < marker_deadline,
            "the script never reached the paused safe child"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    first_process
        .kill()
        .expect("kill after first receipt and second child admission");
    let killed = first_process.wait().expect("wait for killed process");
    assert!(
        !killed.success(),
        "the first process unexpectedly completed"
    );

    let mut resumed_process = launch("resume");
    let resume_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let resumed_status = loop {
        if let Some(status) = resumed_process.try_wait().expect("poll resumed process") {
            break status;
        }
        assert!(
            tokio::time::Instant::now() < resume_deadline,
            "the resumed code-mode task did not settle"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };
    assert!(
        resumed_status.success(),
        "resume process failed: {resumed_status}"
    );
    assert_eq!(
        std::fs::read_to_string(&effect_path).expect("effect count"),
        "1"
    );
    let result = std::fs::read_to_string(&result_path).expect("resumed tool result");
    assert!(
        result.starts_with("false\n"),
        "code mode failed after restart: {result}"
    );
    assert!(
        result.contains("\"count\":1"),
        "first receipt changed: {result}"
    );
    assert!(
        result.contains("resumed"),
        "safe child did not resume: {result}"
    );
    std::fs::remove_dir_all(directory).expect("remove crash test directory");
}

async fn run_codemode_crash_child(mode: &str) {
    let database_path = std::env::var("ROOST_CODEMODE_CRASH_DB").expect("database path");
    let effect_path = std::env::var("ROOST_CODEMODE_CRASH_EFFECT").expect("effect path");
    let pause_path = std::env::var("ROOST_CODEMODE_CRASH_PAUSE").expect("pause marker path");
    let identity_path = std::env::var("ROOST_CODEMODE_CRASH_IDENTITY").expect("identity path");
    let result_path = std::env::var("ROOST_CODEMODE_CRASH_RESULT").expect("result path");
    let storage: Arc<dyn Storage> = Arc::new(
        rpi_durable::storage::open_node_sqlite_storage(
            &database_path,
            rpi_durable::storage::SqliteDatabaseOptions::default(),
        )
        .await
        .expect("open crash test storage"),
    );
    let opened = open_with_storage(
        storage,
        vec![crash_replay_extension(mode, &effect_path, &pause_path)],
        None,
        test_models(Vec::new()),
    )
    .await;
    if mode == "seed" {
        let conversation = opened
            .harness
            .root(&context(), Default::default())
            .await
            .expect("root");
        let task_id = create_tool_task(&opened, conversation.id(), ToolCall {
            id: "codemode-crash-call".to_string(),
            name: "codemode".to_string(),
            arguments: serde_json::json!({
                "code": "const first = await tools.unsafe_effect({}); const second = await tools.safe_pause({}); return [first, second];"
            }).as_object().cloned().unwrap_or_default(),
            thought_signature: None,
            namespace: None,
        }).await;
        std::fs::write(
            &identity_path,
            format!("{}:{}", conversation.id().as_number(), task_id.as_number()),
        )
        .expect("write crash task identity");
        opened
            .harness
            .wait_for_task(task_id, &context())
            .await
            .expect("wait for paused tool");
    } else {
        let identity = std::fs::read_to_string(&identity_path).expect("read crash task identity");
        let (conversation_id, task_id) = identity.split_once(':').expect("task identity pair");
        let conversation_id =
            ConversationId::from_number(conversation_id.parse().expect("conversation number"));
        let task_id = TaskId::from_number(task_id.parse().expect("task number"));
        opened
            .harness
            .wait_for_task(task_id, &context())
            .await
            .expect("wait for resumed tool");
        let entries = conversation_entries(&opened, conversation_id).await;
        let (is_error, text, _, _) = tool_result(&entries);
        std::fs::write(&result_path, format!("{is_error}\n{text}")).expect("write final result");
    }
    opened
        .harness
        .close(&context())
        .await
        .expect("close crash test harness");
}
