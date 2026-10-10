//! The chat's top row: the conversation's title, where its tools run, and the
//! overflow menu that deletes it through the deck's close. Model, thinking and
//! Stop live in the composer, where the next message is written.
//! Mounted by `agent_chat::surface`.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::ConversationSummary;

use super::toolbar_menu::{ToolbarMenu, ToolbarMenuItem, ToolbarTrigger};
use crate::components::md::{Button, ButtonVariant, Dialog, StatusDot};
use crate::pump::Pump;

#[component]
pub fn AgentChatHeader(conversation: ConversationSummary, online: bool, pump: Pump) -> Element {
    let mut confirm_delete = use_signal(|| false);
    let conversation_id = conversation.id.clone();
    let place = format!(
        "{} · {}",
        conversation.worker_label,
        short_path(&conversation.cwd)
    );
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (&pump, &conversation_id);
    rsx! {
        header { class: "agent-chat__header",
            span { class: "agent-chat__title", title: conversation.title.clone(), "{conversation.title}" }
            span { class: "agent-chat__place", title: "{conversation.worker_label} · {conversation.cwd}",
                StatusDot {
                    status: if online { "ok".to_string() } else { "offline".to_string() },
                    title: Some(if online { "Machine online" } else { "Machine offline" }.to_string()),
                }
                span { class: "agent-chat__place-text", "{place}" }
                if !online { span { class: "agent-chat__offline", "Offline" } }
            }
            span { class: "agent-chat__header-actions",
                ToolbarMenu {
                    menu_key: "actions",
                    trigger: ToolbarTrigger::Icon { icon: "more_horiz" },
                    aria_label: "Conversation actions",
                    items: vec![ToolbarMenuItem {
                        danger: true,
                        ..ToolbarMenuItem::choice("delete", "Delete conversation", false)
                    }],
                    on_choose: move |_| confirm_delete.set(true),
                }
            }
            Dialog {
                open: confirm_delete(),
                on_close: move |_| confirm_delete.set(false),
                headline: Some("Delete conversation?".to_string()),
                actions: Some(rsx! {
                    Button { variant: ButtonVariant::Outline, onclick: move |_| confirm_delete.set(false), "Cancel" }
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
                }),
                "The transcript is removed from Roost. You can undo for a few seconds after it closes."
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
