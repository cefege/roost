//! The pinned composer warning for request failures that remain durable in history.
//! Dismissing this surface never removes transcript items or failure records.
//! The active error clears only when the next transcript attempt clears it.

use dioxus::prelude::*;

use super::agent_alert::AgentAlert;
use crate::components::ui::{
    Button, ButtonVariant, Collapsible, CollapsibleContent, CollapsibleTrigger,
};

#[component]
pub(super) fn ComposerErrorBanner(
    error: Option<String>,
    running: bool,
    on_stop: EventHandler<()>,
    on_switch_model: EventHandler<()>,
) -> Element {
    let mut dismissed = use_signal(|| false);
    let mut details_open = use_signal(|| false);
    let mut previous_error = use_signal(|| error.clone());
    use_effect(use_reactive((&error,), move |(current_error,)| {
        if current_error != previous_error.peek().clone() {
            previous_error.set(current_error.clone());
            dismissed.set(false);
            details_open.set(false);
        }
        if current_error.is_none() {
            dismissed.set(false);
        }
    }));
    let Some(error_text) = error else {
        return rsx! {};
    };
    if dismissed() {
        return rsx! {};
    }
    let headline = error_headline(&error_text);
    let has_details = error_text.len() > 80 || error_text.lines().count() > 1;
    rsx! {
        AgentAlert { test_id: "agent-chat-pinned-error",
            Collapsible {
                open: details_open() && has_details,
                on_open_change: move |next: bool| details_open.set(next),
                div { class: "flex flex-wrap items-center gap-2",
                    strong { class: "min-w-0 flex-1 break-words font-semibold", "{headline}" }
                    if has_details {
                        CollapsibleTrigger {
                            test_id: "agent-chat-error-details",
                            controls_id: "agent-chat-pinned-error-details",
                            if details_open() { "Less" } else { "Details" }
                        }
                    }
                    Button {
                        variant: ButtonVariant::Ghost,
                        "data-testid": "agent-chat-error-dismiss",
                        onclick: move |_| dismissed.set(true),
                        "Dismiss"
                    }
                }
                CollapsibleContent {
                    pre {
                        id: "agent-chat-pinned-error-details",
                        class: "m-0 max-h-48 overflow-auto whitespace-pre-wrap break-words rounded-md bg-muted p-2 font-mono text-xs text-foreground",
                        "{error_text}"
                    }
                }
            }
            div { class: "flex flex-wrap items-center gap-2",
                if running {
                    Button {
                        variant: ButtonVariant::Destructive,
                        "data-testid": "agent-chat-error-stop",
                        onclick: move |_| on_stop.call(()),
                        "Stop"
                    }
                }
                Button {
                    variant: ButtonVariant::Outline,
                    "data-testid": "agent-chat-error-switch-model",
                    onclick: move |_| on_switch_model.call(()),
                    "Switch model"
                }
            }
        }
    }
}

fn error_headline(error: &str) -> String {
    error
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("The request failed.")
        .to_owned()
}
