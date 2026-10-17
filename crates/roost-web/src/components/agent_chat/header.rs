//! The chat's top row: the conversation's title, where its tools run, a
//! context-usage ring, and the overflow menu that deletes it through the
//! deck's close. Model, thinking and Stop live in the composer, where the
//! next message is written. Mounted by `agent_chat::surface`.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{ConversationSummary, ModelsCatalog};

use super::toolbar_menu::{ToolbarMenu, ToolbarMenuItem, ToolbarTrigger};
use crate::components::md::{Button, ButtonVariant, Dialog, StatusDot, UsageRing};
use crate::pump::Pump;

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
    /// `None` until the transcript loads or nothing has been spent yet.
    usage: Option<HeaderUsage>,
    pump: Pump,
) -> Element {
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
            if let Some(usage) = usage {
                span { class: "agent-chat__usage", "data-testid": "agent-chat-usage",
                    UsageRing { fraction: usage.fraction, title: Some(usage.title.clone()) }
                    if let Some(fraction) = usage.fraction {
                        span { "{(fraction * 100.0).round() as i64}%" }
                    }
                }
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
