//! Port of `packages/agent/src/harness/tools/bash.ts` @ pi 0.82.1 (2efa728) —
//! the `bash` tool: execute a command through the [`ExecutionEnv`] shell with
//! throttled streaming updates and tail-truncated output.
//!
//! Intentional differences:
//! - `setTimeout` / `Date.now()` throttling (bash.ts:9, 70-105) becomes a
//!   tokio timer task driven by shared state; the coalescing semantics are
//!   preserved (one pending timer, dirty flag, latest-progress-at-fire-time).
//!   The leading edge is preserved too: upstream's `lastUpdateAt = 0`
//!   (bash.ts:72) becomes `None`, so the first chunk fires immediately
//!   (`delay = 100 - Date.now() <= 0`, bash.ts:95-99) instead of waiting out
//!   the throttle window.
//! - `ShellExecOptions.timeout` is `Option<u64>` whole seconds (harness type
//!   layer), while upstream accepts fractional seconds. The tool validates
//!   with the upstream messages and passes `ceil`ed whole seconds to the env,
//!   clamped to the env's maximum (2_147_483). The error message still reports
//!   the original value (`Command timed out after {timeout} seconds`).
//! - `throw capture.executionError` (bash.ts:151) keeps only the message text
//!   (`AgentError::Message`); the error class is not preserved.
//! - `BashPrepare` (bash.ts:30-34) becomes an `Arc<dyn Fn>` returning a
//!   `BoxFuture`; `TContext` is handed to it by value (the context also
//!   provides the env handle, so nothing needs to outlive the call).

use std::collections::BTreeMap;
use std::future::Future;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::error::AgentError;
use crate::harness::tools::tool_context::ToolContext;
use crate::harness::tools::truncation_to_value;
use crate::harness::types::{AgentHarnessTool, ExecutionErrorCode};
use crate::harness::utils::shell_output::{
    ShellCaptureOptions, ShellCaptureProgress, execute_shell_with_capture,
};
use crate::harness::utils::truncate::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncatedBy, TruncationResult, format_size,
};
use crate::types::{AgentToolResult, AgentToolUpdateCallback};

/// `MAX_TIMEOUT_SECONDS = 2_147_483_647 / 1000` (bash.ts:8).
const MAX_TIMEOUT_SECONDS: f64 = 2_147_483_647.0 / 1000.0;
/// `BASH_UPDATE_THROTTLE_MS` (bash.ts:9).
const BASH_UPDATE_THROTTLE_MS: u64 = 100;

/// `BashToolInput` (bash.ts:11-15).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct BashToolInput {
    pub command: String,
    #[serde(default)]
    pub timeout: Option<f64>,
}

/// `BashToolDetails` (bash.ts:18-21).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashToolDetails {
    pub truncation: Option<TruncationResult>,
    pub full_output_path: Option<String>,
}

/// `BashExecution` (bash.ts:23-28).
#[derive(Debug, Clone)]
pub struct BashExecution {
    pub command: String,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    pub inherit_env: bool,
}

/// `BashPrepare` (bash.ts:30-34): `(execution, context, signal) => void |
/// Promise<void>`.
pub type BashPrepare<TContext> = Arc<
    dyn Fn(
            &mut BashExecution,
            TContext,
            CancellationToken,
        ) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

/// `BashToolOptions` (bash.ts:36-39).
pub struct BashToolOptions<TContext> {
    pub command_prefix: Option<String>,
    pub prepare: Option<BashPrepare<TContext>>,
}

impl<TContext> Default for BashToolOptions<TContext> {
    fn default() -> Self {
        BashToolOptions {
            command_prefix: None,
            prepare: None,
        }
    }
}

/// The `bash` tool (bash.ts:51-160).
pub struct BashTool<TContext> {
    options: BashToolOptions<TContext>,
    description: String,
    parameters: Value,
    _context: PhantomData<TContext>,
}

/// `createBashTool` (bash.ts:51).
pub fn create_bash_tool<TContext: ToolContext>(
    options: BashToolOptions<TContext>,
) -> BashTool<TContext> {
    BashTool {
        options,
        description: format!(
            "Execute a bash command in the current working directory. Returns stdout and stderr. Output is truncated to last {DEFAULT_MAX_LINES} lines or {}KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.",
            DEFAULT_MAX_BYTES / 1024
        ),
        parameters: json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Bash command to execute"
                },
                "timeout": {
                    "type": "number",
                    "description": "Timeout in seconds (optional, no default timeout)"
                }
            },
            "required": ["command"]
        }),
        _context: PhantomData,
    }
}

