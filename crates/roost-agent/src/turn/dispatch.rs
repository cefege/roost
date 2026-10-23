//! A model dispatch's durable accepted-input boundary and journal snapshot.
//! Consumption is appended before provider work in the store's withdrawal order.
//! A continuation whose only new input was withdrawn must not reach the provider.

use roost_llm::Message;
use tokio_util::sync::CancellationToken;

use crate::error::AgentError;
use crate::history;
use crate::records::Entry;
use crate::runtime::{AgentRuntime, latest_submission_seq};

pub(super) struct ClaimedInputs {
    pub entries: Vec<(u64, Entry)>,
    pub through_seq: u64,
    pub new_submission: bool,
}

pub(super) async fn claim_dispatch_inputs(
    runtime: &AgentRuntime,
    id: &str,
    cancel: &CancellationToken,
    provider: &str,
) -> Result<Option<ClaimedInputs>, AgentError> {
    let initial_entries = runtime.inner.store.entries(id).await?;
    let previously_consumed = initial_entries
        .iter()
        .filter_map(|(_, entry)| match entry {
            Entry::InputConsumed { through_seq } => Some(*through_seq),
            Entry::Assistant {
                metadata: Some(metadata),
                ..
            } => Some(metadata.input_through_seq),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    let had_unconsumed_user = initial_entries.iter().any(|(sequence, entry)| {
        *sequence > previously_consumed && matches!(entry, Entry::User { .. })
    });
    let through_seq = latest_submission_seq(&initial_entries).unwrap_or(0);
    if through_seq > 0 {
        runtime
            .append_fenced(id, &Entry::InputConsumed { through_seq }, &[cancel])
            .await?;
    }
    let mut entries = runtime.inner.store.entries(id).await?;
    entries.retain(|(sequence, entry)| {
        *sequence <= through_seq || !matches!(entry, Entry::User { .. })
    });
    let effective = crate::journal::effective_entries(&entries);
    let has_unconsumed_user = effective.iter().any(|(sequence, entry)| {
        *sequence > previously_consumed
            && *sequence <= through_seq
            && matches!(entry, Entry::User { .. })
    });
    let messages = history::build_messages(&entries, provider);
    if had_unconsumed_user
        && !has_unconsumed_user
        && (messages.is_empty() || matches!(messages.last(), Some(Message::Assistant { .. })))
    {
        return Ok(None);
    }
    let submission_ids: Vec<_> = effective
        .iter()
        .filter_map(|(sequence, entry)| match entry {
            Entry::User {
                item_id: Some(item_id),
                ..
            } if *sequence > previously_consumed && *sequence <= through_seq => {
                Some(item_id.clone())
            }
            _ => None,
        })
        .collect();
    let new_submission = !submission_ids.is_empty();
    if new_submission {
        runtime
            .emit(
                id,
                vec![
                    roost_protocol::wire::agent_chat::ChatEvent::QueueDelivered { submission_ids },
                ],
            )
            .await;
    }
    Ok(Some(ClaimedInputs {
        entries,
        through_seq,
        new_submission,
    }))
}
