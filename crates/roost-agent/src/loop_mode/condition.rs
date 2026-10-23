//! The `--while`/`--until` gate. Runs the condition command on the worker and
//! turns its exit status into a verdict on whether the loop repeats. Only exit
//! codes 0 and 1 are conditions, as in OMP; any other status stops the loop
//! rather than guessing which way to go.

use roost_protocol::wire::agent_chat::TOOL_BASH;
use serde_json::json;
use tokio::sync::mpsc;

use super::parse::Condition;
use super::state::LoopCheck;
use crate::records::ConversationRecord;
use crate::runtime::AgentRuntime;
use crate::traits::{OwnerConversationId, ToolCall, ToolOutcome, WorkerConversationNamespace};

const CHECK_TIMEOUT_SECS: u32 = 120;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    Continue,
    Halt(String),
    Error(String),
    Cancelled,
}

/// Maps a condition's exit status to a verdict. `Err` is a run that produced
/// no exit status: it was killed, or it never started.
pub(crate) fn decide(condition: &Condition, outcome: Result<i64, String>) -> Verdict {
    let code = match outcome {
        Ok(code) => code,
        Err(detail) => {
            return Verdict::Error(format!(
                "condition `{}` did not exit: {detail}",
                condition.command
            ));
        }
    };
    if code != 0 && code != 1 {
        return Verdict::Error(format!(
            "condition `{}` exited {code}; only 0 and 1 are conditions.",
            condition.command
        ));
    }
    let passed = code == 0;
    // `--while` repeats while the command passes; `--until` repeats while it fails.
    let repeat = if condition.until { !passed } else { passed };
    if repeat {
        return Verdict::Continue;
    }
    if condition.until {
        Verdict::Halt(format!(
            "Loop condition met: `{}` exited 0. Loop mode disabled.",
            condition.command
        ))
    } else {
        Verdict::Halt(format!(
            "Loop condition failed: `{}` exited 1. Loop mode disabled.",
            condition.command
        ))
    }
}

/// Runs the condition on the conversation's worker. Cancelling the check's
/// token (a user abort, or the loop ending) yields `Cancelled` rather than a
/// verdict.
pub(crate) async fn evaluate(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
    condition: &Condition,
    check: &LoopCheck,
) -> Verdict {
    let (out, mut receiver) = mpsc::channel(8);
    let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
    let call = ToolCall {
        call_id: check.id.clone(),
        conversation_id: WorkerConversationNamespace(format!("{}-loop", record.id)),
        owner_conversation_id: OwnerConversationId(record.id.clone()),
        cwd: record.cwd.clone(),
        tool: TOOL_BASH.to_owned(),
        args_json: json!({ "command": condition.command, "timeout": CHECK_TIMEOUT_SECS })
            .to_string(),
        timeout_ms: CHECK_TIMEOUT_SECS * 1000,
        checkpoint_id: 0,
    };
    let result = runtime
        .inner
        .tools
        .execute(&record.worker_fp, call, out, check.token.clone())
        .await;
    let _ = drain.await;
    if check.token.is_cancelled() {
        return Verdict::Cancelled;
    }
    match result {
        Ok(outcome) => decide(condition, exit_status(&outcome)),
        Err(error) => Verdict::Error(format!("the condition could not run: {error}")),
    }
}

/// The exit code from the bash tool's structured details. A run with no exit
/// code is reported by its first output line.
pub(crate) fn exit_status(outcome: &ToolOutcome) -> Result<i64, String> {
    let details: serde_json::Value =
        serde_json::from_str(&outcome.details_json).unwrap_or(serde_json::Value::Null);
    match details.get("exit_code").and_then(serde_json::Value::as_i64) {
        Some(code) => Ok(code),
        None => Err(outcome
            .content
            .lines()
            .next()
            .unwrap_or("no output")
            .to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate(until: bool) -> Condition {
        Condition {
            command: "bun test".to_owned(),
            until,
        }
    }

    #[test]
    fn until_halts_once_the_command_passes() {
        assert_eq!(
            decide(&gate(true), Ok(0)),
            Verdict::Halt(
                "Loop condition met: `bun test` exited 0. Loop mode disabled.".to_owned()
            )
        );
        assert_eq!(decide(&gate(true), Ok(1)), Verdict::Continue);
    }

    #[test]
    fn while_halts_once_the_command_fails() {
        assert_eq!(decide(&gate(false), Ok(0)), Verdict::Continue);
        assert_eq!(
            decide(&gate(false), Ok(1)),
            Verdict::Halt(
                "Loop condition failed: `bun test` exited 1. Loop mode disabled.".to_owned()
            )
        );
    }

    #[test]
    fn other_exit_codes_stop_with_an_error() {
        assert_eq!(
            decide(&gate(true), Ok(2)),
            Verdict::Error(
                "condition `bun test` exited 2; only 0 and 1 are conditions.".to_owned()
            )
        );
    }

    #[test]
    fn a_run_without_an_exit_code_stops_with_an_error() {
        assert_eq!(
            decide(&gate(false), Err("killed".to_owned())),
            Verdict::Error("condition `bun test` did not exit: killed".to_owned())
        );
    }

    #[test]
    fn exit_status_reads_the_bash_details_or_the_first_output_line() {
        let exited = ToolOutcome {
            is_error: false,
            content: "ok".to_owned(),
            details_json: r#"{"exit_code":1}"#.to_owned(),
        };
        assert_eq!(exit_status(&exited), Ok(1));
        let unstructured = ToolOutcome {
            is_error: true,
            content: "boom\nmore".to_owned(),
            details_json: "{}".to_owned(),
        };
        assert_eq!(exit_status(&unstructured), Err("boom".to_owned()));
    }
}
