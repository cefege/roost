//! Rebuilds the loop position an interrupted run had reached from its last
//! assistant entry's metadata, so recovery resumes the turn rather than
//! starting a new one. `recovery` calls this after claiming the run lease.

use roost_llm::AssistantBlock;

use super::AgentRuntime;
use super::recovery::latest_submission_seq;
use crate::model_call::ModelOutput;
use crate::records::Entry;
use crate::roles::ResolvedModel;

pub(super) fn recovered_run(
    runtime: &AgentRuntime,
    id: &str,
    entries: &[(u64, Entry)],
) -> Option<crate::run_loop::RecoveredRun> {
    let effective = crate::journal::effective_entries(entries);
    let (assistant_seq, entry) = effective.iter().rev().find_map(|(sequence, entry)| {
        matches!(entry, Entry::Assistant { .. }).then_some((*sequence, entry))
    })?;
    let active_before = entries.iter().rev().find_map(|(sequence, entry)| {
        if *sequence >= assistant_seq {
            return None;
        }
        match entry {
            Entry::RunIntent { active, .. } => Some(*active),
            _ => None,
        }
    })?;
    if !active_before
        || entries.iter().any(|(sequence, entry)| {
            *sequence > assistant_seq && matches!(entry, Entry::RunIntent { active: false, .. })
        })
    {
        return None;
    }
    let Entry::Assistant {
        blocks,
        usage,
        model,
        metadata: Some(metadata),
        ..
    } = entry
    else {
        return None;
    };
    let has_late_submission = latest_submission_seq(entries)
        .is_some_and(|sequence| sequence > metadata.input_through_seq);
    let has_late_user = effective.iter().any(|(sequence, entry)| {
        *sequence > metadata.input_through_seq && matches!(entry, Entry::User { .. })
    });
    let loop_body = runtime
        .inner
        .loops
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(id)
        .and_then(|state| state.body.clone());
    let mut continued = metadata.continued && !has_late_user;
    let mut yield_reminders = metadata.yield_reminders;
    for (sequence, entry) in effective
        .iter()
        .filter(|(sequence, _)| *sequence > assistant_seq)
    {
        let Entry::User { text, .. } = entry else {
            continue;
        };
        let steered_user = entries.iter().any(|(intent_seq, entry)| {
            matches!(
                entry,
                Entry::RunIntent {
                    active: true,
                    through_seq,
                    ..
                } if through_seq == sequence && intent_seq > sequence
            )
        });
        if steered_user {
            continued = false;
        } else if text == crate::prompts::UNEXPECTED_STOP {
            continued = true;
        } else if let Some(attempt) = crate::run_loop::yield_reminder_attempt(text) {
            yield_reminders = yield_reminders.max(attempt);
        } else if loop_body.as_deref() != Some(text.as_str()) {
            continued = false;
        }
    }
    let pending_text = if metadata.completed
        && !has_late_submission
        && metadata.stop_reason.is_some()
        && !blocks
            .iter()
            .any(|block| matches!(block, AssistantBlock::ToolCall { .. }))
    {
        let model = model.as_ref().and_then(|reference| {
            runtime
                .catalog()
                .get(&reference.provider, &reference.model_id)
                .cloned()
        });
        Some(crate::run_loop::RecoveredTextTurn {
            output: ModelOutput {
                blocks: blocks.clone(),
                usage: *usage,
                stop: metadata.stop_reason.clone()?,
            },
            model: model.map(|info| ResolvedModel {
                info,
                thinking: None,
            }),
        })
    } else {
        None
    };
    Some(crate::run_loop::RecoveredRun {
        continued,
        yield_reminders,
        pending_text,
    })
}
