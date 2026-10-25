//! Store-aware folder filtering for the sidebar's agent conversation rows.
//! Folder matches reveal the whole folder; conversation matches preserve their
//! ancestry and matching descendants without pulling in unrelated siblings.

use std::collections::BTreeSet;

use crate::store::Store;
use crate::store::navigation::query::normalize_navigation_search_query;

use super::folder_groups::FolderGroup;

/// Filter folders by folder metadata and their native agent conversation data.
pub fn filter_folder_groups_for_store(
    store: &Store,
    groups: &[FolderGroup],
    query: &str,
) -> Vec<FolderGroup> {
    let needle = normalize_navigation_search_query(query);
    if needle.is_empty() {
        return groups.to_vec();
    }
    let terms: Vec<&str> = needle.split(' ').collect();
    let mut filtered = Vec::new();
    for group in groups {
        let folder_text = normalize_navigation_search_query(
            &[group.name.as_str(), &group.server, &group.spawn_cwd].join("\n"),
        );
        let folder_matches = terms.iter().all(|term| folder_text.contains(term));
        let folder_ids: BTreeSet<&str> = group
            .agent_conversation_ids
            .iter()
            .map(String::as_str)
            .collect();
        let mut visible_ids = BTreeSet::new();
        if folder_matches {
            visible_ids.extend(folder_ids.iter().copied());
        } else {
            let matching_ids: Vec<&str> = group
                .agent_conversation_ids
                .iter()
                .filter_map(|id| {
                    let conversation = store.agent_chat.conversations.get(id.as_str())?;
                    let model_id = conversation
                        .model
                        .as_ref()
                        .map_or("", |model| model.model_id.as_str());
                    [
                        conversation.title.as_str(),
                        conversation.worker_label.as_str(),
                        conversation.cwd.as_str(),
                        model_id,
                    ]
                    .iter()
                    .any(|field| normalize_navigation_search_query(field).contains(needle.as_str()))
                    .then_some(id.as_str())
                })
                .collect();
            visible_ids.extend(matching_ids.iter().copied());
            loop {
                let mut added_descendant = false;
                for id in group.agent_conversation_ids.iter().map(String::as_str) {
                    let Some(conversation) = store.agent_chat.conversations.get(id) else {
                        continue;
                    };
                    if conversation
                        .parent_id
                        .as_deref()
                        .is_some_and(|parent| visible_ids.contains(parent))
                        && visible_ids.insert(id)
                    {
                        added_descendant = true;
                    }
                }
                if !added_descendant {
                    break;
                }
            }
            let mut ancestor_ids: Vec<&str> = visible_ids.iter().copied().collect();
            while let Some(id) = ancestor_ids.pop() {
                let Some(parent) = store
                    .agent_chat
                    .conversations
                    .get(id)
                    .and_then(|conversation| conversation.parent_id.as_deref())
                else {
                    continue;
                };
                if folder_ids.contains(parent) && visible_ids.insert(parent) {
                    ancestor_ids.push(parent);
                }
            }
        }
        let mut matched = group.clone();
        matched
            .agent_conversation_ids
            .retain(|id| visible_ids.contains(id.as_str()));
        if folder_matches || !matched.agent_conversation_ids.is_empty() {
            filtered.push(matched);
        }
    }
    filtered
}
