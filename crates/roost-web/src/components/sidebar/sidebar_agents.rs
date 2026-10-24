//! The Agents panel: machine and workspace navigation for native conversations,
//! their lineage, and every open terminal. Terminals stay in a collapsed
//! secondary section; conversation and terminal rows link to their own routes.

use dioxus::prelude::*;
use libero::components::{Box, Button, Flex, List, Pictogram, Text, Title};
use libero::sx::sx;
use roost_client_core::store::navigation::query::normalize_navigation_search_query;
use roost_client_core::store::selectors::live_session_ids_for_folder;
use roost_client_core::store::sidebar::documents::store_navigation_documents;
use roost_client_core::store::sidebar::folder_groups::build_folder_groups;

use super::agent_conversation_row::AgentConversationRow;
use super::session_row::SessionRow;
use crate::components::notifications::notify_target::{ring_attribute, use_notify_target};
use crate::platform::BrowserWorkerPaths;
use crate::pump::use_store;
use crate::router_state::use_location;

/// The panel.
#[component]
pub fn SidebarAgents(query: String) -> Element {
    use roost_protocol::wire::agent_chat::ConversationSummary;
    use std::collections::{BTreeMap, BTreeSet};

    let pump = use_store();
    let path = use_location();
    let notify_target = use_notify_target();
    let mut terminal_folders = use_signal(BTreeSet::<String>::new);
    let mut collapsed_machines = use_signal(BTreeSet::<String>::new);
    let mut collapsed_folders = use_signal(BTreeSet::<String>::new);
    let empty_sx = use_hook(|| {
        sx().align_items("center")
            .text_align("center")
            .padding("lg")
    });
    let toggle_sx = use_hook(|| sx().width("100%").justify_content("flex-start").gap("sm"));
    let list_sx = use_hook(|| sx().padding_inline("sm"));
    let panel_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .flex("1")
            .min_height("0")
            .overflow_y("auto")
            .padding_block("xs")
    });
    let group_sx = use_hook(|| sx().display("flex").flex_direction("column").gap("xs"));
    let folder_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .gap("xs")
            .padding_left("sm")
    });
    let session_sx = use_hook(|| sx().border_radius("md"));
    let (groups, conversations, visible_ids, active_agent_id) = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        let now_ms = i64::try_from(core.clock().now_ms()).unwrap_or(i64::MAX);
        let documents = store_navigation_documents(store, &BrowserWorkerPaths, now_ms);
        let mut groups = build_folder_groups(store, &BrowserWorkerPaths, now_ms);
        for group in &mut groups {
            group.session_ids = live_session_ids_for_folder(store, &BrowserWorkerPaths, &group.key);
        }
        let conversations: Vec<ConversationSummary> =
            store.agent_chat.conversations.values().cloned().collect();
        let by_id: BTreeMap<&str, &ConversationSummary> = conversations
            .iter()
            .map(|conversation| (conversation.id.as_str(), conversation))
            .collect();
        let needle = normalize_navigation_search_query(&query);
        let mut visible_ids = BTreeSet::new();
        let mut matched_ids = BTreeSet::new();
        for group in &groups {
            if !needle.is_empty()
                && normalize_navigation_search_query(&format!(
                    "{} {} {}",
                    group.name, group.server, group.spawn_cwd
                ))
                .contains(needle.as_str())
            {
                matched_ids.extend(group.agent_conversation_ids.iter().cloned());
            }
        }
        for conversation in &conversations {
            let matches = needle.is_empty()
                || matched_ids.contains(&conversation.id)
                || [
                    conversation.title.as_str(),
                    conversation.worker_label.as_str(),
                    conversation.cwd.as_str(),
                    conversation
                        .model
                        .as_ref()
                        .map_or("", |model| model.model_id.as_str()),
                ]
                .iter()
                .any(|value| normalize_navigation_search_query(value).contains(needle.as_str()));
            if matches {
                matched_ids.insert(conversation.id.clone());
                let mut ancestor = Some(conversation.id.as_str());
                let mut seen = BTreeSet::new();
                while let Some(id) = ancestor {
                    if !seen.insert(id) {
                        break;
                    }
                    visible_ids.insert(id.to_owned());
                    ancestor = by_id.get(id).and_then(|row| row.parent_id.as_deref());
                }
            }
        }
        // Only a matching parent includes its children; ancestors kept for a
        // child match must not pull unrelated siblings into search results.
        let mut expanded = true;
        while expanded {
            expanded = false;
            for conversation in &conversations {
                if conversation
                    .parent_id
                    .as_ref()
                    .is_some_and(|parent| matched_ids.contains(parent))
                    && matched_ids.insert(conversation.id.clone())
                {
                    visible_ids.insert(conversation.id.clone());
                    expanded = true;
                }
            }
        }
        let document_by_session: BTreeMap<&str, &str> = documents
            .iter()
            .map(|document| {
                (
                    document.session_id.as_str(),
                    document.display_title.as_str(),
                )
            })
            .collect();
        for group in &mut groups {
            group.session_ids.retain(|session_id| {
                needle.is_empty()
                    || document_by_session
                        .get(session_id.as_str())
                        .is_some_and(|title| {
                            normalize_navigation_search_query(title).contains(needle.as_str())
                        })
            });
            group
                .agent_conversation_ids
                .retain(|id| visible_ids.contains(id));
        }
        groups.retain(|group| {
            needle.is_empty()
                || normalize_navigation_search_query(&format!(
                    "{} {} {}",
                    group.name, group.server, group.spawn_cwd
                ))
                .contains(needle.as_str())
                || !group.agent_conversation_ids.is_empty()
                || !group.session_ids.is_empty()
        });
        let active_agent = match crate::routes::Route::parse(&path.read()) {
            crate::routes::Route::Agent { conversation_id } => Some(conversation_id),
            _ => None,
        };
        (groups, conversations, visible_ids, active_agent)
    };
    let conversations = std::rc::Rc::new(conversations);
    let visible_ids = std::rc::Rc::new(visible_ids);
    let filtered = !normalize_navigation_search_query(&query).is_empty();
    let conversations_by_parent = |parent_id: Option<&str>| {
        let mut children: Vec<_> = conversations
            .iter()
            .filter(|conversation| conversation.parent_id.as_deref() == parent_id)
            .filter(|conversation| visible_ids.contains(&conversation.id))
            .cloned()
            .collect();
        children.sort_by_key(|conversation| std::cmp::Reverse(conversation.updated_ms));
        children
    };
    let mut machine_groups: Vec<(
        String,
        String,
        Vec<roost_client_core::store::sidebar::folder_groups::FolderGroup>,
    )> = Vec::new();
    for group in groups {
        if let Some((_, _, folders)) = machine_groups
            .iter_mut()
            .find(|(worker_fp, _, _)| worker_fp == &group.spawn_fp)
        {
            folders.push(group);
        } else {
            machine_groups.push((group.spawn_fp.clone(), group.server.clone(), vec![group]));
        }
    }
    rsx! {
        Box {
            component: "section",
            sx: panel_sx,
            "data-testid": "sidebar-agents",
            "aria-label": "Agents",
            if machine_groups.is_empty() {
                Flex { sx: empty_sx, direction: "column", gap: "md",
                    Pictogram {
                        icon: if filtered { pictogram_icons_lucide::search_x::outlined } else { pictogram_icons_lucide::bot::outlined },
                        width: "1.5rem", height: "1.5rem",
                    }
                    Title { size: "md", if filtered { "No matching agents" } else { "No agents or terminals" } }
                    Text { size: "sm", color: "muted", if filtered { "Try a different filter." } else { "Start an agent in a workspace or expand Terminals." } }
                }
            }
            for (worker_fp, machine_label, folders) in machine_groups {
                Box {
                    component: "section",
                    sx: group_sx.clone(),
                    "data-testid": "sidebar-agent-machine-{worker_fp}",
                    "aria-label": "{machine_label}",
                    Button {
                        variant: "standard", size: "sm", sx: toggle_sx.clone(),
                        "aria-expanded": !collapsed_machines.read().contains(&worker_fp),
                        onclick: {
                            let key = worker_fp.clone();
                            move |_| {
                                let mut collapsed = collapsed_machines.write();
                                if !collapsed.insert(key.clone()) { collapsed.remove(&key); }
                            }
                        },
                        Pictogram {
                            icon: if collapsed_machines.read().contains(&worker_fp) { pictogram_icons_lucide::chevron_right::outlined } else { pictogram_icons_lucide::chevron_down::outlined },
                            width: "1rem", height: "1rem",
                        }
                        Text { size: "sm", "{machine_label}" }
                    }
                    if filtered || !collapsed_machines.read().contains(&worker_fp) {
                    for group in folders {
                        Flex { direction: "column", sx: folder_sx.clone(),
                            "data-testid": "sidebar-agent-group-{group.key}",
                            "data-folder-key": group.key.clone(),
                            "aria-label": "{group.name} on {group.server}",
                            Button {
                                variant: "standard", size: "sm", sx: toggle_sx.clone(),
                                title: group.spawn_cwd.clone(),
                                "aria-expanded": !collapsed_folders.read().contains(&group.key),
                                onclick: {
                                    let key = group.key.clone();
                                    move |_| {
                                        let mut collapsed = collapsed_folders.write();
                                        if !collapsed.insert(key.clone()) { collapsed.remove(&key); }
                                    }
                                },
                                Pictogram {
                                    icon: if collapsed_folders.read().contains(&group.key) { pictogram_icons_lucide::chevron_right::outlined } else { pictogram_icons_lucide::chevron_down::outlined },
                                    width: "1rem", height: "1rem",
                                }
                                Text { size: "sm", "{group.name.clone()}" }
                                if !group.online { Text { size: "xs", color: "muted", "Offline" } }
                            }
                            if filtered || !collapsed_folders.read().contains(&group.key) {
                            List { sx: list_sx.clone(),
                                for conversation in conversations_by_parent(None).into_iter().filter(|conversation| {
                                    group.agent_conversation_ids.contains(&conversation.id)
                                }) {
                                    ConversationBranch {
                                        key: "{conversation.id}",
                                        conversation,
                                        all_conversations: conversations.clone(),
                                        visible_ids: visible_ids.clone(),
                                        active_id: active_agent_id.clone(),
                                        depth: 0,
                                    }
                                }
                                {
                                    let expanded = terminal_folders.read().contains(&group.key);
                                    let folder_key = group.key.clone();
                                    rsx! {
                                        Button {
                                            variant: "standard",
                                            size: "sm",
                                            sx: toggle_sx.clone(),
                                            "data-testid": "sidebar-terminals-toggle-{folder_key}",
                                            "aria-expanded": "{expanded}",
                                            onclick: move |_| {
                                                let mut folders = terminal_folders.write();
                                                if !folders.insert(folder_key.clone()) {
                                                    folders.remove(&folder_key);
                                                }
                                            },
                                            Pictogram { icon: pictogram_icons_lucide::terminal::outlined, width: "1rem", height: "1rem" }
                                            "Terminals"
                                            Text { size: "xs", "{group.session_ids.len()}" }
                                        }
                                    }
                                }
                                if filtered || terminal_folders.read().contains(&group.key) {
                                    for session_id in group.session_ids.iter().cloned() {
                                        Box {
                                            component: "div",
                                            sx: session_sx.clone(),
                                            "data-notify-target": notify_target.as_ref().and_then(|target| ring_attribute(target, &session_id)),
                                            "data-notify-ring": "child",
                                            SessionRow { session_id: session_id.clone(), cursor: false }
                                        }
                                    }
                                }
                            }
                            }
                        }
                    }
                    }
                }
            }
        }
    }
}

