//! Durable JavaScript code mode driven by the isolated QuickJS WASM worker.
//!
//! Task-owned start and journal documents gate nested effects and replay; the
//! conversation-owned store changes only after successful script completion.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};

use crate::chord::context::Context;
use crate::chord::json::JsonObject;
use crate::harness::define::{define_extension, section};
use crate::harness::types::{
    Extension, PromptInput, ToolCaller, ToolDiagnostic, ToolExecute, ToolExecuteError,
    ToolExecutionApi, ToolExecutionResult, ToolRegistration, ToolReplay,
};
use rpi_ai::types::{TextContent, ToolResultContent};

mod driver;
mod models;
mod output;
mod source;
mod state;
use state::{
    CODEMODE_VERSION, DeclaredGlobal, DeclaredTool, load_or_create_run, persist_completion,
};

const MAX_SCRIPT_BYTES: usize = 1_000_000;
pub(super) const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

/// Durable code-mode tool plus its hidden model-call tools and prompt contract.
pub fn create_codemode_extension() -> Extension {
    let mut tools = vec![create_codemode_tool()];
    tools.extend(models::tools());
    define_extension(Extension {
        name: "codemode".to_string(),
        tools,
        sections: vec![section(
            "codemode",
            Arc::new(|_input: PromptInput, _context: Context| {
                Box::pin(async { Some(models::DESCRIPTION.to_string()) })
            }),
            None,
        )],
        ..Extension::default()
    })
}

fn create_codemode_tool() -> ToolRegistration {
    ToolRegistration {
        tool: rpi_ai::types::Tool {
            name: "codemode".to_string(),
            description: "Run JavaScript that composes the conversation's tools. Use tools.<name>(args), text(), image(), console, store(), and load().".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "code": { "type": "string", "description": "JavaScript source to execute." } },
                "required": ["code"],
                "additionalProperties": false
            }),
            constrained_sampling: None,
        },
        replay: ToolReplay::Safe,
        execution_mode: None,
        deferred: false,
        prepare_arguments: None,
        output_limits: Some(crate::harness::types::OutputLimits {
            max_bytes: Some(MAX_OUTPUT_BYTES as u64),
            max_lines: Some(100_000),
            retain: None,
        }),
        structured_output_schema: None,
        callers: Some(vec![ToolCaller::Model]),
        execute: Arc::new(Codemode),
    }
}

struct Codemode;

#[async_trait]
impl ToolExecute for Codemode {
    async fn execute(
        &self,
        arguments: JsonObject,
        api: &dyn ToolExecutionApi,
        context: &Context,
    ) -> Result<ToolExecutionResult, ToolExecuteError> {
        let source = arguments
            .get("code")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| tool_error("codemode expects a string `code` argument"))?;
        if source.len() > MAX_SCRIPT_BYTES {
            return Ok(error_result(
                "codemode source exceeds the one-megabyte limit",
            ));
        }
        let parsed = match source::parse_source(source) {
            Ok(parsed) => parsed,
            Err(error) => return Ok(error_result(&error)),
        };
        let agent = api.agent(context).await.map_err(tool_error)?;
        let tools = agent
            .callable
            .iter()
            .map(|tool| DeclaredTool {
                name: tool.name().to_string(),
                js_name: tool_identifier(tool.name()),
                description: tool.tool.description.clone(),
                structured: tool.structured_output_schema.is_some()
                    || tool.name() == models::CLASSIFY_TOOL
                    || tool.name() == models::GENERATE_IMAGES_TOOL,
                parameters: tool.tool.parameters.clone(),
            })
            .collect::<Vec<_>>();
        let globals = [
            "models.getModelsOfType",
            "models.getAvailableOfType",
            "models.getModelOfType",
            "models.classify",
            "models.generateImages",
        ]
        .into_iter()
        .map(|name| DeclaredGlobal {
            name: name.to_string(),
            spread: true,
        })
        .collect::<Vec<_>>();
        let task_id = api.task_id();
        let conversation_id = api.conversation_id();
        let now = api.now().map_err(tool_error)?;
        let now_ms = timestamp_ms(now)?;
        let hash = format!("{:x}", Sha256::digest(parsed.code.as_bytes()));
        let seed_bytes = Sha256::digest(task_id.as_number().to_be_bytes());
        let seed = u64::from_be_bytes(
            seed_bytes[..8]
                .try_into()
                .map_err(|_| tool_error("codemode seed"))?,
        );
        let deadline_ms = parsed
            .timeout_ms
            .map(|timeout| now_ms.saturating_add(timeout));
        let (run, journal, rerun) = load_or_create_run(
            api,
            task_id,
            conversation_id,
            &hash,
            now_ms,
            seed,
            deadline_ms,
            parsed.max_output_tokens,
            tools,
            globals,
            context,
        )
        .await?;
        if let Some(result) = run.result.clone() {
            return Ok(result);
        }
        if run.version != CODEMODE_VERSION || run.script_hash != hash {
            let failure = error_result(
                "The script cannot resume because its code-mode version or source changed.",
            );
            return persist_completion(
                api,
                task_id,
                conversation_id,
                failure,
                None,
                context.clone(),
            )
            .await;
        }
        tracing::info!(task_id = %task_id, rerun = rerun, journal_entries = journal.entries.len(), "codemode_run_started");
        driver::execute(
            parsed.code,
            run,
            journal,
            rerun,
            api.shared(),
            context.clone(),
        )
        .await
    }
}

fn tool_identifier(name: &str) -> String {
    let mut identifier = String::with_capacity(name.len());
    for (index, code_unit) in name.encode_utf16().enumerate() {
        let character = char::from_u32(u32::from(code_unit)).unwrap_or('_');
        let valid = if index == 0 {
            character.is_ascii_alphabetic() || character == '_' || character == '$'
        } else {
            character.is_ascii_alphanumeric() || character == '_' || character == '$'
        };
        identifier.push(if valid { character } else { '_' });
    }
    if identifier.is_empty() {
        "_".to_string()
    } else {
        identifier
    }
}

pub(super) fn apply_store_writes(
    mut store: BTreeMap<String, JsonValue>,
    writes: Vec<(String, Option<JsonValue>)>,
) -> BTreeMap<String, JsonValue> {
    for (key, value) in writes {
        if let Some(value) = value {
            store.insert(key, value);
        } else {
            store.remove(&key);
        }
    }
    store
}

fn timestamp_ms(now: f64) -> Result<u64, ToolExecuteError> {
    if !now.is_finite() || now < 0.0 || now > u64::MAX as f64 {
        return Err(tool_error(
            "Code-mode clock returned an invalid millisecond timestamp",
        ));
    }
    Ok(now as u64)
}

fn error_result(message: &str) -> ToolExecutionResult {
    ToolExecutionResult {
        content: Some(vec![ToolResultContent::Text(TextContent {
            text: message.to_string(),
            text_signature: None,
        })]),
        is_error: Some(true),
        diagnostics: Some(vec![ToolDiagnostic {
            severity: crate::harness::types::DiagnosticSeverity::Error,
            code: Some("codemode_error".to_string()),
            message: message.to_string(),
        }]),
        ..ToolExecutionResult::default()
    }
}

fn tool_error(message: impl std::fmt::Display) -> ToolExecuteError {
    Box::new(std::io::Error::other(message.to_string()))
}
