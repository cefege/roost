//! Applies advisor notes to the primary conversation according to mode,
//! primary run state, abort state, severity, and the temporary immunity window.

use crate::error::AgentError;
use crate::records::{AdvisorySeverity, ConversationRecord, Entry, Mode};
use crate::runtime::{AgentRuntime, Steer, random_id};

use super::review::ReviewState;

pub(crate) async fn deliver(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
    state: &mut ReviewState,
    notes: &[(String, AdvisorySeverity)],
    ended_with_tool_calls: bool,
) -> Result<(), AgentError> {
    let mut started_run = false;
    for (note, severity) in notes {
        let item_id = random_id("adv_");
        let mut output_severity = *severity;
        let mut deliver_now = false;
        if *severity == AdvisorySeverity::Nit {
            deliver_now = true;
        } else if state.immune_turns_left > 0 && *severity == AdvisorySeverity::Concern {
            output_severity = AdvisorySeverity::Nit;
            deliver_now = true;
        } else if record.mode != Mode::Plan && !runtime.last_run_aborted(&record.id) {
            if runtime.is_running(&record.id) {
                deliver_now = true;
            } else if !started_run
                && (*severity == AdvisorySeverity::Blocker
                    || ended_with_tool_calls
                    || stopped_unexpectedly(runtime, record).await)
            {
                deliver_now = true;
                started_run = true;
            }
        }

        if deliver_now {
            if output_severity == AdvisorySeverity::Nit {
                runtime
                    .append(
                        &record.id,
                        Entry::Advisory {
                            item_id,
                            severity: output_severity,
                            note: note.clone(),
                            delivered: true,
                        },
                    )
                    .await?;
            } else {
                runtime
                    .steer(
                        &record.id,
                        Steer::Advisory {
                            item_id,
                            severity: output_severity,
                            note: note.clone(),
                        },
                    )
                    .await?;
                state.immune_turns_left = 3;
            }
        } else {
            runtime
                .append(
                    &record.id,
                    Entry::Advisory {
                        item_id,
                        severity: output_severity,
                        note: note.clone(),
                        delivered: false,
                    },
                )
                .await?;
        }
    }
    Ok(())
}

/// Whether the primary's last answer promised work it did not do, in which
/// case a concern restarts it rather than waiting as a card.
async fn stopped_unexpectedly(runtime: &AgentRuntime, record: &ConversationRecord) -> bool {
    let Ok(model) = crate::turn::conversation_model(runtime, record).await else {
        return false;
    };
    let Ok(entries) = runtime.inner.store.entries(&record.id).await else {
        return false;
    };
    let Some(blocks) = entries.iter().rev().find_map(|(_, entry)| match entry {
        Entry::Assistant { blocks, .. } => Some(blocks.clone()),
        _ => None,
    }) else {
        return false;
    };
    let output = crate::model_call::ModelOutput {
        blocks,
        usage: roost_llm::Usage::default(),
        stop: roost_llm::StopReason::EndTurn,
    };
    crate::unexpected_stop::stopped_unexpectedly(runtime, &record.id, &model.info, &output).await
}
