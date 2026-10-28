//! Durable queued submissions beside the composer, with explicit delivery mode.
//! The transcript owns membership; conversation-level actions outlive individual
//! rows so a consumed-input refusal remains visible after Sync removes its row.

use dioxus::prelude::*;
use roost_client_core::client::rpc::calls::agent_chat::WithdrawAgentChat;
use roost_protocol::wire::agent_chat::QueuedMessage;

use crate::components::ui::{
    Badge, BadgeSize, BadgeVariant, ButtonVariant, FieldError, IconButton, IconType,
};
use crate::pump::Pump;

#[component]
pub(super) fn QueuedBand(
    messages: Vec<QueuedMessage>,
    conversation_id: String,
    pump: Pump,
) -> Element {
    let mut pending = use_signal(|| None::<String>);
    let mut error = use_signal(|| None::<String>);
    let withdraw = EventHandler::new(move |(submission_id, request_id): (String, String)| {
        if pending.peek().is_some() {
            return;
        }
        pending.set(Some(submission_id.clone()));
        error.set(None);
        let pump = pump.clone();
        let conversation_id = conversation_id.clone();
        spawn(async move {
            let result = pump
                .rpc()
                .call(&WithdrawAgentChat {
                    conversation_id,
                    submission_id,
                    request_id,
                })
                .await;
            pending.set(None);
            error.set(match result {
                Ok(true) => None,
                Ok(false) => Some("This input was already consumed or withdrawn; it was not cancelled by this action.".to_owned()),
                Err(problem) => Some(format!("Could not withdraw queued input: {problem}")),
            });
        });
    });
    let count = messages.len();
    if count == 0 && error().is_none() {
        return rsx! {};
    }
    rsx! {
        section { class: "flex max-h-40 shrink-0 flex-col gap-1 overflow-y-auto overscroll-y-contain text-sm",
            "aria-label": "Queued messages", "data-testid": "agent-chat-queued",
            if count > 0 {
                p { class: "text-xs text-muted-foreground", "Queued · {count}" }
                ol { class: "m-0 flex list-none flex-col gap-1 p-0",
                    for message in messages {
                        QueuedMessageRow { key: "{message.id}", message, disabled: pending().is_some(), on_withdraw: withdraw }
                    }
                }
            }
            if let Some(message) = error() {
                div { class: "flex items-start gap-2",
                    FieldError { "{message}" }
                    IconButton { icon: IconType::X, label: "Dismiss queue error", variant: ButtonVariant::Ghost,
                        onclick: move |_| error.set(None) }
                }
            }
        }
    }
}

#[component]
fn QueuedMessageRow(
    message: QueuedMessage,
    disabled: bool,
    on_withdraw: EventHandler<(String, String)>,
) -> Element {
    let request_id = use_hook(super::request_ids::new_request_id);
    let mode_label = if message.mode == "follow_up" {
        "Follow-up"
    } else {
        "Steer"
    };
    let mode_hint = if message.mode == "follow_up" {
        "Waits for the current run to finish"
    } else {
        "Joins after the current tool round"
    };
    let attachment_names = message.attachment_names.clone();
    rsx! {
        li { class: "flex min-w-0 items-center gap-2 text-muted-foreground",
            span { class: "shrink-0 text-xs font-medium", title: mode_hint, "{mode_label}" }
            div { class: "flex min-w-0 flex-1 flex-col gap-1",
                span { class: "truncate", title: message.text.clone(), "{message.text}" }
                if !attachment_names.is_empty() {
                    div { class: "flex flex-wrap gap-1",
                        for filename in attachment_names {
                            Badge {
                                variant: BadgeVariant::Outline,
                                size: BadgeSize::Sm,
                                class: Some("min-w-0 max-w-full".to_owned()),
                                span { class: "block max-w-full truncate", title: "{filename}", "{filename}" }
                            }
                        }
                    }
                }
            }
            IconButton { icon: IconType::X, label: format!("Withdraw queued submission {}", message.id),
                title: "Withdraw queued input", variant: ButtonVariant::Ghost, disabled,
                onclick: move |_| on_withdraw.call((message.id.clone(), request_id.clone())) }
        }
    }
}
