//! The chat header traces the machine, working directory and conversation
//! lineage. It owns conversation picking, renaming and the context usage ring;
//! deletion stays behind its overflow menu. Mounted by `agent_chat::surface`.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{ConversationSummary, ModelsCatalog};

use super::toolbar_menu::{ToolbarMenu, ToolbarMenuItem, ToolbarTrigger};
use crate::components::ui::{
    Button, ButtonVariant, Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter,
    DialogHeader, DialogTitle, IconType, ListRow, StatusDot, TextField, UsageRing,
};
use crate::pump::{Pump, use_store};
use crate::router_state::use_navigate;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::ConfigureAgentChat;

/// The context window of the model this chat actually runs on, from the
/// catalog, for the usage ring's fraction. `None` when the catalog hasn't
/// loaded or the model isn't in it.
pub fn current_context_window(
    catalog: Option<&ModelsCatalog>,
    conversation: &ConversationSummary,
) -> Option<u64> {
    let current = conversation
        .effective_model
        .as_ref()
        .or(conversation.model.as_ref())?;
    catalog
        .and_then(|catalog| {
            catalog.models.iter().find(|model| {
                model.provider == current.provider && model.model_id == current.model_id
            })
        })
        .map(|model| model.context_window)
}

/// The header's usage ring: a fraction of the model's context window, when
/// it's known, and the tooltip naming the exact tokens and cost.
#[derive(Debug, Clone, PartialEq)]
pub struct HeaderUsage {
    pub fraction: Option<f64>,
    pub title: String,
}

/// `None` while nothing has been spent yet, so a fresh chat's header stays
/// bare. `context_window` is the current model's, from the catalog; without
/// it the ring still draws (an empty track), since the tooltip's numbers are
/// worth having even with no limit to fill the ring against.
pub fn header_usage(
    context_tokens: u64,
    total_tokens: u64,
    cost_usd: f64,
    context_window: Option<u64>,
) -> Option<HeaderUsage> {
    if total_tokens == 0 {
        return None;
    }
    let window = context_window.filter(|window| *window > 0);
    let fraction = window.map(|window| context_tokens as f64 / window as f64);
    let title = match window {
        Some(window) => format!(
            "{context_tokens} of {window} context tokens ({:.0}%) · {total_tokens} total this session · ${cost_usd:.4} spent",
            fraction.unwrap_or(0.0) * 100.0
        ),
        None => format!("{total_tokens} tokens this session · ${cost_usd:.4} spent"),
    };
    Some(HeaderUsage { fraction, title })
}

#[component]
pub fn AgentChatHeader(
    conversation: ConversationSummary,
    online: bool,
    usage: Option<HeaderUsage>,
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
    let lineage = conversation_lineage(&conversation, &conversations);
    let lineage_len = lineage.len();
    let rename_pump = pump.clone();
    let rename_conversation_id = conversation_id.clone();
    let navigate = use_navigate();
    rsx! {
        header { class: "flex min-w-0 flex-wrap items-center gap-2 text-sm",
            nav { class: "flex min-w-0 flex-1 flex-wrap items-center gap-x-1.5 gap-y-1", aria_label: "Conversation lineage",
                span { class: "flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground",
                    StatusDot {
                        status: if online { "ok".to_string() } else { "offline".to_string() },
                        title: Some(if online { "Machine online" } else { "Machine offline" }.to_string()),
                    }
                    span { class: "truncate", title: "{conversation.worker_label}", "{conversation.worker_label}" }
                    span { aria_hidden: "true", "›" }
                    span { class: "truncate", title: conversation.cwd.clone(), "{short_path(&conversation.cwd)}" }
                }
                for (index, ancestor) in lineage.into_iter().enumerate() {
                    span { class: "text-muted-foreground", aria_hidden: "true", "›" }
                    if index + 1 == lineage_len {
                        span { class: "min-w-0 truncate font-medium text-foreground",
                            title: super::display_conversation_title(&display_title.read()),
                            "{super::display_conversation_title(&display_title.read())}"
                        }
                    } else {
                        a {
                            class: "min-w-0 truncate text-muted-foreground hover:text-foreground",
                            href: crate::routes::agent_href(&ancestor.id),
                            onclick: move |event| { event.prevent_default(); navigate.call(crate::routes::agent_href(&ancestor.id)); },
                            title: super::display_conversation_title(&ancestor.title),
                            "{super::display_conversation_title(&ancestor.title)}"
                        }
                    }
                }
                if !online { span { class: "shrink-0 text-destructive", "Offline" } }
            }
            if let Some(usage) = usage {
                span {
                    class: "flex shrink-0 items-center gap-1.5 text-xs text-muted-foreground",
                    "data-testid": "agent-chat-usage",
                    UsageRing { fraction: usage.fraction, title: Some(usage.title.clone()) }
                    if let Some(fraction) = usage.fraction { span { "{(fraction * 100.0).round() as i64}%" } }
                }
            }
            span { class: "ml-auto flex shrink-0 items-center gap-1",
                Button {
                    variant: ButtonVariant::Ghost,
                    "aria-label": "Rename conversation",
                    title: "Rename conversation",
                    onclick: move |_| {
                        rename_title.set(display_title());
                        rename_error.set(None);
                        rename_open.set(true);
                    },
                    crate::components::ui::Icon { icon: IconType::Pencil, class: "size-4" }
                }
                Button {
                    variant: ButtonVariant::Ghost,
                    "aria-label": "Browse conversations",
                    title: "Browse conversations",
                    onclick: move |_| picker_open.set(true),
                    crate::components::ui::Icon { icon: IconType::ChevronDown, class: "size-4" }
                }
                ToolbarMenu {
                    menu_key: "actions",
                    trigger: ToolbarTrigger::Icon { icon: IconType::Ellipsis },
                    aria_label: "Conversation actions",
                    items: vec![ToolbarMenuItem {
                        danger: true,
                        ..ToolbarMenuItem::choice("delete", "Delete conversation", false)
                    }],
                    on_choose: move |_| confirm_delete.set(true),
                }
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

#[cfg(test)]
mod tests {
    use super::header_usage;

    #[test]
    fn nothing_spent_yet_leaves_the_header_bare() {
        assert_eq!(header_usage(0, 0, 0.0, Some(200_000)), None);
    }

    #[test]
    fn a_known_window_gives_the_rings_fill_from_the_latest_call_only() {
        let usage = header_usage(40_000, 140_000, 0.0123, Some(200_000)).expect("spent");
        assert!((usage.fraction.expect("known window") - 0.2).abs() < 1e-9);
        assert!(usage.title.contains("40000 of 200000"));
        assert!(usage.title.contains("20%"));
        assert!(usage.title.contains("140000 total"));
        assert!(usage.title.contains("$0.0123"));
    }

    #[test]
    fn an_unknown_window_still_reports_spend_with_no_ring_fill() {
        let usage = header_usage(40_000, 40_000, 0.5, None).expect("spent");
        assert_eq!(usage.fraction, None);
        assert!(usage.title.contains("40000 tokens"));
    }
}
