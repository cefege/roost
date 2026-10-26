//! Bash and PowerShell tools (port of `packages/durable/src/tools/bash.ts`
//! @ `abe508e1b`).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rpi_ai::types::Tool;
use serde_json::{Value, json};

use crate::chord::context::Context;
use crate::chord::json::JsonObject;
use crate::env::types::{
    ExecutionErrorCode, ShellCommand, ShellExecOptions, ShellExecResult, ShellSpillOptions,
};
use crate::harness::types::{
    DiagnosticSeverity, OutputLimits, OutputRetain, ToolDiagnostic, ToolExecute, ToolExecuteError,
    ToolExecutionApi, ToolExecutionResult, ToolRegistration, ToolReplay,
};
use crate::tools::env::require_env;
use crate::tools::tool_error;
use crate::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES};

const MAX_TIMEOUT_SECONDS: f64 = 2_147_483_647.0 / 1000.0;

/// A command about to run, which `prepare` may change: the script, its working
/// directory and environment (`BashExecution`).
#[derive(Debug, Clone, PartialEq)]
pub struct BashExecution {
    /// The script to run (with any command prefix).
    pub command: String,
    /// Working directory.
    pub cwd: String,
    /// Extra environment variables.
    pub env: BTreeMap<String, String>,
    /// Inherit the host environment.
    pub inherit_env: bool,
}

/// Hook that may change a command before it runs (`BashPrepare`).
#[async_trait]
pub trait BashPrepare: Send + Sync {
    /// Change `execution` in place.
    async fn prepare(
        &self,
        execution: &mut BashExecution,
        api: &dyn ToolExecutionApi,
        context: &Context,
    );
}

/// Options of [`create_bash_tool`] (`BashToolOptions`).
#[derive(Default)]
pub struct BashToolOptions {
    /// Lines run before each command.
    pub command_prefix: Option<String>,
    /// Hook that may change each command.
    pub prepare: Option<Arc<dyn BashPrepare>>,
}

/// Options of [`create_power_shell_tool`] (`PowerShellToolOptions`).
#[derive(Default)]
pub struct PowerShellToolOptions {
    /// Lines run before each command.
    pub command_prefix: Option<String>,
    /// Hook that may change each command.
    pub prepare: Option<Arc<dyn BashPrepare>>,
    /// PowerShell programs to try in order; default `pwsh`, then `powershell`.
    pub programs: Option<Vec<String>>,
}

struct Options {
    command_prefix: Option<String>,
    prepare: Option<Arc<dyn BashPrepare>>,
}

fn validate_timeout(timeout: Option<f64>) -> Result<(), ToolExecuteError> {
    let Some(timeout) = timeout else {
        return Ok(());
    };
    if !timeout.is_finite() || timeout <= 0.0 {
        return Err(tool_error(
            "Invalid timeout: must be a finite number of seconds",
        ));
    }
    if timeout > MAX_TIMEOUT_SECONDS {
        return Err(tool_error(format!(
            "Invalid timeout: maximum is {MAX_TIMEOUT_SECONDS} seconds"
        )));
    }
    Ok(())
}

/// The execution of one call: the command with its prefix, in the
/// environment's working directory, then `prepare`.
async fn prepare_execution(
    command: &str,
    options: &Options,
    api: &dyn ToolExecutionApi,
    context: &Context,
) -> Result<BashExecution, ToolExecuteError> {
    let env = require_env(api)?;
    let mut execution = BashExecution {
        command: match &options.command_prefix {
            Some(prefix) => format!("{prefix}\n{command}"),
            None => command.to_string(),
        },
        cwd: env.cwd(),
        env: BTreeMap::new(),
        inherit_env: true,
    };
    if let Some(prepare) = &options.prepare {
        prepare.prepare(&mut execution, api, context).await;
    }
    Ok(execution)
}

/// JS template rendering of a finite timeout.
fn timeout_text(timeout: f64) -> String {
    if timeout.fract() == 0.0 {
        format!("{}", timeout as i64)
    } else {
        format!("{timeout}")
    }
}

