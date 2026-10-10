//! Executes one advisor review using its private transcript and the same
//! worker tools as the primary, with only read, grep, glob, and advise exposed.

use roost_llm::{ChatRequest, Message, ToolSpec};
use roost_protocol::wire::agent_chat::worker_tool_specs;
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::context_files;
use crate::error::AgentError;
use crate::model_call::{Silent, call_model};
use crate::records::{AdvisorySeverity, ConversationRecord, Role};
use crate::roles::{ResolvedModel, resolve_role};
use crate::runtime::AgentRuntime;
use crate::traits::{ToolCall, ToolOutcome};

use super::delivery;

use super::guard::{Emission, EmissionGuard};
use super::quarantine;

const ADVISOR_SYSTEM: &str = include_str!("../prompts/advisor_system.md");
const ADVISE_TOOL: &str = include_str!("../prompts/advise_tool.md");
const MAX_TOOL_ROUNDS: usize = 8;
const MAX_RETRIES: usize = 3;

#[derive(Debug, Default)]
pub(crate) struct ReviewState {
    pub transcript: Vec<Message>,
    pub cursor: u64,
    pub seeded: bool,
    pub immune_turns_left: u32,
    pub failed_cycles: u32,
    pub reviews: u64,
    pub accepted: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub halted: bool,
    pub no_model_noticed: bool,
    pub quarantine_streak: u8,
    pub guard: EmissionGuard,
}

#[derive(Debug, Deserialize)]
struct AdviseArgs {
    note: String,
    severity: Option<String>,
}

pub(crate) async fn review_with_retries(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
    delta: &str,
    state: &mut ReviewState,
    ended_with_tool_calls: bool,
) {
    if state.halted || delta.trim().is_empty() {
        return;
    }
    for attempt in 0..=MAX_RETRIES {
        let result = review_once(runtime, record, delta, state, ended_with_tool_calls).await;
        match result {
            Ok(quarantined) => {
                state.failed_cycles = 0;
                state.reviews += 1;
                state.quarantine_streak = if quarantined {
                    state.quarantine_streak.saturating_add(1)
                } else {
                    0
                };
                if state.quarantine_streak >= 2 {
                    runtime
                        .notice(
                            &record.id,
                            "warn",
                            "Advisor output quarantined twice; review dropped",
                            "",
                        )
                        .await;
                }
                return;
            }
            Err(error) if attempt < MAX_RETRIES => {
                let waits = &runtime.inner.config.advisor_backoff;
                let wait = waits.get(attempt).copied().unwrap_or_default();
                tracing::warn!(conversation_id = record.id, attempt = attempt + 1, %error, "advisor review failed; retrying");
                tokio::time::sleep(wait).await;
            }
            Err(error) => {
                state.failed_cycles += 1;
                tracing::warn!(conversation_id = record.id, %error, failures = state.failed_cycles, "advisor review dropped after retries");
                if state.failed_cycles >= 3 {
                    state.halted = true;
                    runtime
                        .notice(
                            &record.id,
                            "warn",
                            "Advisor stopped after repeated failures; /advisor on restarts it",
                            "",
                        )
                        .await;
                }
            }
        }
    }
}

async fn review_once(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
    delta: &str,
    state: &mut ReviewState,
    ended_with_tool_calls: bool,
) -> Result<bool, AgentError> {
    let settings = runtime.inner.store.settings().await?;
    let Some(resolved) = resolve_role(runtime.inner.llm.as_ref(), &settings, Role::Advisor).await
    else {
        if !state.no_model_noticed {
            runtime
                .notice(
                    &record.id,
                    "warn",
                    "Advisor has no model",
                    "Configure the advisor role to enable reviews.",
                )
                .await;
            state.no_model_noticed = true;
        }
        return Ok(false);
    };
    state.no_model_noticed = false;
    state.guard.begin_review();
    let cancel = CancellationToken::new();
    let files = context_files::context_files(runtime, record, &cancel).await;
    let system = format!(
        "{}\n\n<project-context>\n{}\n</project-context>\n\n<watchdog>\n{}\n</watchdog>",
        ADVISOR_SYSTEM, files.context, files.watchdog
    );
    let mut transcript = state.transcript.clone();
    transcript.push(Message::user_text(format!(
        "<primary-transcript-delta>\n{delta}\n</primary-transcript-delta>"
    )));
    let primary_needs_more_work =
        ended_with_tool_calls || delta.contains("Continue: you stopped before finishing the task.");
    let tools = advisor_specs();
    let mut generated = Vec::new();
    let mut notes = Vec::new();
    for _round in 0..MAX_TOOL_ROUNDS {
        let request = ChatRequest {
            model: resolved.info.clone(),
            system: vec![system.clone()],
            messages: transcript.clone(),
            tools: tools.clone(),
            thinking: thinking_level(&resolved),
            session_id: format!("{}-advisor", record.id),
            max_tokens: None,
        };
        let output =
            call_model(runtime, &request.session_id, &request, &cancel, &mut Silent).await?;
        state.input_tokens = state.input_tokens.saturating_add(output.usage.input);
        state.output_tokens = state.output_tokens.saturating_add(output.usage.output);
        runtime
            .add_extra_usage(
                &record.id,
                &resolved.info.provider,
                &resolved.info.id,
                &output.usage,
            )
            .await;
        let calls = output.tool_calls();
        generated.push(output.text());
        transcript.push(Message::Assistant {
            blocks: output.blocks.clone(),
        });
        if calls.is_empty() {
            state.transcript = transcript;
            return deliver_review(
                runtime,
                record,
                state,
                &notes,
                &generated.join("\n"),
                delta,
                primary_needs_more_work,
            )
            .await;
        }
        let mut results = Vec::new();
        for (call_id, name, args_json) in calls {
            let (text, is_error) = if name == "advise" {
                let result = run_advise(&args_json, &mut state.guard);
                match result {
                    Ok((status, note, severity)) => {
                        generated.push(note.clone());
                        if status == Emission::Accepted {
                            notes.push((note, severity));
                        }
                        (status.as_str().to_owned(), false)
                    }
                    Err(error) => (error, true),
                }
            } else if matches!(name.as_str(), "read" | "grep" | "glob") {
                let outcome =
                    worker_call(runtime, record, &call_id, &name, &args_json, &cancel).await;
                (outcome.content, outcome.is_error)
            } else {
                (
                    format!("Tool {name} is not available to the advisor."),
                    true,
                )
            };
            results.push(Message::ToolResult {
                call_id,
                tool_name: name,
                text,
                is_error,
            });
        }
        transcript.extend(results);
    }
    state.transcript = transcript;
    deliver_review(
        runtime,
        record,
        state,
        &notes,
        &generated.join("\n"),
        delta,
        primary_needs_more_work,
    )
    .await
}