/// `validateTimeout` (bash.ts:41-49).
fn validate_timeout(timeout: Option<f64>) -> Result<(), AgentError> {
    let Some(timeout) = timeout else {
        return Ok(());
    };
    if !timeout.is_finite() || timeout <= 0.0 {
        return Err(AgentError::Message(
            "Invalid timeout: must be a finite number of seconds".to_string(),
        ));
    }
    if timeout > MAX_TIMEOUT_SECONDS {
        return Err(AgentError::Message(format!(
            "Invalid timeout: maximum is {MAX_TIMEOUT_SECONDS} seconds"
        )));
    }
    Ok(())
}

/// Snapshot of the capture progress carried into a throttled update.
#[derive(Clone)]
struct ProgressSnapshot {
    output: String,
    truncation: TruncationResult,
    full_output_path: Option<String>,
}

impl ProgressSnapshot {
    fn from_progress(progress: &ShellCaptureProgress) -> Self {
        ProgressSnapshot {
            output: progress.output.clone(),
            truncation: progress.truncation.clone(),
            full_output_path: progress.full_output_path.clone(),
        }
    }
}

/// `emitOutputUpdate` / `scheduleOutputUpdate` shared state (bash.ts:74-105).
struct UpdateState {
    /// `onUpdate` — the streaming callback, shared with the timer task.
    on_update: Option<Arc<AgentToolUpdateCallback>>,
    /// `getLatestProgress` — latest progress snapshot.
    latest: Mutex<Option<ProgressSnapshot>>,
    /// `updateDirty`.
    dirty: AtomicBool,
    /// `lastUpdateAt` — `None` until the first emission: upstream
    /// initialises it to `0` (bash.ts:72), a sentinel meaning "long ago",
    /// which makes the first chunk's delay `<= 0` (immediate leading edge).
    last_update_at: Mutex<Option<Instant>>,
    /// `updateTimer` — the pending throttled emit task.
    pending: Mutex<Option<JoinHandle<()>>>,
}

impl UpdateState {
    /// `emitOutputUpdate` (bash.ts:74-86).
    fn emit_output_update(&self) {
        let Some(on_update) = &self.on_update else {
            return;
        };
        if !self.dirty.load(Ordering::SeqCst) {
            return;
        }
        let Some(progress) = self
            .latest
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
        else {
            return;
        };
        self.dirty.store(false, Ordering::SeqCst);
        *self
            .last_update_at
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Instant::now());
        on_update(AgentToolResult {
            content: vec![crate::harness::tools::text_content(progress.output)],
            details: update_details(&progress.truncation, progress.full_output_path.as_deref()),
            ..Default::default()
        });
    }

    /// `clearUpdateTimer` (bash.ts:87-91).
    fn clear_update_timer(&self) {
        if let Some(handle) = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            handle.abort();
        }
    }

    /// `scheduleOutputUpdate` (bash.ts:92-105).
    fn schedule_output_update(self: &Arc<Self>) {
        if self.on_update.is_none() {
            return;
        }
        self.dirty.store(true, Ordering::SeqCst);
        let elapsed = self
            .last_update_at
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .map(|last_update_at| last_update_at.elapsed());
        // Upstream `lastUpdateAt = 0` (bash.ts:72) is a sentinel meaning
        // "never updated": the first chunk's `delay = 100 - Date.now()` is
        // `<= 0`, so the first update fires immediately on the leading edge
        // (bash.ts:95-99). `None` plays that role here; afterwards the delay
        // is the time remaining until the throttle window closes.
        let delay = match elapsed {
            None => None,
            Some(elapsed) => Duration::from_millis(BASH_UPDATE_THROTTLE_MS).checked_sub(elapsed),
        };
        match delay {
            None => {
                self.clear_update_timer();
                self.emit_output_update();
            }
            Some(delay) => {
                let mut pending = self
                    .pending
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if pending.is_none() {
                    let state = Arc::clone(self);
                    *pending = Some(tokio::spawn(async move {
                        tokio::time::sleep(delay).await;
                        {
                            let mut slot = state
                                .pending
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            *slot = None;
                        }
                        state.emit_output_update();
                    }));
                }
            }
        }
    }
}

