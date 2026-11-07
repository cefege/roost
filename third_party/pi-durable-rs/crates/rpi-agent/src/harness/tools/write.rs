//! Port of `packages/agent/src/harness/tools/write.ts` @ pi 0.82.1 (2efa728) —
//! the `write` tool.
//!
//! Intentional differences:
//! - The success message uses JS string length (`content.length`, UTF-16 code
//!   units — write.ts:33) so `encode_utf16().count()` reproduces upstream text
//!   for non-ASCII content; the message wording ("bytes") is kept verbatim.
//! - `AbortSignal | undefined` is `CancellationToken`; the two abort checks
//!   (write.ts:29, write.ts:31) throw `AgentError::Message("Operation
//!   aborted")` inside the mutation queue, so the queue stays locked until the
//!   aborted write settles.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::error::AgentError;
use crate::harness::tools::file_mutation_queue::with_file_mutation_queue;
use crate::harness::tools::path_utils::resolve_tool_path;
use crate::harness::tools::tool_context::ToolContext;
use crate::harness::types::AgentHarnessTool;
use crate::types::{AgentToolResult, AgentToolUpdateCallback};

/// `WriteToolInput` (write.ts:8-11).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WriteToolInput {
    pub path: String,
    pub content: String,
}

/// The `write` tool (write.ts:15-38).
pub struct WriteTool {
    description: String,
    parameters: Value,
}

/// `createWriteTool` (write.ts:15).
pub fn create_write_tool() -> WriteTool {
    WriteTool {
        description: "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories."
            .to_string(),
        parameters: json!({
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
        }),
    }
}

