//! The run loop of one conversation: inject steered messages between turns,
//! take turns until the model stops, then apply the end-of-turn policies —
//! unexpected-stop continuation and a subagent's yield reminders.

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::durable::DurableDriver;
use crate::error::AgentError;
use crate::model_call::ModelOutput;
use crate::prompts;
use crate::roles::ResolvedModel;
use crate::runtime::{AgentRuntime, RunEnd, Steer};
use crate::turn::{RunMemory, TurnOutcome};
use crate::unexpected_stop;

const MAX_YIELD_REMINDERS: u32 = 3;

#[derive(Debug)]
pub(crate) struct RecoveredTextTurn {
    pub output: ModelOutput,
    pub model: Option<ResolvedModel>,
}

#[derive(Debug)]
pub(crate) struct RecoveredRun {
    pub continued: bool,
    pub yield_reminders: u32,
    pub pending_text: Option<RecoveredTextTurn>,
}

pub(crate) fn yield_reminder_attempt(text: &str) -> Option<u32> {
    let (prefix, template_tail) = prompts::YIELD_REMINDER.split_once("{{attempt}}")?;
    let (between, suffix) = template_tail.split_once("{{max_attempts}}")?;
    let remaining = text.strip_prefix(prefix)?;
    let (attempt, remaining) = remaining.split_once(between)?;
    let maximum = remaining.strip_suffix(suffix)?.parse::<u32>().ok()?;
    let attempt = attempt.parse::<u32>().ok()?;
    (maximum == MAX_YIELD_REMINDERS && (1..=MAX_YIELD_REMINDERS).contains(&attempt))
        .then_some(attempt)
}

pub(crate) async fn run_conversation(
    runtime: AgentRuntime,
    id: String,
    cancel: CancellationToken,
    recovered: Option<RecoveredRun>,
    steer_rx: &mut mpsc::UnboundedReceiver<Steer>,
) -> RunEnd {
    let driver = match DurableDriver::open(runtime.clone(), id.clone()).await {
        Ok(driver) => driver,
        Err(error) => return RunEnd::Failed(error.to_string()),
    };
    let end = drive(&runtime, &id, &cancel, recovered, steer_rx, &driver).await;
    let closing = driver
        .close(matches!(end, RunEnd::Aborted) || cancel.is_cancelled())
        .await;
    if let Err(error) = closing {
        tracing::error!(conversation_id = id, %error, "durable conversation close failed");
        if !matches!(end, RunEnd::Failed(_)) {
            return RunEnd::Failed(error.to_string());
        }
    }
    end
}

async fn drive(
    runtime: &AgentRuntime,
    id: &str,
    cancel: &CancellationToken,
    mut recovered: Option<RecoveredRun>,
    steer_rx: &mut mpsc::UnboundedReceiver<Steer>,
    driver: &DurableDriver,
) -> RunEnd {
    let mut memory = RunMemory::default();
    loop {
        if cancel.is_cancelled() {
            return RunEnd::Aborted;
        }
        if let Some(recovered) = recovered.take() {
            memory.continued = recovered.continued;
            memory.yield_reminders = recovered.yield_reminders;
            if let Some(turn) = recovered.pending_text {
                match finish_text_turn(
                    runtime,
                    id,
                    cancel,
                    steer_rx,
                    &mut memory,
                    turn.output,
                    turn.model,
                )
                .await
                {
                    Ok(Some(end)) => return end,
                    Ok(None) => {}
                    Err(error) => return RunEnd::Failed(error.to_string()),
                }
            }
        }
        if let Err(error) = inject_steered(runtime, id, steer_rx, &mut memory).await {
            return RunEnd::Failed(error.to_string());
        }
        let outcome = match driver.generate(&mut memory, cancel).await {
            Ok(outcome) => outcome,
            Err(AgentError::Cancelled) => return RunEnd::Aborted,
            Err(_) if cancel.is_cancelled() => return RunEnd::Aborted,
            Err(error) => return RunEnd::Failed(error.to_string()),
        };
        match outcome {
            TurnOutcome::NoWork => return RunEnd::Completed,
            TurnOutcome::Tools { end: Some(end) } => return end,
            TurnOutcome::Tools { end: None } => {}
            TurnOutcome::Text { output, model } => {
                match finish_text_turn(
                    runtime,
                    id,
                    cancel,
                    steer_rx,
                    &mut memory,
                    *output,
                    Some(*model),
                )
                .await
                {
                    Ok(Some(end)) => return end,
                    Ok(None) => {}
                    Err(error) => return RunEnd::Failed(error.to_string()),
                }
            }
        }
    }
}