#[component]
fn ConversationBranch(
    conversation: roost_protocol::wire::agent_chat::ConversationSummary,
    all_conversations: std::rc::Rc<Vec<roost_protocol::wire::agent_chat::ConversationSummary>>,
    visible_ids: std::rc::Rc<std::collections::BTreeSet<String>>,
    active_id: Option<String>,
    depth: usize,
) -> Element {
    let child_rows: Vec<_> = all_conversations
        .iter()
        .filter(|child| child.parent_id.as_deref() == Some(conversation.id.as_str()))
        .filter(|child| visible_ids.contains(&child.id))
        .cloned()
        .collect();
    let selected = active_id.as_deref() == Some(conversation.id.as_str());
    let root_branch_sx = use_hook(|| sx().min_width("0").gap("0"));
    let nested_branch_sx = use_hook(|| sx().min_width("0").gap("0").padding_left("sm"));
    rsx! {
        Flex { direction: "column", sx: if depth == 0 { root_branch_sx } else { nested_branch_sx },
            AgentConversationRow {
                conversation_id: conversation.id.clone(),
                selected,
                compact: true,
            }
            for child in child_rows {
                ConversationBranch {
                    key: "{child.id}",
                    conversation: child,
                    all_conversations: all_conversations.clone(),
                    visible_ids: visible_ids.clone(),
                    active_id: active_id.clone(),
                    depth: depth + 1,
                }
            }
        }
    }
}
