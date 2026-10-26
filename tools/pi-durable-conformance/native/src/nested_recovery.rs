//! Nested-tool identity, deduplication, and structured-error oracle scenario.
//! Called by `recovery_traces` and drives the durable tool execution API.
//! Captures concrete nested identities and structured results from the kernel.
use super::*;
struct Echo;

#[async_trait]
impl ToolExecute for Echo {
    async fn execute(
        &self,
        args: JsonObject,
        _api: &dyn ToolExecutionApi,
        _context: &Context,
    ) -> Result<ToolExecutionResult, ToolExecuteError> {
        let value = args.get("value").and_then(Value::as_str).ok_or_else(|| {
            Box::new(SessionError::message("echo requires a string value")) as ToolExecuteError
        })?;
        Ok(ToolExecutionResult {
            content: Some(vec![ToolResultContent::Text(TextContent {
                text: format!("echo {value}"),
                text_signature: None,
            })]),
            structured_output: Some(json!({ "value": value })),
            ..ToolExecutionResult::default()
        })
    }
}

#[derive(Default)]
struct NestedTrace {
    nested_results: Mutex<Vec<Value>>,
    identities: Mutex<Vec<Value>>,
}

struct Batch(Arc<NestedTrace>);

#[async_trait]
impl ToolExecute for Batch {
    async fn execute(
        &self,
        _args: JsonObject,
        api: &dyn ToolExecutionApi,
        context: &Context,
    ) -> Result<ToolExecutionResult, ToolExecuteError> {
        self.0
            .identities
            .lock()
            .map_err(|_| {
                Box::new(SessionError::message("identity trace poisoned")) as ToolExecuteError
            })?
            .push(json!({ "taskId": api.task_id(), "callId": api.call_id() }));
        let mut results = Vec::new();
        for (name, arguments, key) in [
            ("echo", json!({ "value": "x" }), "stable"),
            ("echo", json!({ "value": "x" }), "stable"),
            ("missing", json!({}), "missing"),
        ] {
            let result = api
                .execute_tool(
                    name,
                    arguments.as_object().cloned().unwrap_or_default(),
                    rpi_durable::harness::types::NestedToolOptions {
                        key: Some(key.to_string()),
                        progress: None,
                    },
                    context,
                )
                .await;
            results.push(match result {
                Ok(value) => serde_json::to_value(value)
                    .map_err(|error| Box::new(error) as ToolExecuteError)?,
                Err(error) => return Err(Box::new(error) as ToolExecuteError),
            });
        }
        if results.len() != 3
            || results[0] != results[1]
            || results[0].get("structuredOutput") != Some(&json!({ "value": "x" }))
            || results[0].get("isError") != Some(&Value::Bool(false))
            || results[0].get("diagnostics") != Some(&json!([]))
            || results[2].get("isError") != Some(&Value::Bool(true))
            || results[2]
                .get("diagnostics")
                .and_then(Value::as_array)
                .and_then(|diagnostics| diagnostics.first())
                .and_then(|diagnostic| diagnostic.get("code"))
                .and_then(Value::as_str)
                != Some("tool_unavailable")
        {
            return Err(Box::new(SessionError::message(
                "nested tool identity, structured output, or error contract diverged",
            )));
        }
        self.0
            .nested_results
            .lock()
            .map_err(|_| {
                Box::new(SessionError::message("nested trace poisoned")) as ToolExecuteError
            })?
            .extend(results);
        Ok(ToolExecutionResult {
            content: Some(vec![ToolResultContent::Text(TextContent {
                text: "batch complete".to_string(),
                text_signature: None,
            })]),
            ..ToolExecutionResult::default()
        })
    }
}

pub async fn nested_tool_identity_replay_structured_error() -> Result<Value, Box<dyn Error>> {
    let trace_state = Arc::new(NestedTrace::default());
    let tools = vec![
        ToolRegistration {
            tool: Tool {
                name: "echo".to_string(),
                description: "Echo structured data".to_string(),
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
            execute: Arc::new(Echo),
        },
        ToolRegistration {
            tool: Tool {
                name: "batch".to_string(),
                description: "Run keyed nested calls".to_string(),
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
            execute: Arc::new(Batch(Arc::clone(&trace_state))),
        },
    ];
    let extension = Extension {
        name: "nested".to_string(),
        tools,
        ..Extension::default()
    };
    let (models, _provider) = OracleProvider::new([
        tool_call("batch", "batch-1", json!({})),
        message("done", StopReason::Stop, None),
    ]);
    let registry = Arc::new(RegistryImpl::with_builtins(builtin_tasks()));
    registry.install(extension.clone())?;
    let storage: Arc<dyn Storage> = Arc::new(rpi_durable::storage::MemoryStorage::new());
    let harness = open_harness(
        storage,
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
                    ..AgentChange::default()
                }),
                ..Default::default()
            },
        )
        .await?;
    conversation
        .configure(
            AgentChange {
                extensions: Some(Some(ExtensionsChange::List(vec![Arc::new(extension)]))),
                ..AgentChange::default()
            },
            &BACKGROUND_CONTEXT,
        )
        .await?;
    let submission = conversation
        .submit(input("run nested batch", None), &BACKGROUND_CONTEXT)
        .await?;
    submission.wait(&BACKGROUND_CONTEXT).await?;
    let inspection = harness.inspect(&BACKGROUND_CONTEXT).await?;
    if !inspection.tasks.is_empty() {
        return Err("completed nested scenario retained live tasks".into());
    }
    let nested_results = trace_state
        .nested_results
        .lock()
        .map_err(|_| "nested trace poisoned")?
        .clone();
    let identities = trace_state
        .identities
        .lock()
        .map_err(|_| "identity trace poisoned")?
        .clone();
    let trace = json!({
        "name": "nested-tool-identity-replay-structured-error",
        "nestedResults": serde_json::to_value(nested_results)?,
        "identities": identities,
        "entries": serde_json::to_value(entries(conversation.as_ref()).await?)?,
        "tasks": [],
    });
    harness.close(&BACKGROUND_CONTEXT).await?;
    Ok(trace)
}
