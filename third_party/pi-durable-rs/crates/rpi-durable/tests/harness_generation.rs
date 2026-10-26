//! Generation task integration tests (port of the generation cases of
//! `packages/durable/test/harness-inbox.test.ts` and `harness-tasks.test.ts`
//! @ `abe508e1b`).
//!
//! Every case drives the real `pi.generation` built-in through a Harness over
//! memory storage with a scripted provider (see `generation_support`).

mod generation_support;
mod test_support;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::test_support::TempDir;
use async_trait::async_trait;
use generation_support::{
    DeferredProviderState, Script, context, entries, error_message, open, open_with_storage,
    root_ready, submit_input, test_models, test_models_with_state, text_message, tool_use_message,
};
use rpi_ai::types::{
    AssistantContent, AssistantMessage, DeferredHandle, Message, StopReason, Tool as AiTool,
    ToolResultContent,
};
use rpi_durable::chord::context::Context;
use rpi_durable::fs::{FileSystem, StdFileSystem};
use rpi_durable::harness::compaction::create_compaction;
use rpi_durable::harness::types::{
    CompactionPolicyPatch, ConversationRetryPolicyPatch, DiagnosticSeverity, ToolExecuteError,
};
use rpi_durable::harness::{
    CompactionReason, ContinueInput, Extension, GenerationHooks, HarnessSettings, HookApi,
    HookHandlers, HookRegistration, LIVE_DOC, LiveState, RequestMessages, ToolDiagnostic,
    ToolExecute, ToolExecutionApi, ToolExecutionResult, ToolRegistration, ToolReplay,
};
use rpi_durable::ids::{ConversationId, EntryId, TaskId};
use rpi_durable::session::{DocOwner, EntryDraft};
use rpi_durable::storage::{
    JsonlStorage, JsonlStorageOptions, SqliteDatabaseOptions, open_node_sqlite_storage,
};
use rpi_durable::types::{
    EntryRecord, InputSubmissionStatus, Storage, SubmissionRecord, TaskState,
};