/// Details JSON of a throttled update: `{ truncation?, fullOutputPath? }`
/// (bash.ts:81-84; absent keys for upstream `undefined` values).
fn update_details(truncation: &TruncationResult, full_output_path: Option<&str>) -> Value {
    let mut map = Map::new();
    if truncation.truncated {
        map.insert("truncation".into(), truncation_to_value(truncation));
    }
    if let Some(path) = full_output_path {
        map.insert("fullOutputPath".into(), Value::String(path.to_string()));
    }
    Value::Object(map)
}

/// `appendStatus` (bash.ts:144).
fn append_status(text: &str, status: String) -> String {
    if text.is_empty() {
        status
    } else {
        format!("{text}\n\n{status}")
    }
}

#[async_trait]
impl<TContext: ToolContext> AgentHarnessTool<TContext> for BashTool<TContext> {
    fn name(&self) -> &str {
        "bash"
    }

    fn label(&self) -> &str {
        "bash"
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
        on_update: Option<AgentToolUpdateCallback>,
        context: TContext,
    ) -> Result<AgentToolResult, AgentError> {
        let input: BashToolInput = serde_json::from_value(params).map_err(AgentError::Json)?;
        validate_timeout(input.timeout)?;
        let env = context.env();
        let mut execution = BashExecution {
            // `commandPrefix` is joined with a newline (bash.ts:63).
            command: match &self.options.command_prefix {
                Some(prefix) => format!("{prefix}\n{}", input.command),
                None => input.command,
            },
            cwd: env.cwd().to_string(),
            env: BTreeMap::new(),
            inherit_env: true,
        };
        if let Some(prepare) = &self.options.prepare {
            (prepare)(&mut execution, context, signal.clone()).await;
        }

        let state = Arc::new(UpdateState {
            on_update: on_update.map(Arc::new),
            latest: Mutex::new(None),
            dirty: AtomicBool::new(false),
            last_update_at: Mutex::new(None),
            pending: Mutex::new(None),
        });
        // Initial empty update (bash.ts:107).
        if let Some(on_update) = &state.on_update {
            on_update(AgentToolResult {
                ..Default::default()
            });
        }

        // Fractional seconds are not representable in `ShellExecOptions.timeout`
        // (see the module header): round up to whole seconds, capped at the
        // env's maximum (2_147_483_647 ms).
        let env_timeout = input.timeout.map(|t| (t.ceil() as u64).min(2_147_483));

        let capture_result = execute_shell_with_capture(
            env.as_ref(),
            &execution.command,
            Some(ShellCaptureOptions {
                cwd: Some(execution.cwd.clone()),
                env: Some(execution.env.clone()),
                inherit_env: Some(execution.inherit_env),
                timeout: env_timeout,
                abort_signal: Some(signal.clone()),
                on_chunk: Some(Box::new({
                    let state = Arc::clone(&state);
                    move |_chunk, progress| {
                        *state
                            .latest
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                            Some(ProgressSnapshot::from_progress(progress));
                        state.schedule_output_update();
                    }
                })),
                return_execution_errors: true,
            }),
        )
        .await
        .map_err(|error| AgentError::Message(error.message))?;

        // The final update carries the complete capture (bash.ts:123-126);
        // the pending timer is cancelled first so it cannot emit stale output
        // afterwards (`clearUpdateTimer`, bash.ts:123).
        state.clear_update_timer();
        *state
            .latest
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(ProgressSnapshot {
            output: capture_result.output.clone(),
            truncation: capture_result.truncation.clone(),
            full_output_path: capture_result.full_output_path.clone(),
        });
        state.dirty.store(true, Ordering::SeqCst);
        state.emit_output_update();
        // Keep the finally-style timer cancellation for the error paths below.
        let _pending_guard = PendingGuard(&state);

        let mut output_text = capture_result.output.clone();
        let mut details: Value = Value::Null;
        if capture_result.truncation.truncated {
            // `details = { truncation, fullOutputPath }` (bash.ts:130-131).
            let mut details_map = Map::new();
            details_map.insert(
                "truncation".into(),
                truncation_to_value(&capture_result.truncation),
            );
            if let Some(path) = &capture_result.full_output_path {
                details_map.insert("fullOutputPath".into(), Value::String(path.clone()));
            }
            details = Value::Object(details_map);

            let start_line =
                capture_result.truncation.total_lines - capture_result.truncation.output_lines + 1;
            let end_line = capture_result.truncation.total_lines;
            let path = capture_result.full_output_path.clone().unwrap_or_default();
            let footer = if capture_result.truncation.last_line_partial {
                format!(
                    "[Showing last {} of line {end_line} (line is {}). Full output: {path}]",
                    format_size(capture_result.truncation.output_bytes),
                    format_size(capture_result.last_line_bytes)
                )
            } else if capture_result.truncation.truncated_by == Some(TruncatedBy::Lines) {
                format!(
                    "[Showing lines {start_line}-{end_line} of {}. Full output: {path}]",
                    capture_result.truncation.total_lines
                )
            } else {
                format!(
                    "[Showing lines {start_line}-{end_line} of {} ({} limit). Full output: {path}]",
                    capture_result.truncation.total_lines,
                    format_size(DEFAULT_MAX_BYTES)
                )
            };
            output_text.push_str("\n\n");
            output_text.push_str(&footer);
        }

        if capture_result.cancelled {
            return Err(AgentError::Message(append_status(
                &output_text,
                "Command aborted".to_string(),
            )));
        }
        if let Some(error) = &capture_result.execution_error {
            if error.code == ExecutionErrorCode::Timeout {
                let timeout_text = input
                    .timeout
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| "undefined".to_string());
                return Err(AgentError::Message(append_status(
                    &output_text,
                    format!("Command timed out after {timeout_text} seconds"),
                )));
            }
            return Err(AgentError::Message(error.message.clone()));
        }
        if let Some(exit_code) = capture_result.exit_code
            && exit_code != 0
        {
            return Err(AgentError::Message(append_status(
                &output_text,
                format!("Command exited with code {exit_code}"),
            )));
        }

