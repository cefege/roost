//! Conversation lineage lookup and compact path presentation.
//! `agent_chat::header` uses these helpers for breadcrumbs and the picker.
//! Values remain protocol summaries; this module does not own navigation.

use roost_protocol::wire::agent_chat::ConversationSummary;

/// The last two path components identify a folder while the full path stays in the tooltip.
pub(super) fn short_path(path: &str) -> String {
    let parts: Vec<&str> = path
        .split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .collect();
    match parts.as_slice() {
        [] => path.to_string(),
        [only] => (*only).to_string(),
        [.., parent, leaf] => format!("{parent}/{leaf}"),
    }
}

pub(super) fn conversation_lineage(
    conversation: &ConversationSummary,
    conversations: &[ConversationSummary],
) -> Vec<ConversationSummary> {
    let mut lineage = vec![conversation.clone()];
    let mut seen = std::collections::HashSet::from([conversation.id.clone()]);
    let mut parent_id = conversation.parent_id.clone();
    while let Some(id) = parent_id {
        if !seen.insert(id.clone()) {
            break;
        }
        let Some(parent) = conversations.iter().find(|candidate| candidate.id == id) else {
            break;
        };
        parent_id = parent.parent_id.clone();
        lineage.push(parent.clone());
    }
    lineage.reverse();
    lineage
}
