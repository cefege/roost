#[path = "../provider.rs"]
mod provider;

use async_trait::async_trait;
use rpi_ai::types::Tool;
use rpi_durable::chord::context::BACKGROUND_CONTEXT;
use rpi_durable::chord::json::JsonObject;
use rpi_durable::harness::{
    AgentChange, Extension, ExtensionsChange, HarnessOptions, InputSubmissionDraft, ModelRef,
    Registry, RegistryImpl, SubmissionDraft, ToolExecute, ToolExecuteError, ToolExecutionApi,
    ToolExecutionResult, ToolRegistration, ToolReplay, builtin_tasks, open_harness,
};
use rpi_durable::storage::{SqliteDatabaseOptions, open_node_sqlite_storage};
use rpi_durable::types::Storage;
use serde_json::json;
use std::error::Error;
use std::io::Write;
use std::sync::Arc;

struct UnsafeEffect;

#[async_trait]
impl ToolExecute for UnsafeEffect {
    async fn execute(
        &self,
        _args: JsonObject,
        _api: &dyn ToolExecutionApi,
        _context: &rpi_durable::chord::context::Context,
    ) -> Result<ToolExecutionResult, ToolExecuteError> {
        println!("READY");
        std::io::stdout().flush()?;
        std::future::pending::<()>().await;
        Ok(ToolExecutionResult::default())
    }
}

fn registration() -> Extension {
    Extension {
        name: "unsafe".to_string(),
        tools: vec![ToolRegistration {
            tool: Tool {
                name: "unsafe".to_string(),
                description: "An externally visible non-idempotent action".to_string(),
                parameters: json!({ "type": "object", "properties": {} }),
                constrained_sampling: None,
            },
            replay: ToolReplay::Unsafe,
            execution_mode: None,
            prepare_arguments: None,
            deferred: false,
            output_limits: None,
            structured_output_schema: None,
            callers: None,
            execute: Arc::new(UnsafeEffect),
        }],
        ..Extension::default()
    }
}

fn options(models: rpi_ai::models::Models, registry: Arc<RegistryImpl>) -> HarnessOptions {
    HarnessOptions {
        models,
        registry: registry as Arc<dyn rpi_durable::harness::RegistryReader>,
        settings: Some(rpi_durable::harness::HarnessSettings {
            progress: Some(rpi_durable::harness::types::ProgressPolicyPatch {
                persist_within_ms: Some(0.0),
                ..Default::default()
            }),
            ..Default::default()
        }),
        env: None,
        conversation_created: None,
        now: None,
        on_report: None,
        read_only: false,
        set_timer: None,
        on_wake: None,
    }
}

async fn run(path: &str) -> Result<(), Box<dyn Error>> {
    let (models, _provider) =
        provider::OracleProvider::new([provider::tool_call("unsafe", "unsafe-1", json!({}))]);
    let extension = registration();
    let registry = Arc::new(RegistryImpl::with_builtins(builtin_tasks()));
    registry.install(extension.clone())?;
    let storage: Arc<dyn Storage> =
        Arc::new(open_node_sqlite_storage(path, SqliteDatabaseOptions::default()).await?);
    let harness = open_harness(storage, options(models, registry), &BACKGROUND_CONTEXT).await?;
    let root = harness
        .root(
            &BACKGROUND_CONTEXT,
            rpi_durable::harness::RootOptions {
                agent: Some(AgentChange {
                    model: Some(Some(ModelRef {
                        provider: "faux".to_string(),
                        model_id: "faux-1".to_string(),
                    })),
                    extensions: Some(Some(ExtensionsChange::List(vec![Arc::new(extension)]))),
                    ..AgentChange::default()
                }),
                ..Default::default()
            },
        )
        .await?;
    let submission = root
        .submit(
            SubmissionDraft::Input(InputSubmissionDraft {
                request_id: Some("unsafe-interrupted".to_string()),
                content: rpi_ai::types::UserContent::Text("perform the unsafe action".to_string()),
                when_busy: None,
            }),
            &BACKGROUND_CONTEXT,
        )
        .await?;
    println!("SUBMISSION:{}", submission.id());
    std::io::stdout().flush()?;
    std::future::pending::<()>().await;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("SQLite path argument is required")?;
    run(&path).await
}
