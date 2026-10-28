//! Agent-conversation rows in the Folders tree, including their parent/child
//! relationship. `FolderList` supplies the visible conversations and selection;
//! this module keeps fork descendants at one submenu depth.

use dioxus::prelude::*;

use super::agent_conversation_row::AgentConversationRow;
use crate::components::ui::{SidenavMenuSub, SidenavMenuSubItem};
use crate::pump::use_store;

#[component]
pub(super) fn FolderConversationRows(
    conversation_ids: Vec<String>,
    active_agent_id: Option<String>,
    nested: bool,
    search_result: bool,
    query: String,
    compact: bool,
) -> Element {
    let pump = use_store();
    let conversation_id_set: std::collections::BTreeSet<&str> =
        conversation_ids.iter().map(String::as_str).collect();
    let (roots, children_by_parent) = {
        let core = pump.core();
        let core = core.borrow();
        let conversations = &core.store().agent_chat.conversations;
        let mut roots = Vec::new();
        let mut children_by_parent =
            std::collections::BTreeMap::<String, Vec<(i64, String)>>::new();
        for id in &conversation_ids {
            let Some(conversation) = conversations.get(id.as_str()) else {
                continue;
            };
            if let Some(parent) = conversation
                .parent_id
                .as_deref()
                .filter(|parent| conversation_id_set.contains(parent))
            {
                children_by_parent
                    .entry(parent.to_owned())
                    .or_default()
                    .push((conversation.updated_ms, conversation.id.clone()));
            } else {
                roots.push((conversation.updated_ms, conversation.id.clone()));
            }
        }
        roots.sort_by_key(|(updated_ms, _)| std::cmp::Reverse(*updated_ms));
        let roots = roots.into_iter().map(|(_, id)| id).collect::<Vec<_>>();
        let children_by_parent = children_by_parent
            .into_iter()
            .map(|(parent, mut children)| {
                children.sort_by_key(|(updated_ms, _)| std::cmp::Reverse(*updated_ms));
                (
                    parent,
                    children.into_iter().map(|(_, id)| id).collect::<Vec<_>>(),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        (roots, std::rc::Rc::new(children_by_parent))
    };
    let active_agent_id = active_agent_id.map(std::rc::Rc::new);
    let branches = rsx! {
        for conversation_id in roots {
            FolderConversationBranch {
                key: "{conversation_id}",
                conversation_id,
                children_by_parent: children_by_parent.clone(),
                active_agent_id: active_agent_id.clone(),
                search_result,
                query: query.clone(),
                compact,
                nest_children: true,
            }
        }
    };
    if nested && !conversation_ids.is_empty() {
        rsx! { SidenavMenuSub { {branches} } }
    } else {
        branches
    }
}

#[component]
fn FolderConversationBranch(
    conversation_id: String,
    children_by_parent: std::rc::Rc<std::collections::BTreeMap<String, Vec<String>>>,
    active_agent_id: Option<std::rc::Rc<String>>,
    search_result: bool,
    query: String,
    compact: bool,
    nest_children: bool,
) -> Element {
    let children = children_by_parent
        .get(&conversation_id)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    rsx! {
        SidenavMenuSubItem {
            AgentConversationRow {
                conversation_id: conversation_id.clone(),
                selected: active_agent_id.as_deref().map(String::as_str)
                    == Some(conversation_id.as_str()),
                search_result,
                query: query.clone(),
            }
            if !children.is_empty() {
                if nest_children {
                    SidenavMenuSub {
                        for child_id in children {
                            FolderConversationBranch {
                                key: "{child_id}",
                                conversation_id: child_id.clone(),
                                children_by_parent: children_by_parent.clone(),
                                active_agent_id: active_agent_id.clone(),
                                search_result,
                                query: query.clone(),
                                compact,
                                nest_children: !compact,
                            }
                        }
                    }
                } else {
                    for child_id in children {
                        FolderConversationBranch {
                            key: "{child_id}",
                            conversation_id: child_id.clone(),
                            children_by_parent: children_by_parent.clone(),
                            active_agent_id: active_agent_id.clone(),
                            search_result,
                            query: query.clone(),
                            compact,
                            nest_children: false,
                        }
                    }
                }
            }
        }
    }
}
