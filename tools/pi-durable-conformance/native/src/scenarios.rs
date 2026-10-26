use crate::provider::{OracleProvider, message, tool_call};
use async_trait::async_trait;
use rpi_ai::types::{StopReason, TextContent, Tool, ToolResultContent};
use rpi_durable::chord::context::{BACKGROUND_CONTEXT, Context};
use rpi_durable::chord::json::JsonObject;
use rpi_durable::harness::{
    AgentChange, Extension, ExtensionsChange, HarnessOptions, ModelRef, Registry, RegistryImpl,
    ToolExecute, ToolExecuteError, ToolExecutionResult, ToolRegistration, ToolReplay,
    builtin_tasks, open_harness,
};
use rpi_durable::storage::MemoryStorage;
use rpi_durable::types::{EntryQuery, ScanOrder, Storage};
use rpi_durable::{SessionError, SessionResult};
use serde_json::{Value, json};
use std::error::Error;
use std::sync::Arc;

type ScenarioResult<T> = Result<T, Box<dyn Error>>;

struct EchoTool;

#[async_trait]
impl ToolExecute for EchoTool {
    async fn execute(
        &self,
        arguments: JsonObject,
        _api: &dyn rpi_durable::harness::ToolExecutionApi,
        _context: &Context,
    ) -> Result<ToolExecutionResult, ToolExecuteError> {
        let value = arguments
            .get("value")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                Box::new(SessionError::message("echo requires a string value")) as ToolExecuteError
            })?;
        Ok(ToolExecutionResult {
            content: Some(vec![ToolResultContent::Text(TextContent {
                text: format!("echo:{value}"),
                text_signature: None,
            })]),
            structured_output: Some(json!({ "value": value })),
            ..ToolExecutionResult::default()
        })
    }
}

fn echo_extension() -> Extension {
    Extension {
        name: "echo".to_string(),
        tools: vec![ToolRegistration {
            tool: Tool {
                name: "echo".to_string(),
                description: "Return the supplied value".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": { "value": { "type": "string" } },
                    "required": ["value"],
                }),
                constrained_sampling: None,
            },
            replay: ToolReplay::Unsafe,
            execution_mode: None,
            prepare_arguments: None,
            deferred: false,
            output_limits: None,
            structured_output_schema: Some(json!({
                "type": "object",
                "properties": { "value": { "type": "string" } },
                "required": ["value"],
            })),
            callers: None,
            execute: Arc::new(EchoTool),
        }],
        ..Extension::default()
    }
}

pub async fn tool_round() -> ScenarioResult<Value> {
    let extension = echo_extension();
    let (models, provider) = OracleProvider::new([
        tool_call("echo", "call-1", json!({ "value": "blue" })),
        message("finished", StopReason::Stop, None),
    ]);
    let registry = Arc::new(RegistryImpl::with_builtins(builtin_tasks()));
    registry.install(extension.clone())?;
    let harness = open_harness(
        Arc::new(MemoryStorage::new()) as Arc<dyn Storage>,
        options(models, Arc::clone(&registry)),
        &BACKGROUND_CONTEXT,
    )
    .await?;
    let conversation = harness
        .root(
            &BACKGROUND_CONTEXT,
            rpi_durable::harness::RootOptions {
                agent: Some(AgentChange {
                    model: Some(Some(ModelRef {
                        provider: "faux".to_string(),
                        model_id: "faux-1".to_string(),
                    })),
                    extensions: Some(Some(ExtensionsChange::List(vec![Arc::new(
                        extension.clone(),
                    )]))),
                    ..AgentChange::default()
                }),
                ..Default::default()
            },
        )
        .await?;
    let submission = conversation
        .submit(input("call echo", None), &BACKGROUND_CONTEXT)
        .await?;
    submission.wait(&BACKGROUND_CONTEXT).await?;
    let entries = entries(conversation.as_ref()).await?;
    let requests = provider
        .requests
        .lock()
        .map_err(|_| "provider request capture poisoned")?
        .clone();
    let trace = json!({
        "name": "tool-round",
        "entries": entries,
        "requests": requests.into_iter().map(|messages| json!({ "messages": messages })).collect::<Vec<_>>(),
        "requestCount": provider.requests.lock().map_err(|_| "provider request capture poisoned")?.len(),
    });
    harness.close(&BACKGROUND_CONTEXT).await?;
    Ok(trace)
}