async fn finish_text_turn(
    runtime: &AgentRuntime,
    id: &str,
    cancel: &CancellationToken,
    steer_rx: &mut mpsc::UnboundedReceiver<Steer>,
    memory: &mut RunMemory,
    output: ModelOutput,
    model: Option<ResolvedModel>,
) -> Result<Option<RunEnd>, AgentError> {
    if inject_steered(runtime, id, steer_rx, memory).await? > 0 {
        enqueue_advisor_review(runtime, id, false).await;
        return Ok(None);
    }
    let record = runtime.record(id).await?;
    if record.agent.is_some() {
        if memory.yield_reminders >= MAX_YIELD_REMINDERS {
            enqueue_advisor_review(runtime, id, true).await;
            return Ok(Some(RunEnd::Yielded(output.text())));
        }
        memory.yield_reminders += 1;
        let reminder = prompts::render(
            prompts::YIELD_REMINDER,
            &[
                ("attempt", &memory.yield_reminders.to_string()),
                ("max_attempts", &MAX_YIELD_REMINDERS.to_string()),
            ],
        );
        runtime
            .append(
                id,
                crate::records::Entry::User {
                    text: reminder,
                    loop_repeat: false,
                    item_id: None,
                },
            )
            .await?;
        enqueue_advisor_review(runtime, id, false).await;
        return Ok(None);
    }
    if cancel.is_cancelled() {
        return Ok(Some(RunEnd::Aborted));
    }
    if !memory.continued
        && let Some(model) = &model
        && unexpected_stop::stopped_unexpectedly(runtime, id, &model.info, &output).await
    {
        memory.continued = true;
        tracing::info!(
            conversation_id = id,
            "unexpected stop detected; continuing once"
        );
        let nudge = crate::records::Entry::User {
            text: prompts::UNEXPECTED_STOP.to_owned(),
            loop_repeat: false,
            item_id: None,
        };
        runtime.append(id, nudge).await?;
        enqueue_advisor_review(runtime, id, false).await;
        return Ok(None);
    }
    if runtime.refresh_goal(id).await?.is_some_and(|goal| {
        goal.status == roost_protocol::wire::agent_chat::GoalStatus::BudgetLimited
            || (goal.status == roost_protocol::wire::agent_chat::GoalStatus::Active
                && goal.waiting_reason.is_none())
    }) {
        if inject_steered(runtime, id, steer_rx, memory).await? > 0 {
            enqueue_advisor_review(runtime, id, false).await;
            return Ok(None);
        }
        tokio::select! {
            () = cancel.cancelled() => return Ok(Some(RunEnd::Aborted)),
            accepted = steer_rx.recv() => {
                if let Some(accepted) = accepted {
                    memory.pending_steers.push_back(accepted);
                    if inject_steered(runtime, id, steer_rx, memory).await? > 0 {
                        enqueue_advisor_review(runtime, id, false).await;
                        return Ok(None);
                    }
                }
            }
            () = tokio::time::sleep(std::time::Duration::from_millis(800)) => {}
        }
        if runtime.continue_goal_after_turn(id, steer_rx).await? {
            enqueue_advisor_review(runtime, id, false).await;
            return Ok(None);
        }
    }
    enqueue_advisor_review(runtime, id, true).await;
    Ok(Some(RunEnd::Completed))
}

async fn enqueue_advisor_review(runtime: &AgentRuntime, id: &str, terminal: bool) {
    let entries = match runtime.inner.store.entries(id).await {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(conversation_id = id, %error, "advisor response boundary unavailable");
            return;
        }
    };
    if let Some((assistant_seq, input_through_seq)) =
        entries.iter().rev().find_map(|(seq, entry)| match entry {
            crate::records::Entry::Assistant {
                metadata: Some(metadata),
                ..
            } if metadata.completed => Some((*seq, metadata.input_through_seq)),
            _ => None,
        })
    {
        runtime.advisor_turn_ended(
            id,
            false,
            Some(crate::advisor::AssistantResponseBoundary {
                assistant_seq,
                input_through_seq,
                terminal,
            }),
        );
    }
}

async fn inject_steered(
    runtime: &AgentRuntime,
    id: &str,
    steer_rx: &mut mpsc::UnboundedReceiver<Steer>,
    memory: &mut RunMemory,
) -> Result<usize, AgentError> {
    let mut injected = 0;
    loop {
        let Some(message) = memory
            .pending_steers
            .pop_front()
            .or_else(|| steer_rx.try_recv().ok())
        else {
            break;
        };
        if let Steer::Persisted { entry, sequence } = message {
            let entries = runtime.inner.store.entries(id).await?;
            let still_present = crate::journal::effective_entries(&entries).iter().any(
                |(candidate_sequence, candidate)| {
                    *candidate_sequence == sequence && candidate == &entry
                },
            );
            if !still_present {
                continue;
            }
            if matches!(entry, crate::records::Entry::User { .. }) {
                let consumed_through = entries
                    .iter()
                    .filter_map(|(_, candidate)| match candidate {
                        crate::records::Entry::InputConsumed { through_seq } => Some(*through_seq),
                        crate::records::Entry::Assistant {
                            metadata: Some(metadata),
                            ..
                        } => Some(metadata.input_through_seq),
                        _ => None,
                    })
                    .max()
                    .unwrap_or(0);
                if sequence <= consumed_through {
                    continue;
                }
            }
            injected += 1;
            runtime.note_processed_seq(id, sequence);
        }
    }
    Ok(injected)
}
