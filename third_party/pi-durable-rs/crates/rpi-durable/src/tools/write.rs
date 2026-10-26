//! Write tool (port of `packages/durable/src/tools/write.ts` @ `abe508e1b`).

use std::sync::Arc;

use async_trait::async_trait;
use rpi_ai::types::ToolResultContent;
use rpi_ai::types::{TextContent, Tool};
use serde_json::json;

use crate::chord::context::Context;
use crate::env::types::FileContent;
use crate::harness::types::{
    ToolExecute, ToolExecuteError, ToolExecutionApi, ToolExecutionResult, ToolRegistration,
    ToolReplay,
};
use crate::tools::env::require_env;
use crate::tools::file_mutation_queue::with_file_mutation_queue;
use crate::tools::path_utils::resolve_tool_path;
use crate::tools::tool_error;

struct WriteTool;

#[async_trait]
impl ToolExecute for WriteTool {
    async fn execute(
        &self,
        args: crate::chord::json::JsonObject,
        api: &dyn ToolExecutionApi,
        context: &Context,
    ) -> Result<ToolExecutionResult, ToolExecuteError> {
        let path = args
            .get("path")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();
        let content = args
            .get("content")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();
        let env = require_env(api)?;
        let absolute = resolve_tool_path(env.as_ref(), &path, context).await?;
        with_file_mutation_queue(Arc::clone(&env), &absolute, context, async {
            if aborted(context) {
                return Err(tool_error("Operation aborted"));
            }
            env.write_file(&absolute, FileContent::Text(content), context)
                .await?;
            if aborted(context) {
                return Err(tool_error("Operation aborted"));
            }
            Ok(ToolExecutionResult {
                content: Some(vec![ToolResultContent::Text(TextContent {
                    text: format!("Successfully wrote to {path}"),
                    text_signature: None,
                })]),
                ..Default::default()
            })
        })
        .await
    }
}

fn aborted(context: &Context) -> bool {
    context
        .abort_signal()
        .map(|signal| signal.is_cancelled())
        .unwrap_or(false)
}

fn parameters() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Path to the file to write (relative or absolute)"
            },
            "content": {
                "type": "string",
                "description": "Content to write to the file"
            }
        },
        "required": ["path", "content"]
    })
}

/// `createWriteTool` (write.ts:16).
pub fn create_write_tool() -> ToolRegistration {
    ToolRegistration {
        tool: Tool {
            name: "write".to_string(),
            description: "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories.".to_string(),
            parameters: parameters(),
            constrained_sampling: None,
        },
        replay: ToolReplay::Unsafe,
        execution_mode: None,
        deferred: false,
        prepare_arguments: None,
        output_limits: None,
        structured_output_schema: None,
        callers: None,
        execute: Arc::new(WriteTool),
    }
}