pub(crate) fn options(
    models: rpi_ai::models::Models,
    registry: Arc<RegistryImpl>,
) -> HarnessOptions {
    HarnessOptions {
        models,
        registry: registry as Arc<dyn rpi_durable::harness::RegistryReader>,
        settings: None,
        env: None,
        conversation_created: None,
        now: None,
        on_report: None,
        read_only: false,
        set_timer: None,
        on_wake: None,
    }
}

pub(crate) fn scratch_sqlite_path() -> ScenarioResult<String> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .join("target/pi-durable-conformance");
    std::fs::create_dir_all(&root)?;
    Ok(root
        .join(format!("pi-durable-oracle-{}.sqlite", uuid::Uuid::new_v4()))
        .to_string_lossy()
        .into_owned())
}

pub async fn sqlite_checkpoint_reopen() -> ScenarioResult<Value> {
    let path = scratch_sqlite_path()?;
    let (models, provider) =
        OracleProvider::new([message("committed once", StopReason::Stop, None)]);
    let registry = Arc::new(RegistryImpl::with_builtins(builtin_tasks()));
    let storage: Arc<dyn Storage> = Arc::new(
        rpi_durable::storage::open_node_sqlite_storage(
            &path,
            rpi_durable::storage::SqliteDatabaseOptions::default(),
        )
        .await?,
    );
    let context = BACKGROUND_CONTEXT.clone();
    let harness = open_harness(
        Arc::clone(&storage),
        options(models.clone(), Arc::clone(&registry)),
        &context,
    )
    .await?;
    let root = harness
        .root(
            &context,
            rpi_durable::harness::RootOptions {
                agent: Some(AgentChange {
                    model: Some(Some(ModelRef {
                        provider: "faux".to_string(),
                        model_id: "faux-1".to_string(),
                    })),
                    ..AgentChange::default()
                }),
                ..Default::default()
            },
        )
        .await?;
    let submission = root
        .submit(input("persist me", Some("reopen-1")), &context)
        .await?;
    submission.wait(&context).await?;
    let before_close = entries(root.as_ref()).await?;
    let conversation_id = root.id();
    harness.close(&context).await?;
    storage.close(&context).await?;

    let reopened_storage: Arc<dyn Storage> = Arc::new(
        rpi_durable::storage::open_node_sqlite_storage(
            &path,
            rpi_durable::storage::SqliteDatabaseOptions::default(),
        )
        .await?,
    );
    let reopened = open_harness(
        Arc::clone(&reopened_storage),
        options(models, registry),
        &context,
    )
    .await?;
    let reopened_root = reopened
        .conversation(conversation_id, &context)
        .await?
        .ok_or_else(|| SessionError::message("reopened root conversation is missing"))?;
    let after_open = entries(reopened_root.as_ref()).await?;
    let duplicate = reopened_root
        .submit(input("persist me", Some("reopen-1")), &context)
        .await?;
    let duplicate_status = duplicate.status(&context).await?;
    let trace = json!({
        "name": "sqlite-checkpoint-reopen-dedup",
        "beforeClose": before_close,
        "afterOpen": after_open,
        "duplicateStatus": duplicate_status,
        "providerCalls": provider.requests.lock().map_err(|_| "provider request capture poisoned")?.len(),
    });
    reopened.close(&context).await?;
    reopened_storage.close(&context).await?;
    std::fs::remove_file(path)?;
    Ok(trace)
}

pub(crate) fn input(
    content: &str,
    request_id: Option<&str>,
) -> rpi_durable::harness::SubmissionDraft {
    rpi_durable::harness::SubmissionDraft::Input(rpi_durable::harness::InputSubmissionDraft {
        request_id: request_id.map(str::to_string),
        content: rpi_ai::types::UserContent::Text(content.to_string()),
        when_busy: None,
    })
}

pub(crate) async fn entries(
    conversation: &dyn rpi_durable::harness::Conversation,
) -> SessionResult<Vec<rpi_durable::EntryRecord>> {
    Ok(conversation
        .entries(
            EntryQuery {
                conversation_id: conversation.id(),
                min_entry_id: None,
                max_entry_id: None,
                order: Some(ScanOrder::Ascending),
            },
            1_000,
            None,
            &BACKGROUND_CONTEXT,
        )
        .await?
        .items)
}