#[async_trait]
impl<TContext: ToolContext> AgentHarnessTool<TContext> for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn label(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> &Value {
        &self.parameters
    }

    async fn execute(
        &self,
        _tool_call_id: &str,
        params: Value,
        signal: CancellationToken,
        _on_update: Option<AgentToolUpdateCallback>,
        context: TContext,
    ) -> Result<AgentToolResult, AgentError> {
        let input: WriteToolInput = serde_json::from_value(params).map_err(AgentError::Json)?;
        let env = context.env();
        let absolute_path =
            resolve_tool_path(env.as_ref(), &input.path, Some(signal.clone())).await?;
        let env_for_queue = Arc::clone(&env);
        let path_for_queue = absolute_path.clone();
        with_file_mutation_queue(env.as_ref(), &absolute_path, async move {
            if signal.is_cancelled() {
                return Err(AgentError::Message("Operation aborted".to_string()));
            }
            env_for_queue
                .write_file(
                    &path_for_queue,
                    input.content.as_bytes(),
                    Some(signal.clone()),
                )
                .await
                .map_err(|error| AgentError::Message(error.message))?;
            if signal.is_cancelled() {
                return Err(AgentError::Message("Operation aborted".to_string()));
            }
            let length = input.content.encode_utf16().count();
            Ok(AgentToolResult {
                content: vec![crate::harness::tools::text_content(format!(
                    "Successfully wrote {length} bytes to {}",
                    input.path
                ))],
                details: Value::Null,
                ..Default::default()
            })
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::harness::env::nodejs::NodeExecutionEnv;
    use crate::harness::tools::ExecutionToolContext;
    use crate::harness::tools::test_helpers::{TempDir, ToolEnv, text_output};
    use crate::harness::types::ExecutionEnv;

    #[tokio::test]
    async fn writes_files_and_creates_parent_directories() {
        // "writes files and creates parent directories" (tools.test.ts:241-253).
        let dir = TempDir::new();
        let env: Arc<dyn ExecutionEnv> = Arc::new(NodeExecutionEnv::new(dir.cwd()));
        let tool = create_write_tool();
        let result = tool
            .execute(
                "write-1",
                serde_json::json!({ "path": "nested/dir/file.txt", "content": "hello" }),
                CancellationToken::new(),
                None,
                ExecutionToolContext::new(Arc::clone(&env)),
            )
            .await
            .unwrap();

        assert_eq!(
            text_output(&result),
            "Successfully wrote 5 bytes to nested/dir/file.txt"
        );
        let content = env
            .read_text_file("nested/dir/file.txt", None)
            .await
            .unwrap();
        assert_eq!(content, "hello");
    }

    #[tokio::test]
    async fn keeps_mutation_queue_locked_until_aborted_write_settles() {
        // "keeps the mutation queue locked until an aborted write settles"
        // (tools.test.ts:255-284).
        let dir = TempDir::new();
        let first_write_started = Arc::new(tokio::sync::watch::channel(false).0);
        let (finish_first_write_tx, finish_first_write_rx) = tokio::sync::watch::channel(false);
        let second_write_started = Arc::new(AtomicBool::new(false));

        let write_hook: crate::harness::tools::test_helpers::WriteFileHook = Arc::new({
            let first_write_started = Arc::clone(&first_write_started);
            let second_write_started = Arc::clone(&second_write_started);
            let finish_first_write_rx = finish_first_write_rx.clone();
            move |content, _abort_signal| {
                let first_write_started = Arc::clone(&first_write_started);
                let second_write_started = Arc::clone(&second_write_started);
                let mut finish_first_write_rx = finish_first_write_rx.clone();
                Box::pin(async move {
                    if content == b"first\n" {
                        let _ = first_write_started.send(true);
                        // Block until the test releases the first write
                        // (BlockingWriteExecutionEnv, tools.test.ts:57-60).
                        let _ = finish_first_write_rx.wait_for(|v| *v).await;
                    } else if content == b"second\n" {
                        second_write_started.store(true, Ordering::SeqCst);
                    }
                    crate::harness::tools::test_helpers::WriteFileAction::Continue
                })
            }
        });

        let env: Arc<dyn ExecutionEnv> =
            Arc::new(ToolEnv::new(NodeExecutionEnv::new(dir.cwd())).with_write_hook(write_hook));
        let tool = Arc::new(create_write_tool());
        let signal = CancellationToken::new();
        // Spawned: Rust futures are lazy, unlike the synchronous-until-first-
        // await `execute` call of the upstream test.
        let first_write = {
            let tool = Arc::clone(&tool);
            let env = Arc::clone(&env);
            let signal = signal.clone();
            tokio::spawn(async move {
                tool.execute(
                    "write-first",
                    serde_json::json!({ "path": "file.txt", "content": "first\n" }),
                    signal,
                    None,
                    ExecutionToolContext::new(env),
                )
                .await
            })
        };
        // Wait for the first write to be in flight, then abort.
        let mut started_rx = first_write_started.subscribe();
        started_rx.wait_for(|v| *v).await.unwrap();
        signal.cancel();
        let second_write = {
            let tool = Arc::clone(&tool);
            let env = Arc::clone(&env);
            tokio::spawn(async move {
                tool.execute(
                    "write-second",
                    serde_json::json!({ "path": "file.txt", "content": "second\n" }),
                    CancellationToken::new(),
                    None,
                    ExecutionToolContext::new(env),
                )
                .await
            })
        };

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(
            !second_write_started.load(Ordering::SeqCst),
            "second write must stay queued while the first is aborted"
        );
        let _ = finish_first_write_tx.send(true);
        // Upstream: `await expect(firstWrite).rejects.toThrow()` — any
        // rejection; the env surfaces its own `aborted` error because the
        // BlockingWriteExecutionEnv forwards the (cancelled) signal.
        first_write.await.unwrap().unwrap_err();
        second_write.await.unwrap().unwrap();
        let content = env.read_text_file("file.txt", None).await.unwrap();
        assert_eq!(content, "second\n");
    }

    #[test]
    fn tool_metadata() {
        let tool: &dyn AgentHarnessTool<ExecutionToolContext> = &create_write_tool();
        assert_eq!(tool.name(), "write");
        assert!(tool.description().contains("parent directories"));
    }
}
