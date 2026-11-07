//! Conversation actions rendered beside the selected agent tab's title.
//! Rename, browsing and deletion live behind its overflow menu.
//! Mounted by desktop and compact deck chrome.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::ConversationSummary;

use super::toolbar_menu::{ToolbarMenu, ToolbarMenuItem, ToolbarTrigger};
use crate::components::ui::{
    Button, ButtonVariant, Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter,
    DialogHeader, DialogTitle, IconType, ListRow, StatusDot, TextField,
};
use crate::pump::{Pump, use_store};
use crate::router_state::use_navigate;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::ConfigureAgentChat;

#[component]
pub fn AgentChatHeader(
    conversation: ConversationSummary,
    online: Option<bool>,
    pump: Pump,
) -> Element {
    let store = use_store();
    let _revision = store.revision()();
    let mut conversations = {
        let core = store.core();
        let core = core.borrow();
        core.store()
            .agent_chat
            .conversations
            .values()
            .filter(|conversation| !conversation.is_native())
            .cloned()
            .collect::<Vec<_>>()
    };
    conversations.sort_by(|left, right| {
        (&left.worker_label, &left.cwd, &left.title).cmp(&(
            &right.worker_label,
            &right.cwd,
            &right.title,
        ))
    });
    let mut confirm_delete = use_signal(|| false);
    let mut picker_open = use_signal(|| false);
    let mut rename_open = use_signal(|| false);
    let mut rename_title = use_signal(|| conversation.title.clone());
    let mut display_title = use_signal(|| conversation.title.clone());
    let mut rename_pending = use_signal(|| false);
    let mut rename_error = use_signal(|| None::<String>);
    use_effect(use_reactive((&conversation.title,), move |(title,)| {
        display_title.set(title.clone())
    }));
    let conversation_id = conversation.id.clone();
    let rename_pump = pump.clone();
    let rename_conversation_id = conversation_id.clone();
    let navigate = use_navigate();
    rsx! {
        header { class: "flex shrink-0 min-w-0 items-center gap-1 text-sm",
            if let Some(online) = online {
                StatusDot {
                    status: if online { "ok".to_string() } else { "offline".to_string() },
                    title: Some(if online { "Machine online" } else { "Machine offline" }.to_string()),
                }
                if !online {
                    span { class: "shrink-0 text-xs text-destructive", "Offline" }
                }
            }
            ToolbarMenu {
                menu_key: "actions",
                trigger: ToolbarTrigger::Icon { icon: IconType::Ellipsis },
                aria_label: "Conversation actions",
                items: vec![
                    ToolbarMenuItem::choice("rename", "Rename conversation", false),
                    ToolbarMenuItem::choice("browse", "Browse conversations", false),
                    ToolbarMenuItem {
                        danger: true,
                        ..ToolbarMenuItem::choice("delete", "Delete conversation", false)
                    },
                ],
                on_choose: move |action: String| match action.as_str() {
                    "rename" => {
                        rename_title.set(display_title());
                        rename_error.set(None);
                        rename_open.set(true);
                    }
                    "browse" => picker_open.set(true),
                    "delete" => confirm_delete.set(true),
                    _ => {}
                },
            }
            Dialog {
                open: picker_open(),
                on_open_change: move |open: bool| picker_open.set(open),
                DialogContent {
                    class: "overflow-y-auto",
                    DialogHeader {
                        DialogTitle { "Conversations" }
                        DialogDescription { "Choose an agent conversation." }
                    }
                    for choice in conversations.iter() {
                        {
                            let headline = if choice.id == conversation_id {
                                super::display_conversation_title(&display_title.read()).to_owned()
                            } else {
                                super::display_conversation_title(&choice.title).to_owned()
                            };
                            let choice_id = choice.id.clone();
                            let choice_href = crate::routes::agent_href(&choice_id);
                            let choice_lineage = conversation_lineage(choice, &conversations)
                                .iter()
                                .map(|entry| {
                                    if entry.id == conversation_id {
                                        super::display_conversation_title(&display_title.read()).to_owned()
                                    } else {
                                        super::display_conversation_title(&entry.title).to_owned()
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join(" / ");
                            let choice_support = format!(
                                "{} · {} · {}",
                                choice.worker_label,
                                short_path(&choice.cwd),
                                choice_lineage,
                            );
                            rsx! {
                                ListRow {
                                    key: "{choice_id}",
                                    href: choice_href,
                                    on_navigate: move |href| {
                                        picker_open.set(false);
                                        navigate.call(href);
                                    },
                                    headline: rsx! { "{headline}" },
                                    support: rsx! { "{choice_support}" },
                                }
                            }
                        }
                    }
                }
            }
            Dialog {
                open: rename_open(),
                on_open_change: move |open: bool| rename_open.set(open),
                DialogContent {
                    DialogHeader {
                        DialogTitle { "Rename conversation" }
                        DialogDescription { "Choose a name shown in the conversation picker." }
                    }
                    TextField {
                        value: rename_title(),
                        on_input: move |value: String| rename_title.set(value),
                        label: "Conversation name",
                        disabled: rename_pending(),
                        test_id: "agent-chat-rename-title",
                    }
                    if let Some(error) = rename_error() {
                        p { class: "text-sm text-destructive", role: "alert", "{error}" }
                    }
                    DialogFooter {
                        DialogClose { "Cancel" }
                        Button {
                            variant: ButtonVariant::Default,
                            disabled: rename_pending() || rename_title().trim().is_empty(),
                            "data-testid": "agent-chat-rename-save",
                            onclick: move |_| {
                                let title = rename_title.peek().trim().to_owned();
                                if title.is_empty() || rename_pending() { return; }
                                rename_pending.set(true);
                                let request_pump = rename_pump.clone();
                                let request_id = rename_conversation_id.clone();
                                let saved_title = title.clone();
                                #[cfg(target_arch = "wasm32")]
                                spawn(async move {
                                    let result = request_pump.rpc().call(&ConfigureAgentChat {
                                        conversation_id: request_id,
                                        title: Some(title),
                                        ..Default::default()
                                    }).await.map(|_| ());
                                    rename_pending.set(false);
                                    match result {
                                        Ok(()) => {
                                            display_title.set(saved_title);
                                            rename_open.set(false);
                                        }
                                        Err(error) => rename_error.set(Some(error.to_string())),
                                    }
                                });
                                #[cfg(not(target_arch = "wasm32"))]
                                let _ = (request_pump, request_id, saved_title);
                            },
                            if rename_pending() { "Saving…" } else { "Save name" }
                        }
                    }
                }
            }
            Dialog {
                open: confirm_delete(),
                on_open_change: move |open: bool| confirm_delete.set(open),
                DialogContent {
                    DialogHeader {
                        DialogTitle { "Delete conversation?" }
                        DialogDescription {
                            "The transcript is removed from Roost. You can undo for a few seconds after it closes."
                        }
                    }
                    DialogFooter {
                        DialogClose { "Cancel" }
                        Button {
                            variant: ButtonVariant::Destructive,
                            "data-testid": "agent-chat-delete-confirm",
                            onclick: move |_| {
                                confirm_delete.set(false);
                                #[cfg(target_arch = "wasm32")]
                                crate::components::deck::close_agent_tab::close_agent_tab(pump.clone(), conversation_id.clone());
                            },
                            "Delete"
                        }
                    }
                }
            }
        }
    }
}

/// The last two components of a path: enough to recognise the folder while
/// the full path stays in the tooltip.
pub fn short_path(path: &str) -> String {
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

fn conversation_lineage(
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