/// Run each command form in turn until one starts, streaming output to
/// `api.output()` within the retained window, and turn the result into the
/// tool's outcome.
async fn run_command(
    commands: Vec<ShellCommand>,
    execution: &BashExecution,
    timeout: Option<f64>,
    api: &dyn ToolExecutionApi,
    context: &Context,
) -> Result<(), ToolExecuteError> {
    let env = require_env(api)?;
    let mut result: Option<Result<ShellExecResult, crate::env::types::ExecutionError>> = None;
    for command in commands {
        let callback_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let captured = Arc::clone(&callback_error);
        let options = ShellExecOptions {
            cwd: Some(execution.cwd.clone()),
            env: execution.env.clone(),
            inherit_env: execution.inherit_env,
            timeout,
            on_output: Some(Arc::new(move |text, _context, info| {
                if let Err(error) = api.output(text.as_bytes(), info.skipped)
                    && let Ok(mut slot) = captured.lock()
                {
                    *slot = Some(error);
                }
            })),
            spill: Some(ShellSpillOptions {
                after_bytes: DEFAULT_MAX_BYTES,
                after_lines: DEFAULT_MAX_LINES,
            }),
            // An environment may omit output outside the retained tail.
            window: api.output_window(),
        };
        let executed = env.exec(command, Some(options), context).await;
        if let Ok(mut slot) = callback_error.lock()
            && let Some(error) = slot.take()
        {
            return Err(tool_error(error));
        }
        // A program that could not start produced no output; the next form may.
        let stop = match &executed {
            Ok(_) => true,
            Err(error) => error.code != ExecutionErrorCode::SpawnError,
        };
        result = Some(executed);
        if stop {
            break;
        }
    }
    let Some(result) = result else {
        return Err(tool_error("No command to run"));
    };
    let spill_path = match &result {
        Ok(value) => value.spill_path.clone(),
        Err(error) => error.spill_path.clone(),
    };
    if let Some(spill_path) = spill_path {
        api.diagnostic(ToolDiagnostic {
            severity: DiagnosticSeverity::Info,
            code: Some("full_output".to_string()),
            message: format!("Full output: {spill_path}"),
        });
    }
    match result {
        Ok(value) => {
            if value.exit_code != 0 {
                Err(tool_error(format!(
                    "Command exited with code {}",
                    value.exit_code
                )))
            } else {
                Ok(())
            }
        }
        Err(error) => {
            if error.code == ExecutionErrorCode::Aborted && context_aborted(context) {
                return Err(Box::new(error));
            }
            match error.code {
                ExecutionErrorCode::Timeout => Err(tool_error(format!(
                    "Command timed out after {} seconds",
                    timeout.map(timeout_text).unwrap_or_default()
                ))),
                ExecutionErrorCode::Aborted => Err(tool_error("Command aborted")),
                _ => Err(Box::new(error)),
            }
        }
    }
}

fn context_aborted(context: &Context) -> bool {
    context
        .abort_signal()
        .map(|signal| signal.is_cancelled())
        .unwrap_or(false)
}

fn bash_description() -> String {
    format!(
        "Execute a bash command in the current working directory. Returns combined stdout and stderr. Output is truncated to last {} lines or {}KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.",
        DEFAULT_MAX_LINES,
        DEFAULT_MAX_BYTES / 1024
    )
}

fn powershell_description() -> String {
    format!(
        "Execute a PowerShell command in the current working directory. Returns combined stdout and stderr. Output is truncated to last {} lines or {}KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.",
        DEFAULT_MAX_LINES,
        DEFAULT_MAX_BYTES / 1024
    )
}

fn bash_parameters(command_name: &str) -> Value {
    json!({
        "type": "object",
        "properties": {
            "command": {
                "type": "string",
                "description": format!("{command_name} command to execute")
            },
            "timeout": {
                "type": "number",
                "description": "Timeout in seconds (optional, no default timeout)"
            }
        },
        "required": ["command"]
    })
}

