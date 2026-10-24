//! Durable queued submissions beside the composer, with explicit delivery mode.
//! The transcript owns membership; conversation-level actions outlive individual
//! rows so a consumed-input refusal remains visible after Sync removes its row.

use dioxus::prelude::*;
use libero::components::{Box, Button, Flex, Pictogram, Text};
use roost_client_core::client::rpc::calls::agent_chat::WithdrawAgentChat;
use roost_protocol::wire::agent_chat::QueuedMessage;

use crate::pump::Pump;
use libero::sx::sx;

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
    let band_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .gap("xs")
            .max_height("10rem")
            .flex_shrink("0")
            .overflow_y("auto")
            .with("overscroll-behavior-y", "contain")
            .font_size("sm")
    });
    let list_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .gap("xs")
            .margin("0")
            .padding("0")
            .with("list-style", "none")
    });
    let count = messages.len();
    if count == 0 && error().is_none() {
        return rsx! {};
    }
    let label_sx = use_hook(|| sx().font_size("xs").color("muted"));
    rsx! {
        Box { component: "section", sx: band_sx,
            "aria-label": "Queued messages", "data-testid": "agent-chat-queued",
            if count > 0 {
                Text { component: "p", sx: label_sx, "Queued · {count}" }
                Box { component: "ol", sx: list_sx,
                    for message in messages {
                        QueuedMessageRow { key: "{message.id}", message, disabled: pending().is_some(), on_withdraw: withdraw }
                    }
                }
            }
            if let Some(message) = error() {
                Flex { align: "center", gap: "sm",
                    Text { color: "error", size: "sm", role: "alert", "{message}" }
                    Button {
                        variant: "standard",
                        color: "muted",
                        aria_label: "Dismiss queue error",
                        icon: rsx! { Pictogram { icon: pictogram_icons_lucide::x::outlined } },
                        onclick: move |_| error.set(None),
                    }
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
    let row_sx = use_hook(|| {
        sx().display("flex")
            .min_width("0")
            .align_items("center")
            .gap("sm")
            .color("muted")
    });
    let mode_sx = use_hook(|| sx().flex_shrink("0").font_size("xs").font_weight("500"));
    let message_sx = use_hook(|| {
        sx().min_width("0")
            .flex("1")
            .overflow("hidden")
            .with("overscroll-behavior-y", "auto")
            .text_overflow("ellipsis")
            .white_space("nowrap")
    });
    rsx! {
        Box { component: "li", sx: row_sx,
            Text { component: "span", sx: mode_sx, title: mode_hint, "{mode_label}" }
            Text { component: "span", sx: message_sx, title: message.text.clone(), "{message.text}" }
            Button {
                variant: "standard",
                color: "muted",
                disabled,
                title: "Withdraw queued input",
                aria_label: format!("Withdraw queued submission {}", message.id),
                icon: rsx! { Pictogram { icon: pictogram_icons_lucide::x::outlined } },
                onclick: move |_| on_withdraw.call((message.id.clone(), request_id.clone())),
            }
        }
    }
}
