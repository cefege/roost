//! The run loop of one conversation: inject steered messages between turns,
//! take turns until the model stops, then apply the end-of-turn policies —
//! unexpected-stop continuation and a subagent's yield reminders.

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::error::AgentError;
use crate::prompts;
use crate::runtime::{AgentRuntime, RunEnd, Steer};
use crate::turn::{RunMemory, TurnOutcome, take_turn};
use crate::unexpected_stop;

const MAX_YIELD_REMINDERS: u32 = 3;

pub(crate) async fn run_conversation(
    runtime: AgentRuntime,
    id: String,
    cancel: CancellationToken,
    mut steer_rx: mpsc::UnboundedReceiver<Steer>,
) -> (RunEnd, mpsc::UnboundedReceiver<Steer>) {
    let end = drive(&runtime, &id, &cancel, &mut steer_rx).await;
    (end, steer_rx)
}

async fn drive(
    runtime: &AgentRuntime,
    id: &str,
    cancel: &CancellationToken,
    steer_rx: &mut mpsc::UnboundedReceiver<Steer>,
) -> RunEnd {
    let mut memory = RunMemory::default();
    loop {
        if cancel.is_cancelled() {
            return RunEnd::Aborted;
        }
        if let Err(error) = inject_steered(runtime, id, steer_rx, &mut memory).await {
            return RunEnd::Failed(error.to_string());
        }
        let outcome = match take_turn(runtime, id, &mut memory, cancel).await {
            Ok(outcome) => outcome,
            Err(AgentError::Cancelled) => return RunEnd::Aborted,
            Err(_) if cancel.is_cancelled() => return RunEnd::Aborted,
            Err(error) => return RunEnd::Failed(error.to_string()),
        };
        match outcome {
            TurnOutcome::Tools { end: Some(end) } => return end,
            TurnOutcome::Tools { end: None } => {}
            TurnOutcome::Text { output, model } => {
                if !steer_rx.is_empty() {
                    continue;
                }
                let record = match runtime.record(id).await {
                    Ok(record) => record,
                    Err(error) => return RunEnd::Failed(error.to_string()),
                };
                if record.agent.is_some() {
                    if memory.yield_reminders >= MAX_YIELD_REMINDERS {
                        return RunEnd::Yielded(output.text());
                    }
                    memory.yield_reminders += 1;
                    let reminder = prompts::render(
                        prompts::YIELD_REMINDER,
                        &[
                            ("attempt", &memory.yield_reminders.to_string()),
                            ("max_attempts", &MAX_YIELD_REMINDERS.to_string()),
                        ],
                    );
                    if let Err(error) = runtime
                        .append(id, crate::records::Entry::User { text: reminder })
                        .await
                    {
                        return RunEnd::Failed(error.to_string());
                    }
                    continue;
                }
                if !memory.continued
                    && unexpected_stop::stopped_unexpectedly(runtime, id, &model.info, &output)
                        .await
                {
                    memory.continued = true;
                    tracing::info!(
                        conversation_id = id,
                        "unexpected stop detected; continuing once"
                    );
                    let nudge = crate::records::Entry::User {
                        text: prompts::UNEXPECTED_STOP.to_owned(),
                    };
                    if let Err(error) = runtime.append(id, nudge).await {
                        return RunEnd::Failed(error.to_string());
                    }
                    continue;
                }
                return RunEnd::Completed;
            }
        }
    }
}

async fn inject_steered(
    runtime: &AgentRuntime,
    id: &str,
    steer_rx: &mut mpsc::UnboundedReceiver<Steer>,
    memory: &mut RunMemory,
) -> Result<(), AgentError> {
    while let Ok(message) = steer_rx.try_recv() {
        if matches!(message, Steer::User(_)) {
            memory.new_submission();
        }
        runtime.append(id, message.into_entry()).await?;
    }
    Ok(())
}