fn tail_limits() -> Option<OutputLimits> {
    Some(OutputLimits {
        max_bytes: None,
        max_lines: None,
        retain: Some(OutputRetain::Tail),
    })
}

struct BashTool {
    options: Options,
}

#[async_trait]
impl ToolExecute for BashTool {
    async fn execute(
        &self,
        args: JsonObject,
        api: &dyn ToolExecutionApi,
        context: &Context,
    ) -> Result<ToolExecutionResult, ToolExecuteError> {
        let command = args
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let timeout = args.get("timeout").and_then(Value::as_f64);
        validate_timeout(timeout)?;
        let execution = prepare_execution(command, &self.options, api, context).await?;
        run_command(
            vec![ShellCommand::Shell(execution.command.clone())],
            &execution,
            timeout,
            api,
            context,
        )
        .await?;
        Ok(ToolExecutionResult::default())
    }
}

struct PowerShellTool {
    options: Options,
    programs: Vec<String>,
}

/// Output in UTF-8 whatever the console's code page, as the coding agent's
/// `powershell` tool does.
const UTF8_OUTPUT: &str = "try { [Console]::OutputEncoding=[System.Text.Encoding]::UTF8 } catch {}";
const POWERSHELL_ARGS: [&str; 5] = [
    "-NoProfile",
    "-NonInteractive",
    "-ExecutionPolicy",
    "Bypass",
    "-Command",
];

#[async_trait]
impl ToolExecute for PowerShellTool {
    async fn execute(
        &self,
        args: JsonObject,
        api: &dyn ToolExecutionApi,
        context: &Context,
    ) -> Result<ToolExecutionResult, ToolExecuteError> {
        let command = args
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let timeout = args.get("timeout").and_then(Value::as_f64);
        validate_timeout(timeout)?;
        let execution = prepare_execution(command, &self.options, api, context).await?;
        let script = format!("{UTF8_OUTPUT}\n{}", execution.command);
        let commands = self
            .programs
            .iter()
            .map(|program| {
                let mut argv = Vec::with_capacity(POWERSHELL_ARGS.len() + 2);
                argv.push(program.clone());
                for argument in POWERSHELL_ARGS {
                    argv.push(argument.to_string());
                }
                argv.push(script.clone());
                ShellCommand::Argv(argv)
            })
            .collect();
        run_command(commands, &execution, timeout, api, context).await?;
        Ok(ToolExecutionResult::default())
    }
}

/// `createBashTool` (bash.ts:125).
pub fn create_bash_tool(options: Option<BashToolOptions>) -> ToolRegistration {
    let options = options.unwrap_or_default();
    ToolRegistration {
        tool: Tool {
            name: "bash".to_string(),
            description: bash_description(),
            parameters: bash_parameters("Bash"),
            constrained_sampling: None,
        },
        replay: ToolReplay::Unsafe,
        execution_mode: None,
        deferred: false,
        prepare_arguments: None,
        output_limits: tail_limits(),
        structured_output_schema: None,
        callers: None,
        execute: Arc::new(BashTool {
            options: Options {
                command_prefix: options.command_prefix,
                prepare: options.prepare,
            },
        }),
    }
}

/// `createPowerShellTool` (bash.ts:148).
pub fn create_power_shell_tool(options: Option<PowerShellToolOptions>) -> ToolRegistration {
    let options = options.unwrap_or_default();
    ToolRegistration {
        tool: Tool {
            name: "powershell".to_string(),
            description: powershell_description(),
            parameters: bash_parameters("PowerShell"),
            constrained_sampling: None,
        },
        replay: ToolReplay::Unsafe,
        execution_mode: None,
        deferred: false,
        prepare_arguments: None,
        output_limits: tail_limits(),
        structured_output_schema: None,
        callers: None,
        execute: Arc::new(PowerShellTool {
            options: Options {
                command_prefix: options.command_prefix,
                prepare: options.prepare,
            },
            programs: options
                .programs
                .unwrap_or_else(|| vec!["pwsh".to_string(), "powershell".to_string()]),
        }),
    }
}