        Ok(AgentToolResult {
            content: vec![crate::harness::tools::text_content(
                if output_text.is_empty() {
                    "(no output)".to_string()
                } else {
                    output_text
                },
            )],
            details,
            ..Default::default()
        })
    }
}

/// `clearUpdateTimer` in a `finally` (bash.ts:156-158) — the pending timer is
/// cancelled on every exit path.
struct PendingGuard<'a>(&'a UpdateState);

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.0.clear_update_timer();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::Value;

    use super::*;
    use crate::harness::env::nodejs::NodeExecutionEnv;
    use crate::harness::tools::ExecutionToolContext;
    use crate::harness::tools::test_helpers::{TempDir, ToolEnv, text_output};
    use crate::harness::types::{ExecutionEnv, FileSystem, ShellExecResult};

    fn context(env: NodeExecutionEnv) -> ExecutionToolContext {
        ExecutionToolContext::new(Arc::new(env))
    }

    #[tokio::test]
    async fn executes_commands_and_combines_stdout_and_stderr() {
        // "executes commands and combines stdout and stderr"
        // (tools.test.ts:451-463).
        let dir = TempDir::new();
        let env = NodeExecutionEnv::new(dir.cwd());
        let result = create_bash_tool::<ExecutionToolContext>(BashToolOptions::default())
            .execute(
                "bash-1",
                serde_json::json!({ "command": "printf out; printf err >&2" }),
                CancellationToken::new(),
                None,
                context(env),
            )
            .await
            .unwrap();

        let output = text_output(&result);
        assert!(output.contains("out"));
        assert!(output.contains("err"));
    }

    #[tokio::test]
    async fn reports_nonzero_exits_and_timeouts() {
        // "reports nonzero exits and timeouts" (tools.test.ts:465-475).
        let dir = TempDir::new();
        let env: Arc<dyn ExecutionEnv> = Arc::new(NodeExecutionEnv::new(dir.cwd()));
        let tool = create_bash_tool::<ExecutionToolContext>(BashToolOptions::default());

        let err = tool
            .execute(
                "bash-2",
                serde_json::json!({ "command": "printf failed; exit 7" }),
                CancellationToken::new(),
                None,
                ExecutionToolContext::new(Arc::clone(&env)),
            )
            .await
            .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("failed"));
        assert!(message.contains("Command exited with code 7"));

        let err = tool
            .execute(
                "bash-3",
                serde_json::json!({ "command": "sleep 2", "timeout": 0.01 }),
                CancellationToken::new(),
                None,
                ExecutionToolContext::new(Arc::clone(&env)),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("Command timed out after 0.01 seconds")
        );
    }

    #[tokio::test]
    async fn preserves_truncated_output_when_command_times_out() {
        // "preserves truncated output when a command times out"
        // (tools.test.ts:477-503).
        let dir = TempDir::new();
        let env: Arc<dyn ExecutionEnv> = Arc::new(NodeExecutionEnv::new(dir.cwd()));
        let err = create_bash_tool::<ExecutionToolContext>(BashToolOptions::default())
            .execute(
                "bash-timeout-output",
                serde_json::json!({
                    "command": "i=1; while [ $i -le 3000 ]; do echo line-$i; i=$((i + 1)); done; sleep 2",
                    "timeout": 0.05
                }),
                CancellationToken::new(),
                None,
                ExecutionToolContext::new(Arc::clone(&env)),
            )
            .await
            .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("Command timed out after 0.05 seconds"));
        let full_output_path = message
            .split("Full output: ")
            .nth(1)
            .and_then(|rest| rest.split([']', '\n']).next())
            .expect("error message must carry the full output path");
        assert!(!full_output_path.is_empty());
        let full_output = env.read_text_file(full_output_path, None).await.unwrap();
        assert!(full_output.contains("line-1\nline-2"));
        assert!(full_output.contains("line-2999\nline-3000"));
    }

    #[tokio::test]
    async fn ignores_output_callbacks_after_execution_settles() {
        // "ignores output callbacks after execution settles"
        // (tools.test.ts:505-519).
        let dir = TempDir::new();
        let env = ToolEnv::new(NodeExecutionEnv::new(dir.cwd()));
        let env = env.with_exec_override(Arc::new(|_command, options| {
            Box::pin(async move {
                // Synchronous chunk, then a late chunk after the tool settled
                // (LateOutputExecutionEnv, tools.test.ts:92-101).
                if let Some(callback) = options.as_ref().and_then(|o| o.on_stdout.as_ref()) {
                    callback("before\n");
                }
                if let Some(callback) = options.and_then(|o| o.on_stdout) {
                    let callback = Arc::new(callback);
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        callback("late\n");
                    });
                }
                Ok(ShellExecResult {
                    stdout: "before\n".to_string(),
                    stderr: String::new(),
                    exit_code: 0,
                })
            })
        }));

        let updates: Arc<std::sync::Mutex<Vec<String>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let updates2 = Arc::clone(&updates);
        let result = create_bash_tool::<ExecutionToolContext>(BashToolOptions::default())
            .execute(
                "bash-late",
                serde_json::json!({ "command": "late" }),
                CancellationToken::new(),
                Some(Box::new(move |update| {
                    updates2.lock().unwrap().push(text_output(&update));
                })),
                ExecutionToolContext::new(Arc::new(env)),
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(text_output(&result), "before\n");
        let updates = updates.lock().unwrap();
        assert!(
            !updates.iter().any(|update| update.contains("late")),
            "late chunks must be ignored after the tool settles"
        );
    }

    #[tokio::test]
    async fn reports_total_size_of_oversized_final_line() {
        // "reports the total size of an oversized final line"
        // (tools.test.ts:521-532).
        let dir = TempDir::new();
        let env = NodeExecutionEnv::new(dir.cwd());
        let result = create_bash_tool::<ExecutionToolContext>(BashToolOptions::default())
            .execute(
                "bash-long-line",
                serde_json::json!({ "command": "printf '%060000d' 0" }),
                CancellationToken::new(),
                None,
                context(env),
            )
            .await
            .unwrap();

        assert!(
            text_output(&result)
                .contains("Showing last 50.0KB of line 1 (line is 58.6KB). Full output:")
        );
    }

    #[tokio::test]
    async fn prepares_command_cwd_and_explicit_environment_with_turn_context() {
        // "prepares command, cwd, and an explicit environment with the turn
        // context" (tools.test.ts:534-561).
        let dir = TempDir::new();
        let cwd = dir.path().to_string_lossy().into_owned();
        let env = NodeExecutionEnv::new(&cwd).with_shell_env(BTreeMap::from([(
            "PI_BASH_PREPARE_INHERITED".to_string(),
            "inherited".to_string(),
        )]));
        env.create_dir(
            "workspace",
            crate::harness::types::CreateDirOptions::default(),
        )
        .await
        .unwrap();
        let workspace = format!("{cwd}/workspace");
        let env_arc: Arc<dyn ExecutionEnv> = Arc::new(env);
        let context = BashTestContext {
            env: Arc::clone(&env_arc),
            workspace: workspace.clone(),
        };
        let signal = CancellationToken::new();

        let received_workspace: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let received_signal: Arc<Mutex<Option<CancellationToken>>> = Arc::new(Mutex::new(None));
        let received_workspace2 = Arc::clone(&received_workspace);
        let received_signal2 = Arc::clone(&received_signal);
        let prepare: BashPrepare<BashTestContext> = Arc::new(
            move |execution, turn_context, signal| {
                *received_workspace2.lock().unwrap() = Some(turn_context.workspace.clone());
                *received_signal2.lock().unwrap() = Some(signal);
                execution.cwd = turn_context.workspace.clone();
                execution.env = BTreeMap::from([(
                    "PI_BASH_PREPARE_EXPLICIT".to_string(),
                    "explicit".to_string(),
                )]);
                execution.inherit_env = false;
                execution.command.push_str(
                    "\nprintf '%s:%s:%s:%s' \"$prefix\" \"${PI_BASH_PREPARE_INHERITED-}\" \"$PI_BASH_PREPARE_EXPLICIT\" \"$PWD\"",
                );
                Box::pin(async {})
            },
        );
        let tool = create_bash_tool::<BashTestContext>(BashToolOptions {
            command_prefix: Some("prefix=ready".to_string()),
            prepare: Some(prepare),
        });

        let result = tool
            .execute(
                "bash-prepare",
                serde_json::json!({ "command": ":" }),
                signal.clone(),
                None,
                context.clone(),
            )
            .await
            .unwrap();

        assert_eq!(
            received_workspace.lock().unwrap().as_deref(),
            Some(workspace.as_str())
        );
        assert_eq!(
            *received_signal.lock().unwrap(),
            Some(signal),
            "prepare must receive the execute signal"
        );
        let canonical = env_arc.canonical_path(&workspace, None).await.unwrap();
        assert_eq!(text_output(&result), format!("ready::explicit:{canonical}"));
    }

    #[tokio::test]
    async fn supports_command_prefixes() {
        // "supports command prefixes" (tools.test.ts:563-574).
        let dir = TempDir::new();
        let env = NodeExecutionEnv::new(dir.cwd());
        let result = create_bash_tool::<ExecutionToolContext>(BashToolOptions {
            command_prefix: Some("value=hello".to_string()),
            ..Default::default()
        })
        .execute(
            "bash-4",
            serde_json::json!({ "command": "printf $value" }),
            CancellationToken::new(),
            None,
            context(env),
        )
        .await
        .unwrap();

        assert_eq!(text_output(&result), "hello");
    }

    #[tokio::test]
    async fn coalesces_updates_and_persists_truncated_full_output() {
        // "coalesces updates and persists truncated full output"
        // (tools.test.ts:576-608).
        let dir = TempDir::new();
        let env: Arc<dyn ExecutionEnv> = Arc::new(NodeExecutionEnv::new(dir.cwd()));
        let updates: Arc<std::sync::Mutex<Vec<Value>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let updates2 = Arc::clone(&updates);
        let result = create_bash_tool::<ExecutionToolContext>(BashToolOptions::default())
            .execute(
                "bash-5",
                serde_json::json!({
                    "command": "i=1; while [ $i -le 3000 ]; do echo line-$i; i=$((i + 1)); done"
                }),
                CancellationToken::new(),
                Some(Box::new(move |update| {
                    updates2
                        .lock()
                        .unwrap()
                        .push(serde_json::to_value(&update).unwrap());
                })),
                ExecutionToolContext::new(Arc::clone(&env)),
            )
            .await
            .unwrap();

        assert!(
            updates.lock().unwrap().len() < 25,
            "updates must be coalesced"
        );
        let truncation = &result.details["truncation"];
        assert_eq!(truncation["truncated"], Value::Bool(true));
        assert_eq!(truncation["truncatedBy"], Value::String("lines".into()));
        assert_eq!(truncation["totalLines"], Value::from(3000));
        assert_eq!(truncation["outputLines"], Value::from(2000));
        assert!(text_output(&result).contains("line-3000"));
        let full_output_path = result.details["fullOutputPath"]
            .as_str()
            .unwrap()
            .to_string();

        let (final_text, final_truncation_total, final_total_bytes, final_full_output_path) = {
            let updates = updates.lock().unwrap();
            let final_update = updates.last().unwrap();
            (
                final_update["content"][0]["text"]
                    .as_str()
                    .map(str::to_owned),
                final_update["details"]["truncation"]["totalLines"].clone(),
                final_update["details"]["truncation"]["totalBytes"].clone(),
                final_update["details"]["fullOutputPath"].clone(),
            )
        };
        assert!(final_text.is_some_and(|text| text.contains("line-3000")));
        assert_eq!(final_truncation_total, Value::from(3000));
        assert!(final_total_bytes.is_number());
        assert_eq!(
            final_full_output_path,
            Value::String(full_output_path.clone())
        );

        let full_output = env.read_text_file(&full_output_path, None).await.unwrap();
        assert!(full_output.contains("line-1\nline-2"));
        assert!(full_output.contains("line-2999\nline-3000"));
    }

    #[tokio::test]
    async fn emits_first_chunk_update_without_throttle_delay() {
        // `lastUpdateAt = 0` upstream (bash.ts:72): the first chunk's
        // `delay = 100 - Date.now()` is `<= 0`, so the first update fires on
        // the leading edge (bash.ts:95-99) — far sooner than the 100ms
        // throttle window.
        let dir = TempDir::new();
        let env = ToolEnv::new(NodeExecutionEnv::new(dir.cwd()));
        let env = env.with_exec_override(Arc::new(|_command, options| {
            Box::pin(async move {
                if let Some(callback) = options.as_ref().and_then(|o| o.on_stdout.as_ref()) {
                    callback("first\n");
                }
                Ok(ShellExecResult {
                    stdout: "first\n".to_string(),
                    stderr: String::new(),
                    exit_code: 0,
                })
            })
        }));
        let started = Instant::now();
        let updates: Arc<Mutex<Vec<(Instant, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let updates2 = Arc::clone(&updates);
        let result = create_bash_tool::<ExecutionToolContext>(BashToolOptions::default())
            .execute(
                "bash-first-chunk",
                serde_json::json!({ "command": "first" }),
                CancellationToken::new(),
                Some(Box::new(move |update| {
                    updates2
                        .lock()
                        .unwrap()
                        .push((Instant::now(), text_output(&update)));
                })),
                ExecutionToolContext::new(Arc::new(env)),
            )
            .await
            .unwrap();
        assert_eq!(text_output(&result), "first\n");
        let updates = updates.lock().unwrap();
        // [0] is the initial empty frame (bash.ts:107); [1] is the first
        // stdout chunk.
        assert!(
            updates.len() >= 2,
            "expected the empty frame plus the first chunk"
        );
        assert_eq!(updates[0].1, "");
        let (arrival, first_chunk) = &updates[1];
        assert_eq!(first_chunk, "first\n");
        assert!(
            arrival.duration_since(started) < Duration::from_millis(50),
            "first chunk update must arrive well before the 100ms throttle, took {:?}",
            arrival.duration_since(started)
        );
    }

    #[tokio::test]
    async fn validates_timeouts() {
        // `validateTimeout` (bash.ts:41-49).
        let tool: BashTool<ExecutionToolContext> =
            create_bash_tool::<ExecutionToolContext>(BashToolOptions::default());
        for (timeout, expected) in [
            (0.0, "Invalid timeout: must be a finite number of seconds"),
            (-5.0, "Invalid timeout: must be a finite number of seconds"),
            (
                2_147_483.648,
                "Invalid timeout: maximum is 2147483.647 seconds",
            ),
        ] {
            let err = tool
                .execute(
                    "bash-timeout-validate",
                    serde_json::json!({ "command": ":", "timeout": timeout }),
                    CancellationToken::new(),
                    None,
                    ExecutionToolContext::new(Arc::new(NodeExecutionEnv::new(
                        TempDir::new().cwd(),
                    ))),
                )
                .await
                .unwrap_err();
            assert!(
                err.to_string().contains(expected),
                "timeout {timeout}: got {err}"
            );
        }
    }

    /// Context with an extra `workspace` field (tools.test.ts:534-560).
    #[derive(Clone)]
    struct BashTestContext {
        env: Arc<dyn ExecutionEnv>,
        workspace: String,
    }

    impl ToolContext for BashTestContext {
        fn env(&self) -> Arc<dyn ExecutionEnv> {
            Arc::clone(&self.env)
        }
    }

    #[test]
    fn tool_metadata() {
        let tool: BashTool<ExecutionToolContext> =
            create_bash_tool::<ExecutionToolContext>(BashToolOptions::default());
        assert_eq!(tool.name(), "bash");
        assert!(tool.description().contains("last 2000 lines or 50KB"));
    }
}
