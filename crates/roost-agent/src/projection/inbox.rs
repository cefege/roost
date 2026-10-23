//! Durable inbox projection for inputs accepted before model dispatch.
//! The journal is authoritative; run intents do not mark input consumed.

use std::collections::HashMap;

use roost_protocol::wire::agent_chat::QueuedMessage;

use crate::journal::effective_entries;
use crate::records::{Entry, QueuedInputState};

pub(super) fn queued_follow_ups(entries: &[(u64, Entry)]) -> Vec<QueuedMessage> {
    let entries = effective_entries(entries);
    let mut queued = HashMap::new();
    let mut users = HashMap::new();
    let mut withdrawn = std::collections::HashSet::new();
    let mut consumed_through = 0;
    let mut run_active = false;
    for (sequence, entry) in &entries {
        match entry {
            Entry::RunIntent { active, .. } => run_active = *active,
            Entry::QueuedInput {
                submission_id,
                text,
                state,
            } => {
                queued.insert(submission_id.clone(), (*sequence, text.clone(), *state));
                withdrawn.remove(submission_id);
            }
            Entry::QueuedInputWithdrawn { submission_id } => {
                withdrawn.insert(submission_id.clone());
            }
            Entry::User {
                text,
                item_id: Some(id),
                ..
            } => {
                users.insert(id.clone(), (*sequence, text.clone()));
            }
            Entry::InputConsumed { through_seq } => {
                consumed_through = consumed_through.max(*through_seq);
            }
            Entry::Assistant {
                metadata: Some(metadata),
                ..
            } => {
                consumed_through = consumed_through.max(metadata.input_through_seq);
            }
            _ => {}
        }
    }
    let mut messages = Vec::new();
    for (id, (sequence, text, state)) in queued {
        if withdrawn.contains(&id) {
            continue;
        }
        match state {
            QueuedInputState::Pending => messages.push((sequence, id, text, "follow_up")),
            QueuedInputState::Activated => {
                if let Some((user_sequence, user_text)) = users.get(&id)
                    && *user_sequence > consumed_through
                    && run_active
                {
                    messages.push((*user_sequence, id, user_text.clone(), "follow_up"));
                }
            }
        }
    }
    for (id, (sequence, text)) in users {
        if run_active
            && sequence > consumed_through
            && !withdrawn.contains(&id)
            && !messages.iter().any(|(_, queued_id, _, _)| queued_id == &id)
        {
            messages.push((sequence, id, text, ""));
        }
    }
    messages.sort_by_key(|(sequence, _, _, _)| *sequence);
    messages
        .into_iter()
        .map(|(_, id, text, mode)| QueuedMessage {
            id,
            text,
            mode: mode.into(),
        })
        .collect()
}