fn advisor_specs() -> Vec<ToolSpec> {
    let mut specs = worker_tool_specs(true)
        .into_iter()
        .filter(|spec| matches!(spec.name.as_str(), "read" | "grep" | "glob"))
        .map(|spec| ToolSpec {
            name: spec.name,
            description: spec.description,
            parameters: spec.parameters,
        })
        .collect::<Vec<_>>();
    specs.push(ToolSpec {
        name: "advise".into(), description: ADVISE_TOOL.into(),
        parameters: serde_json::json!({"type":"object","properties":{"note":{"type":"string"},"severity":{"type":"string","enum":["nit","concern","blocker"]}},"required":["note"],"additionalProperties":false}),
    });
    specs
}

fn thinking_level(model: &ResolvedModel) -> String {
    let supported = model.info.supported_thinking_levels();
    if model
        .thinking
        .as_deref()
        .is_some_and(|level| supported.contains(&level))
    {
        return model.thinking.clone().unwrap_or_else(|| "medium".into());
    }
    if supported.contains(&"medium") {
        "medium".into()
    } else {
        supported.first().copied().unwrap_or("off").into()
    }
}

fn run_advise(
    args: &str,
    guard: &mut EmissionGuard,
) -> Result<(Emission, String, AdvisorySeverity), String> {
    let parsed: AdviseArgs =
        serde_json::from_str(args).map_err(|error| format!("Invalid advise arguments: {error}"))?;
    let severity = parsed
        .severity
        .as_deref()
        .and_then(AdvisorySeverity::from_name)
        .unwrap_or(AdvisorySeverity::Concern);
    let status = guard.check(&parsed.note, severity);
    Ok((status, parsed.note, severity))
}

async fn worker_call(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
    call_id: &str,
    tool: &str,
    args: &str,
    cancel: &CancellationToken,
) -> ToolOutcome {
    let (out, mut receiver) = mpsc::channel(8);
    let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
    let outcome = runtime
        .inner
        .tools
        .execute(
            &record.worker_fp,
            ToolCall {
                call_id: call_id.to_owned(),
                conversation_id: format!("{}-advisor", record.id),
                cwd: record.cwd.clone(),
                tool: tool.to_owned(),
                args_json: args.to_owned(),
                timeout_ms: 120_000,
            },
            out,
            cancel.child_token(),
        )
        .await
        .unwrap_or_else(|error| ToolOutcome {
            is_error: true,
            content: error,
            details_json: "{}".into(),
        });
    let _ = drain.await;
    outcome
}

async fn deliver_review(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
    state: &mut ReviewState,
    notes: &[(String, AdvisorySeverity)],
    generated: &str,
    delta: &str,
    ended_with_tool_calls: bool,
) -> Result<bool, AgentError> {
    let current = runtime.record(&record.id).await?;
    let settings = runtime.inner.store.settings().await?;
    if current.parent_id.is_some() || !current.advisor.unwrap_or(settings.advisor_enabled) {
        return Ok(false);
    }
    if let Some(reason) = quarantine::unsafe_output(generated, delta) {
        state.transcript.clear();
        runtime
            .notice(&record.id, "warn", "Advisor response quarantined", &reason)
            .await;
        return Ok(true);
    }
    state.quarantine_streak = 0;
    state.accepted += notes.len() as u64;
    delivery::deliver(runtime, &current, state, notes, ended_with_tool_calls).await?;
    Ok(false)
}