/// Assistant text of every `pi.assistant` entry, in order.
fn assistant_texts(entries: &[EntryRecord]) -> Vec<String> {
    let mut texts = Vec::new();
    for entry in entries {
        let Some(messages) = &entry.model else {
            continue;
        };
        for message in messages {
            if let Message::Assistant(assistant) = message {
                let text = assistant
                    .content
                    .iter()
                    .filter_map(|content| match content {
                        AssistantContent::Text(text) => Some(text.text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                texts.push(text);
            }
        }
    }
    texts
}

/// The text of every `pi.tool-result` entry's message.
fn tool_result_texts(entries: &[EntryRecord]) -> Vec<String> {
    let mut texts = Vec::new();
    for entry in entries {
        let Some(messages) = &entry.model else {
            continue;
        };
        for message in messages {
            if let Message::ToolResult(result) = message {
                texts.push(
                    result
                        .content
                        .iter()
                        .filter_map(|content| match content {
                            ToolResultContent::Text(text) => Some(text.text.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
            }
        }
    }
    texts
}

/// The task ID currently owning `pi.live.run`, if any.
async fn run_task_id(
    opened: &generation_support::Opened,
    conversation_id: ConversationId,
) -> Option<TaskId> {
    let live = opened
        .harness
        .session()
        .snapshot(
            &LIVE_DOC,
            DocOwner::Conversation(conversation_id),
            &context(),
        )
        .await
        .ok()??;
    let state: LiveState =
        serde_json::from_value(serde_json::Value::Object(live)).expect("live state");
    state.run.map(|run| run.task_id)
}

fn input_status(record: &SubmissionRecord) -> Option<&InputSubmissionStatus> {
    match record {
        SubmissionRecord::Input { status, .. } => Some(status),
        SubmissionRecord::Write { .. } => None,
    }
}

/// upstream: "answers with a committed assistant entry and ends the run done".
#[tokio::test]
async fn answer_appends_assistant_entry_and_ends_run_done() {
    let opened = open(
        Vec::new(),
        None,
        test_models(vec![Script::Message(text_message(
            "hello",
            StopReason::Stop,
        ))]),
    )
    .await;
    let conversation = root_ready(&opened.harness).await;
    let submission = submit_input(&conversation, "hi").await;
    let settled = submission.wait(&context()).await.expect("wait");
    assert!(
        matches!(
            input_status(&settled),
            Some(InputSubmissionStatus::Done { .. })
        ),
        "expected Done, got {settled:?}"
    );
    assert_eq!(
        assistant_texts(&entries(&conversation).await),
        vec!["hello"]
    );
    assert!(run_task_id(&opened, conversation.id()).await.is_none());
}

/// upstream: "retries a retryable assistant error, then succeeds".
#[tokio::test]
async fn retryable_error_retries_then_succeeds() {
    let settings = HarnessSettings {
        retry: Some(ConversationRetryPolicyPatch {
            base_delay_ms: Some(1),
            ..Default::default()
        }),
        ..Default::default()
    };
    let opened = open(
        Vec::new(),
        Some(settings),
        test_models(vec![
            Script::Message(error_message("transient", "Please retry your request")),
            Script::Message(text_message("ok", StopReason::Stop)),
        ]),
    )
    .await;
    let conversation = root_ready(&opened.harness).await;
    let submission = submit_input(&conversation, "hi").await;
    let settled = submission.wait(&context()).await.expect("wait");
    assert!(
        matches!(
            input_status(&settled),
            Some(InputSubmissionStatus::Done { .. })
        ),
        "expected Done, got {settled:?}"
    );
    // The failed attempt's assistant entry is committed too.
    assert_eq!(
        assistant_texts(&entries(&conversation).await),
        vec!["", "ok"]
    );
}

/// upstream: "fails model_error on a context overflow without compaction".
#[tokio::test]
async fn overflow_without_compaction_fails_model_error() {
    let settings = HarnessSettings {
        compaction: Some(CompactionPolicyPatch {
            enabled: Some(false),
            ..Default::default()
        }),
        ..Default::default()
    };
    let opened = open(
        Vec::new(),
        Some(settings),
        test_models(vec![Script::Message(error_message(
            "overflow",
            "This model's maximum context length is 200000 tokens, but 208432 were requested",
        ))]),
    )
    .await;
    let conversation = root_ready(&opened.harness).await;
    let submission = submit_input(&conversation, "hi").await;
    let settled = submission.wait(&context()).await.expect("wait");
    match input_status(&settled) {
        Some(InputSubmissionStatus::Unanswered { reason, .. }) => {
            assert_eq!(reason, "model_error");
        }
        other => panic!("expected Unanswered(model_error), got {other:?}"),
    }
}

#[tokio::test]
async fn overflow_compacts_once_then_retries_with_the_compacted_context() {
    let settings = HarnessSettings {
        compaction: Some(CompactionPolicyPatch {
            keep_recent_tokens: Some(1),
            ..Default::default()
        }),
        ..Default::default()
    };
    let opened = open(
        Vec::new(),
        Some(settings),
        test_models(vec![
            Script::Message(error_message(
                "overflow",
                "This model's maximum context length is 200000 tokens, but 208432 were requested",
            )),
            Script::Message(text_message("the summary", StopReason::Stop)),
            Script::Message(text_message("recovered", StopReason::Stop)),
        ]),
    )
    .await;
    let conversation = root_ready(&opened.harness).await;
    let conversation_id = conversation.id();
    for index in 0..4 {
        let change = rpi_durable::session::commit_fn(move |tx| {
            Box::pin(async move {
                let mut draft = EntryDraft::new(rpi_durable::entries::USER_ENTRY);
                draft.model = Some(vec![Message::User(rpi_ai::types::UserMessage {
                    role: rpi_ai::types::UserRole::User,
                    content: rpi_ai::types::UserContent::Text(format!("history {index}")),
                    timestamp: index as i64,
                })]);
                tx.append_entry(conversation_id, draft).await?;
                Ok(())
            })
        });
        opened
            .harness
            .session()
            .commit(change, &context())
            .await
            .expect("append history");
    }
    let submission = submit_input(&conversation, "continue").await;
    let settled = submission.wait(&context()).await.expect("wait");
    assert!(matches!(
        input_status(&settled),
        Some(InputSubmissionStatus::Done { .. })
    ));
    let committed = entries(&conversation).await;
    assert_eq!(
        committed
            .iter()
            .filter(|entry| entry.kind == rpi_durable::entries::COMPACTION_ENTRY)
            .count(),
        1
    );
    assert!(assistant_texts(&committed).contains(&"recovered".to_string()));
}

/// A provider-side deferred result is fetched and completed rather than failed.
#[tokio::test]
async fn deferred_response_is_fetched() {
    let handle = rpi_ai::types::DeferredHandle {
        provider: "test".to_string(),
        model_id: "test-model".to_string(),
        api: "openai".to_string(),
        id: "d1".to_string(),
        expires_at: None,
        poll_after_ms: Some(0),
        data: None,
    };
    let opened = open(
        Vec::new(),
        None,
        test_models(vec![Script::Deferred {
            handle,
            result: text_message("deferred answer", StopReason::Stop),
        }]),
    )
    .await;
    let conversation = root_ready(&opened.harness).await;
    let submission = submit_input(&conversation, "hi").await;
    let settled = submission.wait(&context()).await.expect("wait");
    assert!(matches!(
        input_status(&settled),
        Some(InputSubmissionStatus::Done { .. })
    ));
    assert_eq!(
        assistant_texts(&entries(&conversation).await),
        vec!["deferred answer".to_string()]
    );
}

async fn persistent_test_storage(
    directory: &str,
    root: &std::path::Path,
    jsonl: bool,
) -> Arc<dyn Storage> {
    if jsonl {
        let fs: Arc<dyn FileSystem> = Arc::new(StdFileSystem::new(root));
        Arc::new(
            JsonlStorage::open(
                directory,
                fs,
                JsonlStorageOptions { fsync: false },
                &context(),
            )
            .await
            .expect("open jsonl storage"),
        )
    } else {
        Arc::new(
            open_node_sqlite_storage(
                &(directory.to_owned() + ".sqlite"),
                SqliteDatabaseOptions::default(),
            )
            .await
            .expect("open sqlite storage"),
        )
    }
}

/// Deferred provider handles survive durable reopen and cancel against that handle.
#[tokio::test]
async fn deferred_poll_reopens_and_abort_cancels_on_sqlite_and_jsonl() {
    for jsonl in [false, true] {
        let directory = TempDir::new("generation-deferred-reopen");
        let storage_path = directory.path_str() + "/storage";
        let initial_storage = persistent_test_storage(&storage_path, directory.path(), jsonl).await;
        let handle = DeferredHandle {
            provider: "test".to_string(),
            model_id: "test-model".to_string(),
            api: "openai".to_string(),
            id: if jsonl { "jsonl-d1" } else { "sqlite-d1" }.to_string(),
            expires_at: None,
            poll_after_ms: Some(60_000),
            data: None,
        };
        let handle_id = handle.id.clone();
        let (models, provider_state) = test_models_with_state(
            vec![Script::Deferred {
                handle,
                result: text_message("unused", StopReason::Stop),
            }],
            DeferredProviderState::default(),
        );
        let opened = open_with_storage(initial_storage.clone(), Vec::new(), None, models).await;
        let conversation = root_ready(&opened.harness).await;
        let submission = submit_input(&conversation, "hi").await;
        let task_id = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(task_id) = run_task_id(&opened, conversation.id()).await
                    && let Some(record) = opened
                        .harness
                        .get_task(task_id, &context())
                        .await
                        .expect("read generation")
                    && let TaskState::Sleeping { checkpoint, .. } = record.state
                    && checkpoint.get("phase").and_then(serde_json::Value::as_str) == Some("poll")
                {
                    break task_id;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("generation enters durable poll sleep");
        opened
            .harness
            .close(&context())
            .await
            .expect("close harness");
        initial_storage
            .close(&context())
            .await
            .expect("close initial storage");
        drop(opened);

        let reopened_storage =
            persistent_test_storage(&storage_path, directory.path(), jsonl).await;
        let models = test_models_with_state(Vec::new(), provider_state.clone()).0;
        let reopened = open_with_storage(reopened_storage.clone(), Vec::new(), None, models).await;
        reopened
            .harness
            .abort_task(task_id, &context())
            .await
            .expect("abort reopened deferred poll");
        let cancellation_log = provider_state.cancelled.lock().expect("cancel log");
        assert_eq!(cancellation_log.as_slice(), &[handle_id]);
        drop(cancellation_log);
        let reopened_submission = reopened
            .harness
            .submission(submission.id(), &context())
            .await
            .expect("reacquire submission")
            .expect("submission remains present");
        let settled = reopened_submission.wait(&context()).await.expect("wait");
        assert!(matches!(
            input_status(&settled),
            Some(InputSubmissionStatus::Unanswered { reason, .. }) if reason == "aborted"
        ));
        reopened
            .harness
            .close(&context())
            .await
            .expect("close reopened harness");
        reopened_storage
            .close(&context())
            .await
            .expect("close reopened storage");
    }
}

/// A committed answer stays terminal across reopen and is never requested twice.
#[tokio::test]
async fn committed_answer_is_not_reissued_after_reopen() {
    let storage: Arc<dyn Storage> = Arc::new(rpi_durable::storage::MemoryStorage::new());
    let (models, provider_state) = test_models_with_state(
        vec![Script::Message(text_message("committed", StopReason::Stop))],
        DeferredProviderState::default(),
    );
    let opened = open_with_storage(Arc::clone(&storage), Vec::new(), None, models).await;
    let conversation = root_ready(&opened.harness).await;
    let submission = submit_input(&conversation, "hi").await;
    let settled = submission.wait(&context()).await.expect("answer settles");
    assert!(matches!(
        input_status(&settled),
        Some(InputSubmissionStatus::Done { .. })
    ));
    assert_eq!(
        provider_state
            .requests
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    opened
        .harness
        .close(&context())
        .await
        .expect("close first harness");

    let models = test_models_with_state(Vec::new(), provider_state.clone()).0;
    let reopened = open_with_storage(Arc::clone(&storage), Vec::new(), None, models).await;
    reopened
        .harness
        .wait_for_idle(&context())
        .await
        .expect("reopened scheduler is idle");
    let conversation = root_ready(&reopened.harness).await;
    assert_eq!(
        assistant_texts(&entries(&conversation).await),
        vec!["committed".to_string()]
    );
    assert_eq!(
        provider_state
            .requests
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    reopened
        .harness
        .close(&context())
        .await
        .expect("close second harness");
}

/// upstream: "fails no_model when the agent has no model".
#[tokio::test]
async fn no_model_fails_no_model() {
    let opened = open(
        Vec::new(),
        None,
        test_models(vec![Script::Message(text_message(
            "unused",
            StopReason::Stop,
        ))]),
    )
    .await;
    let conversation = generation_support::root_only(&opened.harness).await;
    let submission = submit_input(&conversation, "hi").await;
    let settled = submission.wait(&context()).await.expect("wait");
    match input_status(&settled) {
        Some(InputSubmissionStatus::Unanswered { reason, .. }) => assert_eq!(reason, "no_model"),
        other => panic!("expected Unanswered(no_model), got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Tool round
// ---------------------------------------------------------------------------

struct EchoTool;

#[async_trait]
impl ToolExecute for EchoTool {
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

fn echo_extension() -> Extension {
    Extension {
        name: "test".to_string(),
        tools: vec![ToolRegistration {
            tool: AiTool {
                name: "echo".to_string(),
                description: "echo".to_string(),
                parameters: serde_json::json!({ "type": "object", "properties": {} }),
                constrained_sampling: None,
            },
            replay: ToolReplay::Unsafe,
            execution_mode: None,
            deferred: false,
            prepare_arguments: None,
            output_limits: None,
            structured_output_schema: None,
            callers: None,
            execute: Arc::new(EchoTool),
        }],
        ..Extension::default()
    }
}

/// upstream: "dispatches a tool task, appends its result, and issues a follow-up".
#[tokio::test]
async fn tool_round_dispatches_result_and_follow_up() {
    let opened = open(
        vec![echo_extension()],
        None,
        test_models(vec![
            Script::Message(tool_use_message("echo", "c1", serde_json::json!({}))),
            Script::Message(text_message("done", StopReason::Stop)),
        ]),
    )
    .await;
    let conversation = root_ready(&opened.harness).await;
    let submission = submit_input(&conversation, "hi").await;
    let settled = submission.wait(&context()).await.expect("wait");
    assert!(
        matches!(
            input_status(&settled),
            Some(InputSubmissionStatus::Done { .. })
        ),
        "expected Done, got {settled:?}"
    );
    let entries = entries(&conversation).await;
    assert_eq!(assistant_texts(&entries), vec!["", "done"]);
    let results = tool_result_texts(&entries);
    assert_eq!(results.len(), 1);
    assert!(results[0].contains("hello output"), "got {}", results[0]);
    assert!(results[0].contains("[warn] note"), "got {}", results[0]);
}

// ---------------------------------------------------------------------------
// Abort
// ---------------------------------------------------------------------------

/// upstream: "the first onYield continuation appends a user message and hands over".
#[tokio::test]
async fn on_yield_continuation_hands_over() {
    struct ContinueOnce(AtomicBool);

    #[async_trait]
    impl GenerationHooks for ContinueOnce {
        async fn before_request(
            &self,
            _request: RequestMessages,
            _api: &dyn HookApi,
            _context: &Context,
        ) -> Option<RequestMessages> {
            None
        }

        async fn after_response(
            &self,
            _message: &AssistantMessage,
            _api: &dyn HookApi,
            _context: &Context,
        ) {
        }

        async fn on_yield(
            &self,
            _answer: &AssistantMessage,
            _api: &dyn HookApi,
            _context: &Context,
        ) -> Option<ContinueInput> {
            if self.0.swap(true, Ordering::SeqCst) {
                return None;
            }
            Some(ContinueInput {
                continue_input: rpi_ai::types::UserContent::Text("again".to_string()),
            })
        }

        async fn after_tools(
            &self,
            _assistant: EntryId,
            _results: &[EntryId],
            _api: &dyn HookApi,
            _context: &Context,
        ) {
        }
    }

    let hooks = Extension {
        name: "continue".to_string(),
        hooks: vec![HookRegistration {
            task: "pi.generation".to_string(),
            handlers: HookHandlers::Generation(Arc::new(ContinueOnce(AtomicBool::new(false)))),
        }],
        ..Extension::default()
    };
    let opened = open(
        vec![hooks],
        None,
        test_models(vec![
            Script::Message(text_message("first", StopReason::Stop)),
            Script::Message(text_message("second", StopReason::Stop)),
        ]),
    )
    .await;
    let conversation = root_ready(&opened.harness).await;
    let submission = submit_input(&conversation, "hi").await;
    let settled = submission.wait(&context()).await.expect("wait");
    assert!(
        matches!(
            input_status(&settled),
            Some(InputSubmissionStatus::Done { .. })
        ),
        "expected Done, got {settled:?}"
    );
    let entries = entries(&conversation).await;
    assert_eq!(assistant_texts(&entries), vec!["first", "second"]);
    let users: Vec<String> = entries
        .iter()
        .flat_map(|entry| entry.model.iter().flatten())
        .filter_map(|message| match message {
            Message::User(user) => Some(serde_json::to_string(&user.content).unwrap_or_default()),
            _ => None,
        })
        .collect();
    assert!(
        users.iter().any(|user| user.contains("again")),
        "got {users:?}"
    );
}

/// upstream: "an aborted run appends an aborted assistant entry and settles aborted".
#[tokio::test]
async fn abort_mid_stream_appends_aborted_result() {
    let opened = open(
        Vec::new(),
        None,
        test_models(vec![Script::Pending("thinking".to_string())]),
    )
    .await;
    let conversation = root_ready(&opened.harness).await;
    let submission = submit_input(&conversation, "hi").await;
    let mut task_id = None;
    for _ in 0..200 {
        task_id = run_task_id(&opened, conversation.id()).await;
        if task_id.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let task_id = task_id.expect("run started");
    // Let the throttled partial flush land before aborting.
    tokio::time::sleep(Duration::from_millis(200)).await;
    opened
        .harness
        .abort_task(task_id, &context())
        .await
        .expect("abort");
    let settled = submission.wait(&context()).await.expect("wait");
    match input_status(&settled) {
        Some(InputSubmissionStatus::Unanswered { reason, .. }) => assert_eq!(reason, "aborted"),
        other => panic!("expected Unanswered(aborted), got {other:?}"),
    }
    let aborted = entries(&conversation).await.into_iter().any(|entry| {
        entry.model.iter().flatten().any(|message| {
            matches!(message, Message::Assistant(assistant) if assistant.stop_reason == StopReason::Aborted)
        })
    });
    assert!(aborted, "expected an aborted assistant entry");
}

// ---------------------------------------------------------------------------
// Compaction basics
// ---------------------------------------------------------------------------

/// upstream: "a manual compaction places a summary entry with the first kept head".
#[tokio::test]
async fn manual_compaction_places_summary() {
    let settings = HarnessSettings {
        compaction: Some(CompactionPolicyPatch {
            keep_recent_tokens: Some(1),
            ..Default::default()
        }),
        ..Default::default()
    };
    let opened = open(
        Vec::new(),
        Some(settings),
        test_models(vec![Script::Message(text_message(
            "the summary",
            StopReason::Stop,
        ))]),
    )
    .await;
    let conversation = root_ready(&opened.harness).await;
    let conversation_id = conversation.id();
    for index in 0..4 {
        let change = rpi_durable::session::commit_fn(move |tx| {
            Box::pin(async move {
                let mut draft = EntryDraft::new(rpi_durable::entries::USER_ENTRY);
                draft.model = Some(vec![Message::User(rpi_ai::types::UserMessage {
                    role: rpi_ai::types::UserRole::User,
                    content: rpi_ai::types::UserContent::Text(format!("message number {index}")),
                    timestamp: index as i64,
                })]);
                tx.append_entry(conversation_id, draft).await?;
                Ok(())
            })
        });
        opened
            .harness
            .session()
            .commit(change, &context())
            .await
            .expect("append");
    }
    let create = rpi_durable::session::commit_fn(move |tx| {
        Box::pin(async move {
            create_compaction(tx, conversation_id, CompactionReason::Manual, None, None).await
        })
    });
    let task_id = opened
        .harness
        .session()
        .commit(create, &context())
        .await
        .expect("create compaction");
    let settled = opened
        .harness
        .wait_for_task(task_id, &context())
        .await
        .expect("wait compaction");
    assert!(matches!(
        settled.record.state,
        rpi_durable::types::TaskState::Terminal { .. }
    ));
    let result: rpi_durable::harness::types::CompactionResult =
        serde_json::from_value(settled.result).expect("typed compaction receipt");
    assert!(result.submission_id.is_some());
    assert!(result.entry_id.is_none());
    let entries = entries(&conversation).await;
    let compaction = entries
        .iter()
        .find(|entry| entry.kind == rpi_durable::entries::COMPACTION_ENTRY)
        .expect("compaction entry");
    assert!(compaction.head.is_some());
    assert_eq!(
        compaction.data.as_ref().and_then(|data| data.get("reason")),
        Some(&serde_json::json!("manual"))
    );
    let text = compaction
        .model
        .iter()
        .flatten()
        .find_map(|message| match message {
            Message::User(user) => Some(serde_json::to_string(&user.content).unwrap_or_default()),
            _ => None,
        })
        .unwrap_or_default();
    assert!(text.contains("the summary"), "got {text}");
}
